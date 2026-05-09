//! `search` tool (Phase 3): codebase-aware search with smart query expansion.
//!
//! Strategy:
//! 1. Try the full query as a regex against file contents.
//! 2. Tokenize the query (identifier-like words) and run each token as a
//!    literal substring search in parallel.
//! 3. Merge, dedupe, rank by token-overlap with the query, cap at max_results.

use anyhow::{anyhow, Result};
use futures_util::future::FutureExt;
use ignore::WalkBuilder;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use super::Tool;

pub fn tool() -> Tool {
    Tool {
        name: "search",
        schema: json!({
            "type": "function",
            "function": {
                "name": "search",
                "description": "Codebase search with query expansion. Ranked file:line:text hits. definitions_only=true -> declarations only; files_only=true -> file paths only.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                        "max_results": { "type": "integer", "description": "default 10" },
                        "path": { "type": "string", "description": "root dir; default cwd" },
                        "definitions_only": { "type": "boolean", "description": "filter to declaration lines (fn/class/def/const/etc); default false" },
                        "files_only": { "type": "boolean", "description": "return ranked file paths only, no line-level hits; default false" }
                    },
                    "required": ["query"]
                }
            }
        }),
        exec: |args| async move { run(args).await }.boxed(),
    }
}

async fn run(args: Value) -> Result<String> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("search: missing query"))?
        .to_string();
    let max = args
        .get("max_results")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(10)
        .max(1);
    let root = args
        .get("path")
        .and_then(|v| v.as_str())
        .map(|s| PathBuf::from(shellexpand::tilde(s).into_owned()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let definitions_only = args
        .get("definitions_only")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let files_only = args
        .get("files_only")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // First pass: case-sensitive. If empty, retry once case-insensitive
    // (helps when the model misremembers identifier casing in a refactor).
    let primary = scan(&query, &root, false)?;
    let (mut hits, files_scanned, case_sensitive) = if primary.0.is_empty() {
        let fallback = scan(&query, &root, true)?;
        if !fallback.0.is_empty() {
            (fallback.0, fallback.1, false)
        } else {
            (primary.0, primary.1, true)
        }
    } else {
        (primary.0, primary.1, true)
    };

    // `definitions_only` filter: a line is a "definition" when its weight
    // includes the +8 def-boost (see scan()). We don't store the boost as
    // a separate field, but the scan threshold for verbatim+def is at least
    // 4 (verbatim) + 8 (def) + path_bonus. The cleanest detector: re-test
    // each line against the same def_regex used during the scan.
    if definitions_only {
        let def_query = sanitize_for_regex(&query);
        if !def_query.is_empty() {
            let def_re = regex::RegexBuilder::new(&format!(
                r"^\s*(pub\s+)?(async\s+)?(fn|class|interface|impl|function|def|const|let|var|struct|enum|trait|type|export\s+(default\s+)?(class|function|const|let|var))\s+(\w*\b{}\b\w*)",
                regex::escape(&def_query)
            ))
            .case_insensitive(!case_sensitive)
            .build()
            .ok();
            if let Some(re) = def_re {
                for (_path, lines) in hits.iter_mut() {
                    lines.retain(|(_, text, _)| re.is_match(text));
                }
                hits.retain(|_, lines| !lines.is_empty());
            }
        }
    }

    if hits.is_empty() {
        let suffix = if definitions_only {
            " (definitions_only filter on; try without it to see usages)"
        } else {
            ""
        };
        return Ok(format!(
            "(no matches for '{}' under {} - scanned {} file(s){}; try a shorter token, regex, or set path explicitly)\n",
            query, root.display(), files_scanned, suffix,
        ));
    }

    // Cap matches per file so a single noisy file (e.g. minified bundle that
    // slipped past the size filter) can't drown the result list.
    const PER_FILE_CAP: usize = 5;

    // File-level aggregate score: sum line weights + count bonus.
    let mut file_scores: Vec<(String, u32, usize)> = hits
        .iter()
        .map(|(p, lines)| {
            let total: u32 = lines.iter().map(|(_, _, w)| *w).sum();
            (p.clone(), total + lines.len() as u32, lines.len())
        })
        .collect();
    file_scores.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    // Flatten — file rank, then by line within file, capped per file.
    let mut flat: Vec<(String, usize, String, u32)> = Vec::new();
    let mut total_hits = 0usize;
    for (path, _file_score, _) in &file_scores {
        if let Some(lines) = hits.get(path) {
            total_hits += lines.len();
            let mut sorted = lines.clone();
            sorted.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
            for (line, text, w) in sorted.into_iter().take(PER_FILE_CAP) {
                flat.push((path.clone(), line, text, w));
            }
        }
    }
    flat.truncate(max);

    let mut out = String::new();
    let case_note = if case_sensitive {
        ""
    } else {
        " (case-insensitive fallback)"
    };
    let def_note = if definitions_only {
        " (definitions_only)"
    } else {
        ""
    };

    if files_only {
        // Compact mode: just ranked file paths with hit counts, capped at `max`.
        out.push_str(&format!(
            "found matches in {} file(s) (showing {}, scanned {} files){}{} (files_only):\n",
            file_scores.len(),
            file_scores.len().min(max),
            files_scanned,
            case_note,
            def_note,
        ));
        for (path, score, hits_in_file) in file_scores.into_iter().take(max) {
            out.push_str(&format!(
                "[score={}] {} ({} hit{})\n",
                score,
                path,
                hits_in_file,
                if hits_in_file == 1 { "" } else { "s" }
            ));
        }
        return Ok(out);
    }

    out.push_str(&format!(
        "found {} match(es) across {} file(s) (showing {}, scanned {} files){}{}:\n",
        total_hits,
        file_scores.len(),
        flat.len(),
        files_scanned,
        case_note,
        def_note,
    ));
    for (path, line, text, w) in flat {
        out.push_str(&format!(
            "[w={}] {}:{}:{}\n",
            w,
            path,
            line,
            text.trim_end()
        ));
    }
    Ok(out)
}

/// One match: (line_number, line_text, weight).
type Hit = (usize, String, u32);
/// Map of path -> hits in that path.
type HitsByFile = HashMap<String, Vec<Hit>>;

/// Single-pass scan over the file tree. Returns (hits-by-file, files_scanned).
fn scan(query: &str, root: &std::path::Path, case_insensitive: bool) -> Result<(HitsByFile, u32)> {
    // Each matcher carries (label, matcher, lowercased-literal-or-empty).
    let mut matchers: Vec<(String, Matcher, String)> = Vec::new();
    let regex_q = if case_insensitive {
        regex::RegexBuilder::new(query)
            .case_insensitive(true)
            .build()
    } else {
        regex::Regex::new(query)
    };
    let v_label = format!("verbatim:{}", query);
    let v_matcher = match regex_q {
        Ok(r) => Matcher::Regex(r),
        Err(_) => Matcher::Literal(query.to_string()),
    };
    let v_lower = match &v_matcher {
        Matcher::Literal(l) => l.to_lowercase(),
        _ => String::new(),
    };
    matchers.push((v_label, v_matcher, v_lower));
    for tok in tokenize(query) {
        if tok.len() < 3 {
            continue;
        }
        let lo = tok.to_lowercase();
        matchers.push((format!("token:{}", tok), Matcher::Literal(tok), lo));
    }

    // Definition pattern — boost lines that look like a declaration of a
    // symbol matching the query token. Helps the agent jump to the source
    // of a name, not just usages.
    let def_query = sanitize_for_regex(query);
    let def_regex = if !def_query.is_empty() {
        regex::RegexBuilder::new(&format!(
            r"^\s*(pub\s+)?(async\s+)?(fn|class|interface|impl|function|def|const|let|var|struct|enum|trait|type|export\s+(default\s+)?(class|function|const|let|var))\s+(\w*\b{}\b\w*)",
            regex::escape(&def_query)
        ))
        .case_insensitive(case_insensitive)
        .build()
        .ok()
    } else {
        None
    };

    let mut hits: HitsByFile = HashMap::new();
    let mut files_scanned = 0u32;

    for entry in WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .build()
    {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        let p = entry.path();
        if let Some(s) = p.to_str() {
            if s.contains("/target/")
                || s.contains("/node_modules/")
                || s.contains("/dist/")
                || s.contains("/.next/")
                || s.contains("/.cache/")
                || s.contains("/build/")
            {
                continue;
            }
        }
        if let Ok(meta) = entry.metadata() {
            if meta.len() > 4 * 1024 * 1024 {
                continue;
            }
        }
        // Skip files that look binary (NUL byte in first 8KB).
        if is_likely_binary(p) {
            continue;
        }
        files_scanned += 1;
        let f = match std::fs::File::open(p) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let reader = BufReader::new(f);
        let path_weight = file_kind_bonus(p);
        for (i, line) in reader.lines().enumerate() {
            let Ok(line) = line else { break };
            if line.len() > 4000 {
                continue;
            }
            let lower_line = if case_insensitive {
                line.to_lowercase()
            } else {
                String::new() // unused in case-sensitive mode
            };
            let mut weight = 0u32;
            for (label, m, lower_lit) in &matchers {
                if m.is_match_with(&line, &lower_line, case_insensitive, lower_lit) {
                    weight += if label.starts_with("verbatim:") { 4 } else { 1 };
                }
            }
            if weight > 0 {
                // Definition bonus: matches `fn foo(...)`, `class Foo`, etc.
                if let Some(re) = &def_regex {
                    if re.is_match(&line) {
                        weight += 8;
                    }
                }
                hits.entry(p.display().to_string()).or_default().push((
                    i + 1,
                    line,
                    weight + path_weight,
                ));
            }
        }
    }

    Ok((hits, files_scanned))
}

fn is_likely_binary(p: &std::path::Path) -> bool {
    let mut buf = [0u8; 8192];
    use std::io::Read;
    if let Ok(mut f) = std::fs::File::open(p) {
        if let Ok(n) = f.read(&mut buf) {
            return buf[..n].contains(&0);
        }
    }
    false
}

fn sanitize_for_regex(q: &str) -> String {
    // Pull out the longest identifier-ish token from a query so the
    // definition regex has something to anchor on. e.g. given
    // "addEventListener('click'" we want "addEventListener".
    let mut best = String::new();
    let mut cur = String::new();
    for ch in q.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            cur.push(ch);
        } else {
            if cur.len() > best.len() {
                best = cur.clone();
            }
            cur.clear();
        }
    }
    if cur.len() > best.len() {
        best = cur;
    }
    best
}

/// Source-file extensions get a small bonus so code matches outrank docs.
fn file_kind_bonus(p: &std::path::Path) -> u32 {
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
    matches!(
        ext,
        "rs" | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "py"
            | "go"
            | "java"
            | "kt"
            | "c"
            | "h"
            | "cpp"
            | "cc"
            | "hpp"
            | "rb"
            | "swift"
            | "lua"
            | "zig"
    ) as u32
        * 2
}

fn tokenize(q: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in q.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            cur.push(ch);
        } else {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

enum Matcher {
    Regex(regex::Regex),
    Literal(String),
}
impl Matcher {
    /// `original` is the line as-read. When `case_insensitive` is true, we
    /// match via the precomputed `lower_probe` (line lowercased once) against
    /// the lowercased literal, which tolerates case differences cheaply. When
    /// false, we do an exact-case substring search on the original.
    fn is_match_with(
        &self,
        original: &str,
        lower_probe: &str,
        case_insensitive: bool,
        lower_literal: &str,
    ) -> bool {
        match self {
            Matcher::Regex(r) => r.is_match(original),
            Matcher::Literal(l) => {
                if case_insensitive {
                    lower_probe.contains(lower_literal)
                } else {
                    original.contains(l)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn build_fixture(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mlx-search-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Fixture: two .rs files, one defining the symbol and using it,
        // another only calling it.
        std::fs::write(
            dir.join("api.rs"),
            "\
pub fn make_widget(n: u32) -> u32 {
    n + 1
}

fn other() {
    let _ = make_widget(5);
}
",
        )
        .unwrap();
        std::fs::write(
            dir.join("user.rs"),
            "\
fn caller() {
    println!(\"{}\", make_widget(10));
}
",
        )
        .unwrap();
        dir
    }

    fn rt_run(args: serde_json::Value) -> Result<String> {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(run(args))
    }

    #[test]
    fn search_default_returns_all_hits() {
        let dir = build_fixture("default");
        let out = rt_run(json!({
            "query": "make_widget",
            "path": dir.to_string_lossy(),
            "max_results": 50
        }))
        .unwrap();
        // Should see at least: 1 definition + 2 calls = 3 lines.
        assert!(
            out.contains("pub fn make_widget"),
            "missing def line:\n{}",
            out
        );
        assert!(
            out.contains("let _ = make_widget"),
            "missing intra-file usage:\n{}",
            out
        );
        assert!(
            out.contains("println!(\"{}\", make_widget"),
            "missing cross-file usage:\n{}",
            out
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_definitions_only_filters_to_def_lines() {
        let dir = build_fixture("defonly");
        let out = rt_run(json!({
            "query": "make_widget",
            "path": dir.to_string_lossy(),
            "max_results": 50,
            "definitions_only": true
        }))
        .unwrap();
        // Should keep ONLY the `pub fn make_widget` line.
        assert!(
            out.contains("pub fn make_widget"),
            "missing def line:\n{}",
            out
        );
        assert!(
            !out.contains("let _ = make_widget"),
            "should drop intra-file usage:\n{}",
            out
        );
        assert!(
            !out.contains("println!(\"{}\", make_widget"),
            "should drop cross-file usage:\n{}",
            out
        );
        assert!(
            out.contains("definitions_only"),
            "header should annotate filter mode:\n{}",
            out
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_files_only_returns_paths_no_lines() {
        let dir = build_fixture("filesonly");
        let out = rt_run(json!({
            "query": "make_widget",
            "path": dir.to_string_lossy(),
            "max_results": 50,
            "files_only": true
        }))
        .unwrap();
        // Header should annotate the mode
        assert!(
            out.contains("files_only"),
            "header should annotate mode:\n{}",
            out
        );
        // Both files should appear (ranked) with hit counts
        assert!(out.contains("api.rs"), "missing api.rs:\n{}", out);
        assert!(out.contains("user.rs"), "missing user.rs:\n{}", out);
        assert!(out.contains("score="), "expected per-file score:\n{}", out);
        // Crucially: NO line-level entries (no `:N:` line numbers, no `[w=...]` weights)
        assert!(
            !out.contains("[w="),
            "files_only should not emit line weights:\n{}",
            out
        );
        assert!(
            !out.contains("pub fn make_widget"),
            "files_only should not emit content:\n{}",
            out
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_files_only_compact_output_under_4_lines_per_file() {
        let dir = build_fixture("compact");
        // Default mode would produce 3 line-level hits + header.
        // files_only should produce 2 file-level hits + header = 3 lines total.
        let out = rt_run(json!({
            "query": "make_widget",
            "path": dir.to_string_lossy(),
            "files_only": true
        }))
        .unwrap();
        let n_lines = out.lines().count();
        // 1 header + 2 file rows = 3
        assert!(
            n_lines <= 4,
            "expected <=4 lines for files_only output, got {}:\n{}",
            n_lines,
            out
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_definitions_only_empty_when_only_usages() {
        let dir =
            std::env::temp_dir().join(format!("mlx-search-defonly-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Only usages, no def.
        std::fs::write(
            dir.join("a.rs"),
            "fn caller() { let x = some_helper(); x + 1 }\n",
        )
        .unwrap();
        let out = rt_run(json!({
            "query": "some_helper",
            "path": dir.to_string_lossy(),
            "definitions_only": true
        }))
        .unwrap();
        // Should report no matches and mention the filter.
        assert!(
            out.starts_with("(no matches"),
            "expected no-match prefix:\n{}",
            out
        );
        assert!(
            out.contains("definitions_only filter on"),
            "expected filter hint:\n{}",
            out
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
