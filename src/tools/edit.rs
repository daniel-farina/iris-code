//! `edit` tool: safe text replacement / file create.
//!
//! Behavior matches what most coding agents expect:
//! - If `old_string` is empty, treat this as a write-new-file or full-overwrite.
//! - Otherwise, the file must contain `old_string` exactly. With `replace_all=false`,
//!   the match must be unique.

use anyhow::{anyhow, Context, Result};
use futures_util::future::FutureExt;
use serde_json::{json, Value};
use std::path::PathBuf;

use super::Tool;

pub fn tool() -> Tool {
    Tool {
        name: "edit",
        schema: json!({
            "type": "function",
            "function": {
                "name": "edit",
                "description": "Replace `old_string` with `new_string` in `path`. Empty old_string creates/overwrites. Ambiguous match -> error unless replace_all=true. dry_run=true previews as diff without writing.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "old_string": { "type": "string", "description": "exact text to replace; empty for create/overwrite" },
                        "new_string": { "type": "string", "description": "replacement text or full file contents" },
                        "replace_all": { "type": "boolean", "default": false },
                        "dry_run": { "type": "boolean", "description": "validate + return diff preview only; no write. default false" }
                    },
                    "required": ["path", "old_string", "new_string"]
                }
            }
        }),
        exec: |args| async move { run(args).await }.boxed(),
    }
}

async fn run(args: Value) -> Result<String> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("edit: missing path"))?
        .to_string();
    let old = args
        .get("old_string")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let new = args
        .get("new_string")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let replace_all = args
        .get("replace_all")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    // Either the per-call dry_run param OR the agent-loop-wide MLX_CODE_DRY_RUN env var
    // sets dry-run mode. The env-var path lets `--dry-run` at the CLI level cascade
    // through every edit call without the model having to know about it.
    let dry_run = args
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || std::env::var("MLX_CODE_DRY_RUN")
            .map(|v| v == "1")
            .unwrap_or(false);

    let p = PathBuf::from(shellexpand::tilde(&path).into_owned());

    if old.is_empty() {
        // create or overwrite
        if dry_run {
            let exists = p.exists();
            let kind: &'static str = if exists { "overwrite" } else { "create" };
            crate::dry_run_log::record_with_bytes(kind, p.display().to_string(), new.len() as u64);
            return Ok(format!(
                "(dry_run) would {} {} ({} bytes)\n",
                kind,
                p.display(),
                new.len(),
            ));
        }
        if let Some(parent) = p.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("mkdir -p {}", parent.display()))?;
            }
        }
        std::fs::write(&p, new).with_context(|| format!("write {}", p.display()))?;
        crate::read_cache::invalidate(&p);
        return Ok(format!("wrote {} ({} bytes)\n", p.display(), new.len()));
    }

    let original = std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
    if !original.contains(old) {
        // Helpful diagnostics: hint at near-misses (whitespace? line endings? case?)
        let hint = diagnose_missing(&original, old);
        return Err(anyhow!(
            "edit: old_string not found in {}{}",
            p.display(),
            hint
        ));
    }
    let count = original.matches(old).count();
    if count > 1 && !replace_all {
        // List line numbers of all occurrences so the model can include more
        // context to disambiguate next try.
        let lines = locate_match_lines(&original, old, 5);
        return Err(anyhow!(
            "edit: old_string occurs {} times in {} - pass replace_all=true OR include more surrounding context to disambiguate. \
            First {} match line(s): {}",
            count, p.display(), lines.len(), lines.into_iter().map(|n| n.to_string()).collect::<Vec<_>>().join(", ")
        ));
    }
    let updated = if replace_all {
        original.replace(old, new)
    } else {
        original.replacen(old, new, 1)
    };
    let lines_added = new.bytes().filter(|&b| b == b'\n').count() as i64;
    let lines_removed = old.bytes().filter(|&b| b == b'\n').count() as i64;
    let net = lines_added - lines_removed;
    let net_disp = if net > 0 {
        format!("+{}", net)
    } else {
        net.to_string()
    };
    let n_replacements = if replace_all { count } else { 1 };
    let plural = if replace_all && count != 1 { "s" } else { "" };

    if dry_run {
        // Record post-replacement byte count - this is what would land on disk.
        crate::dry_run_log::record_with_bytes(
            "replace",
            p.display().to_string(),
            updated.len() as u64,
        );
        let preview = preview_diff(&original, &updated);
        return Ok(format!(
            "(dry_run) would edit {} ({} replacement{}, net {} line(s)){}",
            p.display(),
            n_replacements,
            plural,
            net_disp,
            preview,
        ));
    }
    std::fs::write(&p, &updated).with_context(|| format!("write {}", p.display()))?;
    crate::read_cache::invalidate(&p);
    Ok(format!(
        "edited {} ({} replacement{}, net {} line(s))\n",
        p.display(),
        n_replacements,
        plural,
        net_disp,
    ))
}

/// Compact diff preview for `dry_run`: shows up to 8 lines of removed+added
/// from the prefix/suffix-trimmed difference, line-numbered to where the
/// change occurs in the original file.
fn preview_diff(before: &str, after: &str) -> String {
    let b: Vec<&str> = before.lines().collect();
    let a: Vec<&str> = after.lines().collect();
    let mut prefix = 0usize;
    while prefix < b.len() && prefix < a.len() && b[prefix] == a[prefix] {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < b.len().saturating_sub(prefix)
        && suffix < a.len().saturating_sub(prefix)
        && b[b.len() - 1 - suffix] == a[a.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let removed = &b[prefix..b.len() - suffix];
    let added = &a[prefix..a.len() - suffix];
    if removed.is_empty() && added.is_empty() {
        return String::from("\n  (no line-level changes)\n");
    }
    let mut out = String::from("\n");
    let cap_each = 4usize;
    out.push_str(&format!(
        "  @L{} (-{} +{})\n",
        prefix + 1,
        removed.len(),
        added.len()
    ));
    for (i, line) in removed.iter().take(cap_each).enumerate() {
        let trimmed: String = line.chars().take(110).collect();
        out.push_str(&format!("  - {:>4}  {}\n", prefix + 1 + i, trimmed));
    }
    if removed.len() > cap_each {
        out.push_str(&format!(
            "    ... +{} more removed\n",
            removed.len() - cap_each
        ));
    }
    for (i, line) in added.iter().take(cap_each).enumerate() {
        let trimmed: String = line.chars().take(110).collect();
        out.push_str(&format!("  + {:>4}  {}\n", prefix + 1 + i, trimmed));
    }
    if added.len() > cap_each {
        out.push_str(&format!("    ... +{} more added\n", added.len() - cap_each));
    }
    out
}

/// Best-effort hint when an `old_string` lookup misses. Detects common
/// causes (CRLF, trailing whitespace, case difference) so the model knows
/// what to fix without a follow-up read round-trip.
fn diagnose_missing(haystack: &str, needle: &str) -> String {
    if needle.is_empty() {
        return String::new();
    }
    // Try CRLF normalised - only fire when at least one side actually has
    // CRLF, otherwise this hint mis-fires for whitespace-only differences.
    if haystack.contains("\r\n") || needle.contains("\r\n") {
        let normal_h = haystack.replace("\r\n", "\n");
        let normal_n = needle.replace("\r\n", "\n");
        if normal_h.contains(&normal_n) {
            return " (hint: file has CRLF line endings - supply old_string with \\r\\n line endings or read the file again)".into();
        }
    }
    // Try whitespace-trimmed match on each line of the haystack vs needle.
    let needle_trim = needle.trim();
    if !needle_trim.is_empty() && haystack.contains(needle_trim) {
        return " (hint: leading/trailing whitespace differs — match found if you trim the old_string)".into();
    }
    // Case-insensitive single-line.
    if needle.lines().count() == 1 {
        let n_low = needle.to_lowercase();
        if haystack.lines().any(|l| l.to_lowercase().contains(&n_low)) {
            return " (hint: case differs — match found case-insensitively)".into();
        }
    }
    // First-line of needle present? Probably old_string drifted later in the file.
    if let Some(first_line) = needle.lines().next() {
        if first_line.len() >= 10 && haystack.contains(first_line) {
            return format!(" (hint: first line of old_string '{}…' is present, but the rest doesn't match - file may have changed; read it again)", first_line.chars().take(40).collect::<String>());
        }
    }
    // Final fallback: trigram-based fuzzy match.
    // For SINGLE-line needles: find the line with highest 3-gram overlap.
    // For MULTI-line needles: slide a window of needle.lines().count() lines
    // through the file and score each window's trigram overlap, pointing the
    // agent at the closest matching block.
    let needle_line_count = needle.lines().count();
    if needle_line_count == 1 && needle.len() <= 200 {
        if let Some((best_line_no, best_text, score)) = closest_line_by_trigram(haystack, needle) {
            let needle_grams = trigram_count(needle);
            if needle_grams > 0 && score * 100 / needle_grams >= 40 {
                let preview: String = best_text.chars().take(60).collect();
                return format!(
                    " (hint: closest line in file is L{}: '{}'; check for whitespace/typo)",
                    best_line_no, preview
                );
            }
        }
    } else if needle_line_count >= 2 && needle.len() <= 4000 {
        if let Some((start_line, end_line, score)) = closest_block_by_trigram(haystack, needle) {
            let needle_grams = trigram_count(needle);
            if needle_grams > 0 && score * 100 / needle_grams >= 40 {
                // Show the first line of the matched block as a preview anchor.
                let preview_line = haystack
                    .lines()
                    .nth(start_line - 1)
                    .unwrap_or("")
                    .chars()
                    .take(60)
                    .collect::<String>();
                return format!(" (hint: closest {}-line block is L{}-L{} starting '{}'; re-read that range and retry)",
                    needle_line_count, start_line, end_line, preview_line);
            }
        }
    }
    String::new()
}

/// Count distinct 3-character shingles in s.
fn trigram_count(s: &str) -> usize {
    if s.chars().count() < 3 {
        return 0;
    }
    let chars: Vec<char> = s.chars().collect();
    let mut set = std::collections::HashSet::new();
    for w in chars.windows(3) {
        set.insert((w[0], w[1], w[2]));
    }
    set.len()
}

/// For multi-line needles: slide a needle.lines().count()-sized window
/// through `haystack`, count distinct trigram overlap per window, return
/// (start_line, end_line, overlap_count) of the best match.
/// Window indices are 1-based and INCLUSIVE on both sides.
fn closest_block_by_trigram(haystack: &str, needle: &str) -> Option<(usize, usize, usize)> {
    let needle_chars: Vec<char> = needle.chars().collect();
    if needle_chars.len() < 3 {
        return None;
    }
    let mut needle_grams: std::collections::HashSet<(char, char, char)> =
        std::collections::HashSet::new();
    for w in needle_chars.windows(3) {
        needle_grams.insert((w[0], w[1], w[2]));
    }
    let lines: Vec<&str> = haystack.lines().collect();
    let k = needle.lines().count();
    if lines.len() < k {
        return None;
    }

    let mut best: Option<(usize, usize, usize)> = None;
    for start in 0..=lines.len().saturating_sub(k) {
        // Concatenate window with newline separator; cheap clone for short windows.
        let mut buf = String::with_capacity(needle.len() + 16);
        for (i, l) in lines[start..start + k].iter().enumerate() {
            if i > 0 {
                buf.push('\n');
            }
            buf.push_str(l);
        }
        let buf_chars: Vec<char> = buf.chars().collect();
        if buf_chars.len() < 3 {
            continue;
        }
        let mut overlap = 0usize;
        let mut seen: std::collections::HashSet<(char, char, char)> =
            std::collections::HashSet::new();
        for w in buf_chars.windows(3) {
            let g = (w[0], w[1], w[2]);
            if needle_grams.contains(&g) && seen.insert(g) {
                overlap += 1;
            }
        }
        if overlap > 0 && best.as_ref().map(|b| overlap > b.2).unwrap_or(true) {
            best = Some((start + 1, start + k, overlap));
        }
    }
    best
}

/// For each line in `haystack`, count overlap of trigrams against `needle`.
/// Returns (line_number, line_text, overlap_count) of the best match.
fn closest_line_by_trigram(haystack: &str, needle: &str) -> Option<(usize, String, usize)> {
    if needle.chars().count() < 3 {
        return None;
    }
    let needle_chars: Vec<char> = needle.chars().collect();
    let mut needle_grams: std::collections::HashSet<(char, char, char)> =
        std::collections::HashSet::new();
    for w in needle_chars.windows(3) {
        needle_grams.insert((w[0], w[1], w[2]));
    }
    let mut best: Option<(usize, String, usize)> = None;
    for (i, line) in haystack.lines().enumerate() {
        if line.chars().count() < 3 {
            continue;
        }
        let line_chars: Vec<char> = line.chars().collect();
        let mut overlap = 0usize;
        let mut seen: std::collections::HashSet<(char, char, char)> =
            std::collections::HashSet::new();
        for w in line_chars.windows(3) {
            let g = (w[0], w[1], w[2]);
            if needle_grams.contains(&g) && seen.insert(g) {
                overlap += 1;
            }
        }
        if overlap > 0 && best.as_ref().map(|b| overlap > b.2).unwrap_or(true) {
            best = Some((i + 1, line.to_string(), overlap));
        }
    }
    best
}

fn locate_match_lines(text: &str, needle: &str, cap: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut byte_pos = 0usize;
    while let Some(rel) = text[byte_pos..].find(needle) {
        let abs = byte_pos + rel;
        // Translate byte position to 1-indexed line number.
        let line = text[..abs].bytes().filter(|&b| b == b'\n').count() + 1;
        out.push(line);
        if out.len() >= cap {
            break;
        }
        byte_pos = abs + needle.len();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnose_crlf_difference() {
        let h = "let x = 1;\r\nlet y = 2;\r\n";
        let n = "let x = 1;\nlet y = 2;\n";
        let hint = diagnose_missing(h, n);
        assert!(hint.contains("CRLF"), "expected CRLF hint, got: {}", hint);
    }

    #[test]
    fn diagnose_whitespace_difference() {
        let h = "  let x = 1;\n";
        let n = "let x = 1;";
        let hint = diagnose_missing(h, n);
        // Should fire whitespace-trim hint.
        assert!(
            hint.contains("whitespace"),
            "expected whitespace hint, got: {}",
            hint
        );
    }

    #[test]
    fn diagnose_case_difference() {
        let h = "let x = 1;\nlet name = String::new();\n";
        let n = "LET name = String::new();";
        let hint = diagnose_missing(h, n);
        assert!(hint.contains("case"), "expected case hint, got: {}", hint);
    }

    #[test]
    fn diagnose_fuzzy_typo_suggests_closest_line() {
        // File has `let count = 0;`, agent asked for `let counter = 0;`
        // (typo). None of the prior hints fire (no CRLF, no trim, no case),
        // so the fuzzy fallback should kick in.
        let h = "fn main() {\n    let count = 0;\n    println!(\"hi\");\n}\n";
        let n = "let counter = 0;";
        let hint = diagnose_missing(h, n);
        assert!(
            hint.contains("closest line in file is L"),
            "expected fuzzy hint, got: {}",
            hint
        );
        assert!(
            hint.contains("let count = 0"),
            "should reference the actual line, got: {}",
            hint
        );
    }

    #[test]
    fn diagnose_no_hint_when_unrelated() {
        let h = "totally\nunrelated\ntext\nhere\n";
        let n = "fn very_specific_function() { return 42; }";
        let hint = diagnose_missing(h, n);
        // Should NOT incorrectly suggest a random line.
        assert!(
            hint.is_empty(),
            "expected empty hint for unrelated needle, got: {}",
            hint
        );
    }

    #[test]
    fn diagnose_multiline_typo_suggests_block() {
        // Haystack has a 3-line block; needle is the same block with one
        // character changed inside.
        let h = "fn main() {\n    let count = 0;\n    println!(\"hi\");\n    count + 1\n}\n";
        let n = "    let counter = 0;\n    println!(\"hi\");\n    counter + 1";
        let hint = diagnose_missing(h, n);
        assert!(
            hint.contains("closest 3-line block"),
            "expected multi-line hint, got: {}",
            hint
        );
        assert!(
            hint.contains("L2-L4"),
            "expected L2-L4 range, got: {}",
            hint
        );
    }

    #[test]
    fn diagnose_no_multiline_hint_when_unrelated() {
        let h = "alpha\nbeta\ngamma\ndelta\nepsilon\nzeta\n";
        let n = "fn very_specific(a, b) {\n    return a + b;\n}";
        let hint = diagnose_missing(h, n);
        // Should NOT incorrectly suggest a random 3-line window.
        assert!(
            hint.is_empty(),
            "expected empty hint for unrelated multi-line needle, got: {}",
            hint
        );
    }

    #[test]
    fn closest_block_picks_aligned_window() {
        let h = "// header\nfn one() {}\nfn two() {}\nfn three() {}\n// footer\n";
        let n = "fn one() {}\nfn two() {}";
        let res = closest_block_by_trigram(h, n).unwrap();
        // Expect (start_line=2, end_line=3) - the actual block of those two fns.
        assert_eq!(res.0, 2);
        assert_eq!(res.1, 3);
    }

    #[test]
    fn closest_line_picks_highest_overlap() {
        let h = "alpha bravo\nfoo bar baz\nfoo bat baz\n";
        let needle = "foo bar baz";
        let res = closest_line_by_trigram(h, needle).unwrap();
        // Should prefer line 2 (exact match) over line 3 (one-char diff).
        assert_eq!(res.0, 2);
    }

    fn rt_run(args: serde_json::Value) -> Result<String> {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(run(args))
    }

    #[test]
    fn env_var_dry_run_overrides_arg_default() {
        let _guard = crate::dry_run_log::ENV_LOCK.lock().unwrap();
        let p = std::env::temp_dir().join(format!("mlx-edit-envdry-{}.txt", std::process::id()));
        std::fs::write(&p, "before\n").unwrap();

        std::env::set_var("MLX_CODE_DRY_RUN", "1");
        // No `dry_run` arg passed - default is false. The env var should still trigger preview mode.
        let out = rt_run(serde_json::json!({
            "path": p.to_string_lossy(),
            "old_string": "before",
            "new_string": "after"
        }))
        .unwrap();
        std::env::remove_var("MLX_CODE_DRY_RUN");

        assert!(
            out.starts_with("(dry_run)"),
            "expected dry_run prefix from env var:\n{}",
            out
        );
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "before\n",
            "file was modified"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn dry_run_does_not_write_file_but_returns_preview() {
        let p = std::env::temp_dir().join(format!("mlx-edit-dry-{}.txt", std::process::id()));
        let body = "fn main() {\n    println!(\"hi\");\n}\n";
        std::fs::write(&p, body).unwrap();
        let original_mtime = std::fs::metadata(&p).unwrap().modified().unwrap();

        // Wait a moment so we'd notice if the file got rewritten.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let out = rt_run(serde_json::json!({
            "path": p.to_string_lossy(),
            "old_string": "println!(\"hi\");",
            "new_string": "println!(\"hello\");",
            "dry_run": true
        }))
        .unwrap();

        // Output should announce dry_run, count, and show diff preview lines.
        assert!(
            out.starts_with("(dry_run)"),
            "expected dry_run prefix:\n{}",
            out
        );
        assert!(
            out.contains("would edit"),
            "expected 'would edit' phrase:\n{}",
            out
        );
        assert!(out.contains("@L"), "expected diff anchor line:\n{}", out);
        assert!(out.contains("- "), "expected removed-line marker:\n{}", out);
        assert!(out.contains("+ "), "expected added-line marker:\n{}", out);

        // Crucially: the file should NOT have been modified.
        let now_body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(now_body, body, "file was modified during dry_run");
        let now_mtime = std::fs::metadata(&p).unwrap().modified().unwrap();
        assert_eq!(now_mtime, original_mtime, "mtime changed during dry_run");

        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn dry_run_still_validates_missing_old_string() {
        let p = std::env::temp_dir().join(format!("mlx-edit-dry-miss-{}.txt", std::process::id()));
        std::fs::write(&p, "totally unrelated content\n").unwrap();
        let res = rt_run(serde_json::json!({
            "path": p.to_string_lossy(),
            "old_string": "let x = 42;",
            "new_string": "let x = 0;",
            "dry_run": true
        }));
        assert!(
            res.is_err(),
            "dry_run should still surface missing-old_string errors"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn dry_run_for_create_announces_create_or_overwrite() {
        let p = std::env::temp_dir().join(format!("mlx-edit-dry-new-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&p);

        // empty old_string + dry_run + new path -> should say "would create"
        let out = rt_run(serde_json::json!({
            "path": p.to_string_lossy(),
            "old_string": "",
            "new_string": "hello\n",
            "dry_run": true
        }))
        .unwrap();
        assert!(
            out.contains("would create"),
            "expected 'would create':\n{}",
            out
        );
        // File must NOT have been created.
        assert!(!p.exists(), "dry_run should not create the file");

        // Actually create it.
        std::fs::write(&p, "old\n").unwrap();
        // Now dry_run should say "would overwrite"
        let out = rt_run(serde_json::json!({
            "path": p.to_string_lossy(),
            "old_string": "",
            "new_string": "new\n",
            "dry_run": true
        }))
        .unwrap();
        assert!(
            out.contains("would overwrite"),
            "expected 'would overwrite':\n{}",
            out
        );
        // File still has old content.
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "old\n");

        let _ = std::fs::remove_file(&p);
    }
}
