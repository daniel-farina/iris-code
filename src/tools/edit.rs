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

    // Strip trailing whitespace from new_string for non-Markdown files.
    // Markdown's two-trailing-spaces semantics (hard line break) means we
    // must NOT do this for .md/.mdx.
    let new_stripped;
    let new: &str = if is_markdown_path(&p) {
        new
    } else {
        new_stripped = strip_trailing_whitespace(new);
        new_stripped.as_str()
    };

    if old.is_empty() {
        // create or overwrite. If overwriting an existing CRLF file with an
        // LF-only payload, re-encode to CRLF so the file's typography is
        // preserved across the rewrite.
        let new_owned;
        let new: &str = if p.exists() {
            if let Ok(existing) = std::fs::read_to_string(&p) {
                if is_crlf(&existing) && !new.contains("\r\n") && new.contains('\n') {
                    new_owned = to_crlf(new);
                    new_owned.as_str()
                } else {
                    new
                }
            } else {
                new
            }
        } else {
            new
        };
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

    // Read-before-edit gate (off by default; opt-in via env). When the
    // file already exists AND was never read or seen by a search tool in
    // this session, refuse the edit. Empty old_string (create/overwrite)
    // is exempt above.
    if std::env::var("HIP_REQUIRE_READ_BEFORE_EDIT")
        .map(|v| v == "1")
        .unwrap_or(false)
        && p.exists()
        && crate::read_cache::last_read(&p).is_none()
    {
        return Err(anyhow!(
            "edit: {} was not read in this session; read it before editing (HIP_REQUIRE_READ_BEFORE_EDIT=1 is active)",
            p.display()
        ));
    }

    // Read-staleness gate: if the agent already read this file in this
    // session (we have a stamp with a real seen_mtime), refuse the edit
    // when the file's current mtime is later. The agent must re-read.
    if let Some(stamp) = crate::read_cache::last_read(&p) {
        if stamp.seen_mtime != 0 {
            if let Ok(meta) = std::fs::metadata(&p) {
                let cur_mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                if cur_mtime > stamp.seen_mtime {
                    return Err(anyhow!(
                        "edit: {} changed since last read at {} (now mtime {}); read it again before editing",
                        p.display(),
                        stamp.read_at,
                        cur_mtime
                    ));
                }
            }
        }
    }

    let original = match std::fs::read_to_string(&p) {
        Ok(s) => s,
        Err(e) => {
            // If the file simply doesn't exist, suggest similar names.
            if e.kind() == std::io::ErrorKind::NotFound {
                let suggestions = similar_paths(&p);
                if !suggestions.is_empty() {
                    return Err(anyhow!(
                        "edit: file not found: {} - did you mean: {}?",
                        p.display(),
                        suggestions.join(", ")
                    ));
                }
                return Err(anyhow!("edit: file not found: {}", p.display()));
            }
            return Err(anyhow!("edit: read {}: {}", p.display(), e));
        }
    };

    // Line-ending preservation: if the file uses CRLF and the model
    // supplied LF strings, transparently re-encode old/new so matching
    // works AND the splice keeps CRLF on disk.
    let file_crlf = is_crlf(&original);
    let (old_owned, new_owned);
    let (old, new): (&str, &str) = if file_crlf && !old.contains("\r\n") && old.contains('\n') {
        old_owned = to_crlf(old);
        new_owned = to_crlf(new);
        (old_owned.as_str(), new_owned.as_str())
    } else {
        (old, new)
    };

    // Try exact, then quote-normalized, then whitespace-tolerant fallback.
    // The fallback strips trailing whitespace per line on both sides — a
    // common drift cause (file uses tabs, model emitted spaces, or model
    // dropped a trailing space) — and on hit, uses the file's actual
    // matched slice as `actual_old` so the diff is computed against real bytes.
    let actual_old: String = if original.contains(old) {
        old.to_string()
    } else if let Some(slice) = find_via_quote_normalization(&original, old) {
        eprintln!(
            "\x1b[2m[edit] exact match failed; matched after curly-quote normalization\x1b[0m"
        );
        slice
    } else if let Some((slice, occurrences)) =
        find_with_whitespace_tolerance(&original, old, replace_all)
    {
        eprintln!(
            "\x1b[2m[edit] exact match failed; matched after whitespace normalization ({} occurrence{})\x1b[0m",
            occurrences,
            if occurrences == 1 { "" } else { "s" },
        );
        slice
    } else {
        let hint = diagnose_missing(&original, old);
        return Err(anyhow!(
            "edit: old_string not found in {}{}",
            p.display(),
            hint
        ));
    };

    // If we matched via quote normalization, re-apply the file's curly
    // style to new_string so the file's typography is preserved.
    let new_owned_quote;
    let new: &str = if actual_old != old {
        new_owned_quote = preserve_quote_style(old, &actual_old, new);
        new_owned_quote.as_str()
    } else {
        new
    };

    let count = original.matches(&actual_old).count();
    if count > 1 && !replace_all {
        let lines = locate_match_lines(&original, &actual_old, 5);
        return Err(anyhow!(
            "edit: old_string occurs {} times in {} - pass replace_all=true OR include more surrounding context to disambiguate. \
            First {} match line(s): {}",
            count, p.display(), lines.len(), lines.into_iter().map(|n| n.to_string()).collect::<Vec<_>>().join(", ")
        ));
    }
    let updated = if replace_all {
        original.replace(&actual_old, new)
    } else {
        original.replacen(&actual_old, new, 1)
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

/// Walk the user's CWD (gitignore-aware) and return up to 3 paths whose
/// basename is most similar to `target`'s basename. Used to enrich
/// "file not found" errors so the agent can self-correct typos like
/// `src/Index.tsx` -> `src/index.tsx`. Levenshtein scoring; cap walk
/// at a few thousand entries so we don't pay a fortune on huge trees.
pub(crate) fn similar_paths(target: &std::path::Path) -> Vec<String> {
    let target_base = match target.file_name().and_then(|n| n.to_str()) {
        Some(b) => b.to_string(),
        None => return Vec::new(),
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let mut scored: Vec<(usize, String)> = Vec::new();
    let mut walked = 0usize;
    for entry in ignore::WalkBuilder::new(&cwd)
        .hidden(false)
        .git_ignore(true)
        .build()
    {
        if walked >= 5000 {
            break;
        }
        walked += 1;
        let Ok(entry) = entry else { continue };
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        let p = entry.path();
        let Some(base) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let dist = levenshtein(&target_base.to_lowercase(), &base.to_lowercase());
        // Only suggest if the distance is small relative to the basename.
        let max_dist = target_base.len().max(base.len()).saturating_div(2).max(2);
        if dist <= max_dist {
            scored.push((dist, p.display().to_string()));
        }
    }
    scored.sort_by_key(|s| s.0);
    scored.into_iter().take(3).map(|(_, p)| p).collect()
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let m = a.len();
    let n = b.len();
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut cur: Vec<usize> = vec![0; n + 1];
    for i in 1..=m {
        cur[0] = i;
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[n]
}

/// Strip trailing whitespace from each line of `s`, preserving line
/// endings (CRLF, LF, or bare CR). The model often pads lines with
/// trailing spaces or tabs that the user does not want; stripping
/// keeps diffs clean. Caller must skip this for Markdown where two
/// trailing spaces is a hard-line-break.
pub(crate) fn strip_trailing_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut line_start = 0;
    while i < bytes.len() {
        // Look ahead for a line ending.
        if bytes[i] == b'\n' {
            let line = &s[line_start..i];
            out.push_str(line.trim_end_matches([' ', '\t']));
            out.push('\n');
            i += 1;
            line_start = i;
        } else if bytes[i] == b'\r' {
            // CRLF or bare CR.
            let line = &s[line_start..i];
            out.push_str(line.trim_end_matches([' ', '\t']));
            if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                out.push_str("\r\n");
                i += 2;
            } else {
                out.push('\r');
                i += 1;
            }
            line_start = i;
        } else {
            i += 1;
        }
    }
    // Trailing fragment with no line ending.
    if line_start < s.len() {
        let line = &s[line_start..];
        out.push_str(line.trim_end_matches([' ', '\t']));
    }
    out
}

/// True if the file's extension indicates Markdown - we must NOT strip
/// trailing whitespace for these because Markdown treats two trailing
/// spaces as a hard line break.
pub(crate) fn is_markdown_path(path: &std::path::Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => {
            let lower = ext.to_ascii_lowercase();
            lower == "md" || lower == "mdx"
        }
        None => false,
    }
}

// Curly quote constants — files often contain these (especially prose,
// docs, README). Models almost always emit straight quotes. We normalise
// both sides for matching, then re-apply the file's curly style to the
// new_string so the file's typography is preserved across the edit.
pub(crate) const LSQUO: char = '\u{2018}'; // '
pub(crate) const RSQUO: char = '\u{2019}'; // '
pub(crate) const LDQUO: char = '\u{201C}'; // "
pub(crate) const RDQUO: char = '\u{201D}'; // "

/// Replace curly quotes with straight quotes. Used to widen match scope
/// when the file has curly quotes but the model supplied straight ones.
pub(crate) fn normalize_quotes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            LSQUO | RSQUO => out.push('\''),
            LDQUO | RDQUO => out.push('"'),
            other => out.push(other),
        }
    }
    out
}

fn is_opening_context(prev: Option<char>) -> bool {
    match prev {
        None => true,
        Some(c) => matches!(
            c,
            ' ' | '\t' | '\n' | '\r' | '(' | '[' | '{' | '\u{2014}' | '\u{2013}'
        ),
    }
}

/// Replace straight `"` in `s` with curly double quotes based on context.
fn apply_curly_double(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, ch) in chars.iter().enumerate() {
        if *ch == '"' {
            let prev = if i == 0 { None } else { Some(chars[i - 1]) };
            out.push(if is_opening_context(prev) {
                LDQUO
            } else {
                RDQUO
            });
        } else {
            out.push(*ch);
        }
    }
    out
}

/// Replace straight `'` in `s` with curly single quotes based on context.
/// Apostrophes between two letters (contractions like "don't") become
/// RSQUO regardless.
fn apply_curly_single(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, ch) in chars.iter().enumerate() {
        if *ch == '\'' {
            let prev = if i == 0 { None } else { Some(chars[i - 1]) };
            let next = if i + 1 >= chars.len() {
                None
            } else {
                Some(chars[i + 1])
            };
            let prev_letter = prev.map(|c| c.is_alphabetic()).unwrap_or(false);
            let next_letter = next.map(|c| c.is_alphabetic()).unwrap_or(false);
            if prev_letter && next_letter {
                out.push(RSQUO);
            } else if is_opening_context(prev) {
                out.push(LSQUO);
            } else {
                out.push(RSQUO);
            }
        } else {
            out.push(*ch);
        }
    }
    out
}

/// If `actual_old` (the slice we matched in the file) contains curly
/// quotes but the model's `old_string` does not, re-apply the file's
/// curly-quote style to `new_string`. Returns the (possibly rewritten)
/// new_string.
pub(crate) fn preserve_quote_style(old_string: &str, actual_old: &str, new_string: &str) -> String {
    if old_string == actual_old {
        return new_string.to_string();
    }
    let has_double = actual_old.contains(LDQUO) || actual_old.contains(RDQUO);
    let has_single = actual_old.contains(LSQUO) || actual_old.contains(RSQUO);
    if !has_double && !has_single {
        return new_string.to_string();
    }
    let mut result = new_string.to_string();
    if has_double {
        result = apply_curly_double(&result);
    }
    if has_single {
        result = apply_curly_single(&result);
    }
    result
}

/// True if the buffer uses CRLF line endings (any `\r\n` and no bare `\n`
/// outside CRLF pairs). We use a simple heuristic: if there's at least
/// one `\r\n` and the count of `\r\n` matches the count of `\n`, the file
/// is CRLF-dominant.
pub(crate) fn is_crlf(s: &str) -> bool {
    let crlf = s.matches("\r\n").count();
    if crlf == 0 {
        return false;
    }
    let total_lf = s.matches('\n').count();
    crlf == total_lf
}

/// Re-encode bare LF in `s` to CRLF without touching existing CRLF.
/// Used to align an `old_string`/`new_string` from the model (which
/// almost always uses LF) with a CRLF-on-disk file.
pub(crate) fn to_crlf(s: &str) -> String {
    // First normalise CRLF -> LF, then expand all LF to CRLF.
    let normalized = s.replace("\r\n", "\n");
    normalized.replace('\n', "\r\n")
}

/// Apply a single edit to in-memory `original` content and return the
/// updated string + a count of how many replacements landed. Pure: does
/// not touch disk. Used by both the single `edit` tool and `multi_edit`
/// to share the same fallback / multi-match validation logic.
pub(crate) fn apply_edit_in_memory(
    original: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<(String, usize)> {
    if old.is_empty() {
        // Caller decides whether this is create-or-overwrite; in-memory
        // we treat an empty old_string as "replace whole content".
        return Ok((new.to_string(), 1));
    }
    // Line-ending preservation: if the file is CRLF and the model gave
    // us LF-encoded strings, re-encode to CRLF so matching works AND the
    // splice keeps CRLF on disk.
    let file_crlf = is_crlf(original);
    let (old_owned, new_owned);
    let (old, new): (&str, &str) = if file_crlf && !old.contains("\r\n") && old.contains('\n') {
        old_owned = to_crlf(old);
        new_owned = to_crlf(new);
        (old_owned.as_str(), new_owned.as_str())
    } else {
        (old, new)
    };
    let actual_old: String = if original.contains(old) {
        old.to_string()
    } else if let Some(slice) = find_via_quote_normalization(original, old) {
        slice
    } else if let Some((slice, _occurrences)) =
        find_with_whitespace_tolerance(original, old, replace_all)
    {
        slice
    } else {
        let hint = diagnose_missing(original, old);
        return Err(anyhow!("old_string not found{}", hint));
    };
    let new_owned_quote;
    let new: &str = if actual_old != old {
        new_owned_quote = preserve_quote_style(old, &actual_old, new);
        new_owned_quote.as_str()
    } else {
        new
    };
    let count = original.matches(&actual_old).count();
    if count > 1 && !replace_all {
        let lines = locate_match_lines(original, &actual_old, 5);
        return Err(anyhow!(
            "old_string occurs {} times - pass replace_all=true OR include more surrounding context to disambiguate. \
            First {} match line(s): {}",
            count, lines.len(), lines.into_iter().map(|n| n.to_string()).collect::<Vec<_>>().join(", ")
        ));
    }
    let updated = if replace_all {
        original.replace(&actual_old, new)
    } else {
        original.replacen(&actual_old, new, 1)
    };
    let n_replacements = if replace_all { count } else { 1 };
    Ok((updated, n_replacements))
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
    // First-line of needle present? Probably old_string drifted later in the file
    // OR the model has a typo on a later line. Include the file's actual block
    // starting at that line so the model can spot the divergence without a
    // re-read round-trip — this is THE common case the LLM hits with
    // multi-line edits, so giving it the real content is high-leverage.
    if let Some(first_line) = needle.lines().next() {
        if first_line.len() >= 10 && haystack.contains(first_line) {
            let needle_lines = needle.lines().count();
            // Find the line number of the first occurrence.
            let lines: Vec<&str> = haystack.lines().collect();
            if let Some(start_idx) = lines.iter().position(|l| *l == first_line) {
                // Show up to needle_lines + 2 lines from the file at that
                // location, so the model can diff its old_string against
                // reality. Cap the per-line content at 200 chars to avoid
                // dumping huge minified strings into the error.
                let end_idx = (start_idx + needle_lines).min(lines.len());
                let mut block = String::new();
                for (i, l) in lines[start_idx..end_idx].iter().enumerate() {
                    let trimmed: String = l.chars().take(200).collect();
                    block.push_str(&format!("\n  L{}: {}", start_idx + 1 + i, trimmed));
                }
                return format!(
                    " (hint: first line matches L{} but old_string diverges from actual file content. \
                    Compare your old_string against the real {}-line block:{})",
                    start_idx + 1,
                    end_idx - start_idx,
                    block,
                );
            }
            // Fallback if we can't find the line index for some reason.
            return format!(
                " (hint: first line of old_string '{}…' is present, but the rest doesn't match - file may have changed; read it again)",
                first_line.chars().take(40).collect::<String>()
            );
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

/// If exact matching failed, try matching `needle` against `haystack`
/// with curly quotes normalised to straight on both sides. On hit,
/// returns the file's actual byte slice (containing the curly quotes)
/// so callers can apply `preserve_quote_style` to new_string.
fn find_via_quote_normalization(haystack: &str, needle: &str) -> Option<String> {
    if needle.is_empty() {
        return None;
    }
    let norm_h = normalize_quotes(haystack);
    let norm_n = normalize_quotes(needle);
    if norm_n == needle && norm_h == haystack {
        // Neither side had any curly quotes — quote normalization can't
        // help, and we'd otherwise return a duplicate of the failing
        // exact match.
        return None;
    }
    let idx = norm_h.find(&norm_n)?;
    // Map char index in the normalized string back to a byte slice in
    // the original. Since normalize_quotes is a 1-to-1 char mapping
    // (every curly char becomes exactly one ASCII char), char-index
    // alignment is preserved.
    let start_chars = norm_h[..idx].chars().count();
    let end_chars = start_chars + norm_n.chars().count();
    let mut start_byte = None;
    let mut end_byte = None;
    for (i, (b, _)) in haystack.char_indices().enumerate() {
        if i == start_chars {
            start_byte = Some(b);
        }
        if i == end_chars {
            end_byte = Some(b);
            break;
        }
    }
    let start = start_byte?;
    let end = end_byte.unwrap_or(haystack.len());
    Some(haystack[start..end].to_string())
}

/// Whitespace-tolerant fallback. If the exact `needle` doesn't appear in
/// `haystack`, slide a window of `needle.lines().count()` lines through
/// the haystack and compare line-by-line with both sides trim-end normalized.
/// On hit, return the file's actual matched slice (real bytes, real
/// whitespace) so the caller can apply the edit with the correct anchor.
///
/// Returns `(actual_slice, occurrence_count)`. With `replace_all=false`,
/// stops at the first match (occurrence_count == 1). With `replace_all=true`,
/// requires every fuzzy-matched window to produce the *same* real slice so
/// the replace operation is unambiguous.
fn find_with_whitespace_tolerance(
    haystack: &str,
    needle: &str,
    replace_all: bool,
) -> Option<(String, usize)> {
    let needle_lines: Vec<&str> = needle.lines().collect();
    if needle_lines.is_empty() {
        return None;
    }
    let needle_norm: Vec<&str> = needle_lines.iter().map(|l| l.trim_end()).collect();
    let needle_trim_lead = needle_norm.iter().any(|l| l.trim_start() != *l);
    let normalize = |s: &str| -> String {
        if needle_trim_lead {
            s.trim().to_string()
        } else {
            s.trim_end().to_string()
        }
    };
    let needle_keys: Vec<String> = needle_lines.iter().map(|l| normalize(l)).collect();

    let hay_lines: Vec<&str> = haystack.lines().collect();
    if hay_lines.len() < needle_lines.len() {
        return None;
    }

    // For each window, capture the byte offset in haystack so we can extract
    // the exact slice (including the original whitespace).
    let mut byte_offsets: Vec<usize> = Vec::with_capacity(hay_lines.len() + 1);
    let mut acc = 0usize;
    for l in &hay_lines {
        byte_offsets.push(acc);
        acc += l.len() + 1; // +1 for \n
    }
    byte_offsets.push(haystack.len());

    let mut found: Option<String> = None;
    let mut count = 0usize;
    for start in 0..=hay_lines.len() - needle_lines.len() {
        let window = &hay_lines[start..start + needle_lines.len()];
        let matches = window
            .iter()
            .zip(needle_keys.iter())
            .all(|(h, n)| normalize(h) == *n);
        if !matches {
            continue;
        }
        // Build the actual slice from the file (real whitespace).
        let begin = byte_offsets[start];
        // End at end-of-last-window-line (no trailing newline beyond the slice).
        let last_line_end = byte_offsets[start + needle_lines.len()].saturating_sub(1);
        let end = last_line_end.max(begin);
        let slice = haystack.get(begin..end)?.to_string();

        // Sanity check: the extracted slice must reappear in the haystack
        // (it should — we just sliced from it). If not, bail to avoid
        // corrupting the file with an indeterminate replacement.
        if !haystack.contains(&slice) {
            return None;
        }

        match &found {
            None => {
                found = Some(slice);
                count = 1;
            }
            Some(prev) if prev == &slice => count += 1,
            Some(_) if !replace_all => {
                // Conflicting slices found. Don't try to guess.
                return None;
            }
            Some(_) => return None,
        }
        if !replace_all && count >= 1 {
            break;
        }
    }
    found.map(|s| (s, count))
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
    fn whitespace_tolerant_match_recovers_from_indent_drift() {
        // Acquire env lock — sibling tests mutate MLX_CODE_DRY_RUN under it,
        // which would otherwise leak into this test and silently skip the write.
        let _guard = crate::dry_run_log::ENV_LOCK.lock().unwrap();
        std::env::remove_var("MLX_CODE_DRY_RUN");
        // File uses 4-space indent; agent supplied 2-space.
        let p = std::env::temp_dir().join(format!("mlx-edit-ws-{}.txt", std::process::id()));
        std::fs::write(&p, "fn main() {\n    let x = 1;\n    let y = 2;\n}\n").unwrap();
        let out = rt_run(json!({
            "path": p.to_string_lossy(),
            "old_string": "  let x = 1;\n  let y = 2;",
            "new_string": "    let x = 10;\n    let y = 20;",
        }))
        .unwrap();
        assert!(out.contains("edited"), "expected success:\n{}", out);
        let body = std::fs::read_to_string(&p).unwrap();
        assert!(
            body.contains("    let x = 10;\n    let y = 20;"),
            "expected new content with file's real indent:\n{}",
            body
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn first_line_match_hint_includes_real_file_block() {
        // Multi-line needle whose first line matches but later lines have a typo.
        // The hint should embed the actual file content so the model can
        // diff and self-correct without a re-read round-trip.
        let h =
            "void update() {\n    if (testX.x < 0) return;\n    if (testX.z > 100) return;\n    move();\n}\n";
        let n = "void update() {\n    if (testX.x < 0) return;\n    if (testZ.z > 100) return;\n";
        let hint = diagnose_missing(h, n);
        assert!(
            hint.contains("first line matches L1"),
            "expected line anchor:\n{}",
            hint
        );
        assert!(
            hint.contains("L2: ") && hint.contains("testX.x < 0"),
            "hint should include actual L2 content:\n{}",
            hint
        );
        assert!(
            hint.contains("L3: ") && hint.contains("testX.z > 100"),
            "hint should include actual L3 content (the divergent line):\n{}",
            hint
        );
    }

    #[test]
    fn levenshtein_basic_cases() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("abc", "abc"), 0);
        assert_eq!(levenshtein("abc", "abd"), 1);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("Index", "index"), 1);
    }

    #[test]
    fn similar_paths_suggests_typo_match_in_cwd() {
        let _guard = crate::dry_run_log::ENV_LOCK.lock().unwrap();
        std::env::remove_var("MLX_CODE_DRY_RUN");
        crate::read_cache::clear_reads();
        // Build a small temp tree, cd into it, and probe.
        let root = std::env::temp_dir().join(format!("mlx-edit-sim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/index.tsx"), "// real\n").unwrap();
        std::fs::write(root.join("src/index.ts"), "// real\n").unwrap();
        std::fs::write(root.join("README.md"), "x\n").unwrap();

        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(&root).unwrap();
        // Non-existent path with similar basename.
        let target = std::path::PathBuf::from("src/Index.tsx");
        let suggestions = similar_paths(&target);
        std::env::set_current_dir(&prev).unwrap();

        assert!(!suggestions.is_empty(), "expected at least one suggestion");
        assert!(
            suggestions.iter().any(|s| s.contains("index.tsx")),
            "expected index.tsx in suggestions: {:?}",
            suggestions
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_before_edit_gate_blocks_unread_files_when_env_set() {
        let _guard = crate::dry_run_log::ENV_LOCK.lock().unwrap();
        std::env::remove_var("MLX_CODE_DRY_RUN");
        crate::read_cache::clear_reads();
        let p = std::env::temp_dir().join(format!("mlx-edit-gate-{}.txt", std::process::id()));
        std::fs::write(&p, "alpha\n").unwrap();

        std::env::set_var("HIP_REQUIRE_READ_BEFORE_EDIT", "1");
        let res = rt_run(json!({
            "path": p.to_string_lossy(),
            "old_string": "alpha",
            "new_string": "beta",
        }));
        std::env::remove_var("HIP_REQUIRE_READ_BEFORE_EDIT");
        assert!(res.is_err(), "expected gate failure: {:?}", res);
        let msg = format!("{}", res.unwrap_err());
        assert!(msg.contains("was not read"), "unexpected error: {}", msg);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "alpha\n");
        crate::read_cache::clear_reads();
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_before_edit_gate_off_by_default() {
        let _guard = crate::dry_run_log::ENV_LOCK.lock().unwrap();
        std::env::remove_var("MLX_CODE_DRY_RUN");
        std::env::remove_var("HIP_REQUIRE_READ_BEFORE_EDIT");
        crate::read_cache::clear_reads();
        let p = std::env::temp_dir().join(format!("mlx-edit-gate-off-{}.txt", std::process::id()));
        std::fs::write(&p, "alpha\n").unwrap();

        // No gate -> edit should proceed.
        let out = rt_run(json!({
            "path": p.to_string_lossy(),
            "old_string": "alpha",
            "new_string": "beta",
        }))
        .unwrap();
        assert!(out.contains("edited"));
        crate::read_cache::clear_reads();
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_before_edit_gate_allows_when_seen_by_search() {
        let _guard = crate::dry_run_log::ENV_LOCK.lock().unwrap();
        std::env::remove_var("MLX_CODE_DRY_RUN");
        crate::read_cache::clear_reads();
        let p = std::env::temp_dir().join(format!("mlx-edit-gate-seen-{}.txt", std::process::id()));
        std::fs::write(&p, "alpha\n").unwrap();
        // Simulate a glob/grep hit.
        crate::read_cache::mark_seen_by_search(&p);

        std::env::set_var("HIP_REQUIRE_READ_BEFORE_EDIT", "1");
        let out = rt_run(json!({
            "path": p.to_string_lossy(),
            "old_string": "alpha",
            "new_string": "beta",
        }))
        .unwrap();
        std::env::remove_var("HIP_REQUIRE_READ_BEFORE_EDIT");
        assert!(out.contains("edited"));
        crate::read_cache::clear_reads();
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_before_edit_gate_allows_create_of_new_file() {
        let _guard = crate::dry_run_log::ENV_LOCK.lock().unwrap();
        std::env::remove_var("MLX_CODE_DRY_RUN");
        crate::read_cache::clear_reads();
        let p = std::env::temp_dir().join(format!("mlx-edit-gate-new-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&p);

        std::env::set_var("HIP_REQUIRE_READ_BEFORE_EDIT", "1");
        // Empty old_string + non-existent file -> gate should not fire.
        let out = rt_run(json!({
            "path": p.to_string_lossy(),
            "old_string": "",
            "new_string": "hello\n",
        }))
        .unwrap();
        std::env::remove_var("HIP_REQUIRE_READ_BEFORE_EDIT");
        assert!(out.contains("wrote"));
        crate::read_cache::clear_reads();
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn strip_trailing_whitespace_removes_per_line_spaces_and_tabs() {
        let input = "alpha   \nbeta\t\t\ngamma  \r\nzeta\r\nepsilon \t";
        let out = strip_trailing_whitespace(input);
        assert_eq!(out, "alpha\nbeta\ngamma\r\nzeta\r\nepsilon");
    }

    #[test]
    fn is_markdown_path_recognises_md_and_mdx() {
        assert!(is_markdown_path(std::path::Path::new("notes.md")));
        assert!(is_markdown_path(std::path::Path::new("doc.MD")));
        assert!(is_markdown_path(std::path::Path::new("page.mdx")));
        assert!(!is_markdown_path(std::path::Path::new("a.rs")));
        assert!(!is_markdown_path(std::path::Path::new("a")));
    }

    #[test]
    fn edit_strips_trailing_whitespace_for_non_markdown() {
        let _guard = crate::dry_run_log::ENV_LOCK.lock().unwrap();
        std::env::remove_var("MLX_CODE_DRY_RUN");
        crate::read_cache::clear_reads();
        let p = std::env::temp_dir().join(format!("mlx-edit-strip-{}.rs", std::process::id()));
        std::fs::write(&p, "let x = 1;\n").unwrap();
        let _ = rt_run(json!({
            "path": p.to_string_lossy(),
            "old_string": "let x = 1;",
            // Trailing spaces in new_string should be stripped.
            "new_string": "let x = 42;   ",
        }))
        .unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(
            body, "let x = 42;\n",
            "trailing spaces should be stripped: {:?}",
            body
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn edit_preserves_trailing_whitespace_for_markdown() {
        let _guard = crate::dry_run_log::ENV_LOCK.lock().unwrap();
        std::env::remove_var("MLX_CODE_DRY_RUN");
        crate::read_cache::clear_reads();
        let p = std::env::temp_dir().join(format!("mlx-edit-md-{}.md", std::process::id()));
        std::fs::write(&p, "first line\nsecond line\n").unwrap();
        // Two-trailing-spaces is a Markdown hard line break.
        let _ = rt_run(json!({
            "path": p.to_string_lossy(),
            "old_string": "first line",
            "new_string": "first line  ",
        }))
        .unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        assert!(
            body.starts_with("first line  \n"),
            "markdown trailing spaces lost: {:?}",
            body
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn normalize_quotes_replaces_curly_with_straight() {
        let s = "\u{201C}hello\u{201D} and \u{2018}world\u{2019}";
        assert_eq!(normalize_quotes(s), "\"hello\" and 'world'");
    }

    #[test]
    fn preserve_quote_style_round_trips_double_quotes() {
        let old = "\"hello\"";
        let actual_old = "\u{201C}hello\u{201D}";
        let new = "\"hi\"";
        assert_eq!(
            preserve_quote_style(old, actual_old, new),
            "\u{201C}hi\u{201D}"
        );
    }

    #[test]
    fn preserve_quote_style_keeps_apostrophe_in_contraction() {
        // File has a curly apostrophe inside a contraction. Edit should
        // re-emit a curly apostrophe between two letters.
        let old = "don't go";
        let actual_old = "don\u{2019}t go";
        let new = "don't stop";
        assert_eq!(
            preserve_quote_style(old, actual_old, new),
            "don\u{2019}t stop"
        );
    }

    #[test]
    fn edit_matches_through_curly_quotes_and_preserves_them() {
        let _guard = crate::dry_run_log::ENV_LOCK.lock().unwrap();
        std::env::remove_var("MLX_CODE_DRY_RUN");
        crate::read_cache::clear_reads();
        let p = std::env::temp_dir().join(format!("mlx-edit-curly-{}.txt", std::process::id()));
        // File uses curly quotes in prose.
        std::fs::write(&p, "She said \u{201C}hello\u{201D} loudly.\n").unwrap();
        // Model supplies straight quotes.
        let out = rt_run(json!({
            "path": p.to_string_lossy(),
            "old_string": "She said \"hello\" loudly.",
            "new_string": "She whispered \"goodbye\" softly.",
        }))
        .unwrap();
        assert!(out.contains("edited"), "expected success: {}", out);
        let body = std::fs::read_to_string(&p).unwrap();
        // File should end up with curly quotes preserved around "goodbye".
        assert!(
            body.contains("\u{201C}goodbye\u{201D}"),
            "expected curly quotes preserved: {:?}",
            body
        );
        // No straight quote should have leaked in for that span.
        assert!(
            !body.contains("\"goodbye\""),
            "straight quote leaked: {:?}",
            body
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn edit_preserves_curly_apostrophe_in_contraction() {
        let _guard = crate::dry_run_log::ENV_LOCK.lock().unwrap();
        std::env::remove_var("MLX_CODE_DRY_RUN");
        crate::read_cache::clear_reads();
        let p = std::env::temp_dir().join(format!("mlx-edit-apos-{}.txt", std::process::id()));
        // File has "don't" with curly apostrophe.
        std::fs::write(&p, "We don\u{2019}t support that.\n").unwrap();
        let out = rt_run(json!({
            "path": p.to_string_lossy(),
            "old_string": "We don't support that.",
            "new_string": "We don't allow that.",
        }))
        .unwrap();
        assert!(out.contains("edited"), "expected success: {}", out);
        let body = std::fs::read_to_string(&p).unwrap();
        assert!(
            body.contains("don\u{2019}t allow"),
            "curly apostrophe lost: {:?}",
            body
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn is_crlf_detects_crlf_dominant_files() {
        assert!(is_crlf("a\r\nb\r\n"));
        assert!(!is_crlf("a\nb\n"));
        assert!(!is_crlf("a\r\nb\n"), "mixed should not be classified CRLF");
        assert!(!is_crlf(""));
    }

    #[test]
    fn to_crlf_round_trips_lf_only_input() {
        assert_eq!(to_crlf("a\nb\n"), "a\r\nb\r\n");
        // Already-CRLF input should be unchanged.
        assert_eq!(to_crlf("a\r\nb\r\n"), "a\r\nb\r\n");
    }

    #[test]
    fn edit_preserves_crlf_line_endings_when_model_supplies_lf() {
        let _guard = crate::dry_run_log::ENV_LOCK.lock().unwrap();
        std::env::remove_var("MLX_CODE_DRY_RUN");
        crate::read_cache::clear_reads();
        let p = std::env::temp_dir().join(format!("mlx-edit-crlf-{}.txt", std::process::id()));
        // CRLF on disk.
        std::fs::write(&p, "fn main() {\r\n    let x = 1;\r\n}\r\n").unwrap();

        // Model supplies LF strings (typical).
        let out = rt_run(json!({
            "path": p.to_string_lossy(),
            "old_string": "    let x = 1;",
            "new_string": "    let x = 42;",
        }))
        .unwrap();
        assert!(out.contains("edited"), "expected success: {}", out);
        let body = std::fs::read_to_string(&p).unwrap();
        assert!(
            body.contains("\r\n"),
            "file should still be CRLF: {:?}",
            body
        );
        assert!(body.contains("let x = 42;"));
        // No bare LF should have leaked in.
        let bare_lf = body.matches('\n').count();
        let crlf = body.matches("\r\n").count();
        assert_eq!(bare_lf, crlf, "no bare LF allowed: {:?}", body);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn edit_preserves_crlf_on_overwrite() {
        let _guard = crate::dry_run_log::ENV_LOCK.lock().unwrap();
        std::env::remove_var("MLX_CODE_DRY_RUN");
        crate::read_cache::clear_reads();
        let p = std::env::temp_dir().join(format!("mlx-edit-crlf-ow-{}.txt", std::process::id()));
        std::fs::write(&p, "alpha\r\nbeta\r\n").unwrap();
        // Overwrite with empty old_string; new_string is LF-only.
        let _ = rt_run(json!({
            "path": p.to_string_lossy(),
            "old_string": "",
            "new_string": "one\ntwo\nthree\n",
        }))
        .unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        assert!(
            body.contains("\r\n"),
            "overwrite should preserve CRLF: {:?}",
            body
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn staleness_check_blocks_edit_when_file_modified_since_read() {
        let _guard = crate::dry_run_log::ENV_LOCK.lock().unwrap();
        std::env::remove_var("MLX_CODE_DRY_RUN");
        let p = std::env::temp_dir().join(format!("mlx-edit-stale-{}.txt", std::process::id()));
        std::fs::write(&p, "alpha\n").unwrap();
        // Stamp a "read" at a mtime in the distant past so any current mtime is newer.
        crate::read_cache::mark_read(&p, 1);

        let res = rt_run(json!({
            "path": p.to_string_lossy(),
            "old_string": "alpha",
            "new_string": "beta",
        }));
        assert!(res.is_err(), "expected staleness error, got: {:?}", res);
        let msg = format!("{}", res.unwrap_err());
        assert!(
            msg.contains("changed since last read"),
            "expected staleness wording, got: {}",
            msg
        );
        // File should be untouched.
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "alpha\n");
        crate::read_cache::clear_reads();
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn staleness_check_allows_edit_when_no_prior_read() {
        let _guard = crate::dry_run_log::ENV_LOCK.lock().unwrap();
        std::env::remove_var("MLX_CODE_DRY_RUN");
        crate::read_cache::clear_reads();
        let p = std::env::temp_dir().join(format!("mlx-edit-noread-{}.txt", std::process::id()));
        std::fs::write(&p, "alpha\n").unwrap();
        // No prior read stamp -> staleness check should not fire.
        let out = rt_run(json!({
            "path": p.to_string_lossy(),
            "old_string": "alpha",
            "new_string": "beta",
        }))
        .unwrap();
        assert!(out.contains("edited"), "expected success: {}", out);
        crate::read_cache::clear_reads();
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
