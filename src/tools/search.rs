//! `search` tool: token-efficient codebase search backed by `ripgrep`.
//!
//! Two-phase pattern is the point: `output_mode="files_with_matches"`
//! (default) returns a short list of paths; the model can then re-run with
//! `output_mode="content"` and `context=10-20` to read the actual matches,
//! avoiding a separate `read` round-trip in most cases.
//!
//! If `rg` is not on PATH we fall back to an in-process walker.

use anyhow::{anyhow, Result};
use futures_util::future::FutureExt;
use ignore::WalkBuilder;
use once_cell::sync::OnceCell;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::Tool;

const DEFAULT_HEAD_CONTENT: usize = 250;
const DEFAULT_HEAD_FILES: usize = 100;
const VCS_DIRS: &[&str] = &[".git", ".svn", ".hg", ".bzr", ".jj", ".sl"];

pub fn tool() -> Tool {
    Tool {
        name: "search",
        schema: json!({
            "type": "function",
            "function": {
                "name": "search",
                "description":
"Codebase search backed by ripgrep. Use this BEFORE `read`.

Two-phase pattern:
  1) Find WHERE: leave output_mode=\"files_with_matches\" (default) - cheap path list.
  2) Read the matches: re-run with output_mode=\"content\" plus context=10-20 to see surrounding lines.
  Only drop to the `read` tool when you need a window larger than context can give.

Modes: files_with_matches (default), content (-A/-B/-C supported), count.
Filters: glob (\"*.rs\", \"*.{ts,tsx}\"), type (\"rust\", \"py\", \"js\"), -i (case-insensitive), multiline.
Pattern is a Rust regex; literal braces need escaping. `head_limit` caps output (default 250 content / 100 files; 0 = unlimited). `definitions_only` keeps only declaration lines.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "regex or literal substring" },
                        "query": { "type": "string", "description": "alias for `pattern`" },
                        "path": { "type": "string", "description": "root; default cwd" },
                        "output_mode": { "type": "string", "enum": ["files_with_matches", "content", "count"] },
                        "glob": { "type": "string", "description": "e.g. \"*.rs\" or \"*.{ts,tsx}\"" },
                        "type": { "type": "string", "description": "rg --type, e.g. \"rust\"" },
                        "-A": { "type": "integer" },
                        "-B": { "type": "integer" },
                        "-C": { "type": "integer" },
                        "context": { "type": "integer", "description": "alias for -C" },
                        "-i": { "type": "boolean" },
                        "multiline": { "type": "boolean" },
                        "head_limit": { "type": "integer" },
                        "definitions_only": { "type": "boolean" },
                        "files_only": { "type": "boolean", "description": "alias for output_mode=files_with_matches" },
                        "max_results": { "type": "integer", "description": "alias for head_limit" }
                    },
                    "required": []
                }
            }
        }),
        exec: |args| async move { run(args).await }.boxed(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    FilesWithMatches,
    Content,
    Count,
}

struct Params {
    pattern: String,
    root: PathBuf,
    mode: Mode,
    glob: Option<String>,
    type_filter: Option<String>,
    before: Option<u64>,
    after: Option<u64>,
    context: Option<u64>,
    case_insensitive: bool,
    multiline: bool,
    head_limit: Option<usize>,
    definitions_only: bool,
}

fn parse_params(args: &Value) -> Result<Params> {
    let str_arg = |k: &str| args.get(k).and_then(|v| v.as_str()).map(String::from);
    let bool_arg = |k: &str| args.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    let u64_arg = |k: &str| args.get(k).and_then(|v| v.as_u64());

    let pattern = str_arg("pattern")
        .or_else(|| str_arg("query"))
        .ok_or_else(|| anyhow!("search: missing `pattern` (or `query`)"))?;
    if pattern.is_empty() {
        return Err(anyhow!("search: empty pattern"));
    }

    let root = str_arg("path")
        .map(|s| PathBuf::from(shellexpand::tilde(&s).into_owned()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let mode = match args.get("output_mode").and_then(|v| v.as_str()) {
        Some("content") => Mode::Content,
        Some("count") => Mode::Count,
        Some("files_with_matches") | None => Mode::FilesWithMatches,
        Some(other) => return Err(anyhow!("search: invalid output_mode '{}'", other)),
    };
    // files_only is just an alias; explicit output_mode wins if given.
    let mode = if bool_arg("files_only") && args.get("output_mode").is_none() {
        Mode::FilesWithMatches
    } else {
        mode
    };

    let head_limit = u64_arg("head_limit")
        .or_else(|| u64_arg("max_results"))
        .map(|n| n as usize);

    Ok(Params {
        pattern,
        root,
        mode,
        glob: str_arg("glob"),
        type_filter: str_arg("type"),
        before: u64_arg("-B"),
        after: u64_arg("-A"),
        context: u64_arg("-C").or_else(|| u64_arg("context")),
        case_insensitive: bool_arg("-i"),
        multiline: bool_arg("multiline"),
        head_limit,
        definitions_only: bool_arg("definitions_only"),
    })
}

fn effective_head(mode: Mode, head_limit: Option<usize>) -> Option<usize> {
    match head_limit {
        Some(0) => None,
        Some(n) => Some(n),
        None => Some(match mode {
            Mode::Content => DEFAULT_HEAD_CONTENT,
            Mode::FilesWithMatches | Mode::Count => DEFAULT_HEAD_FILES,
        }),
    }
}

async fn run(args: Value) -> Result<String> {
    let p = parse_params(&args)?;
    if !rg_available() {
        return fallback_walk(&p);
    }
    let out = run_rg(&p, false)?;
    if has_no_matches(&out) && !p.case_insensitive {
        // Forgiving fallback: retry case-insensitive (helps when the model
        // misremembers identifier casing during a refactor).
        let retry = run_rg(&p, true)?;
        if has_no_matches(&retry) {
            Ok(no_match_message(&p))
        } else {
            Ok(format!("(case-insensitive fallback)\n{}", retry))
        }
    } else {
        Ok(out)
    }
}

fn rg_available() -> bool {
    static AVAIL: OnceCell<bool> = OnceCell::new();
    *AVAIL.get_or_init(|| {
        Command::new("rg")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

fn run_rg(p: &Params, force_ci: bool) -> Result<String> {
    let mut cmd = Command::new("rg");
    cmd.arg("--hidden");
    for d in VCS_DIRS {
        cmd.arg("--glob").arg(format!("!{}", d));
    }
    cmd.arg("--max-columns").arg("500");
    if p.multiline {
        cmd.arg("-U").arg("--multiline-dotall");
    }
    if p.case_insensitive || force_ci {
        cmd.arg("-i");
    }
    match p.mode {
        Mode::FilesWithMatches => {
            cmd.arg("-l");
        }
        Mode::Count => {
            cmd.arg("-c");
        }
        Mode::Content => {
            cmd.arg("-n");
            if let Some(c) = p.context {
                cmd.arg("-C").arg(c.to_string());
            } else {
                if let Some(b) = p.before {
                    cmd.arg("-B").arg(b.to_string());
                }
                if let Some(a) = p.after {
                    cmd.arg("-A").arg(a.to_string());
                }
            }
        }
    }
    // Glob: split on whitespace; commas only if no braces (preserve `*.{ts,tsx}`).
    if let Some(g) = &p.glob {
        for raw in g.split_whitespace() {
            if raw.contains('{') && raw.contains('}') {
                cmd.arg("--glob").arg(raw);
            } else {
                for piece in raw.split(',').filter(|s| !s.is_empty()) {
                    cmd.arg("--glob").arg(piece);
                }
            }
        }
    }
    if let Some(t) = &p.type_filter {
        cmd.arg("--type").arg(t);
    }
    // -e so leading-dash patterns aren't misread as flags.
    cmd.arg("-e").arg(&p.pattern).arg(&p.root);

    let output = cmd
        .output()
        .map_err(|e| anyhow!("search: failed to spawn rg: {}", e))?;
    // rg exits 1 for "no matches", 2 for an error. Don't bail on 1.
    if !output.status.success() && output.status.code() != Some(1) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("search: rg failed: {}", stderr.trim()));
    }
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let lines: Vec<&str> = stdout.lines().collect();
    if lines.is_empty() {
        return Ok(no_match_message(p));
    }
    mark_seen_from_lines(&lines, p.mode);

    // definitions_only post-filter (content mode only).
    let owned: Vec<String>;
    let view: Vec<&str> = if p.definitions_only && p.mode == Mode::Content {
        if let Some(re) = build_def_regex(&p.pattern, p.case_insensitive || force_ci) {
            owned = lines
                .iter()
                .filter(|l| {
                    split_content_line(l)
                        .map(|(_, _, t)| re.is_match(t))
                        .unwrap_or(false)
                })
                .map(|s| s.to_string())
                .collect();
            if owned.is_empty() {
                return Ok(no_match_with_filter(p, "definitions_only"));
            }
            owned.iter().map(String::as_str).collect()
        } else {
            lines
        }
    } else {
        lines
    };

    Ok(format_output(
        p,
        &view,
        effective_head(p.mode, p.head_limit),
    ))
}

fn format_output(p: &Params, lines: &[&str], head: Option<usize>) -> String {
    let total = lines.len();
    let take = head.map(|n| n.min(total)).unwrap_or(total);
    let shown = &lines[..take];
    let truncated = head.map(|n| total > n).unwrap_or(false);
    let mut out = String::new();
    match p.mode {
        Mode::FilesWithMatches => {
            out.push_str(&format!(
                "{} file(s) matched (showing {}{}):\n",
                total,
                shown.len(),
                if truncated { ", truncated" } else { "" }
            ));
        }
        Mode::Count => {
            let total_matches: u64 = lines
                .iter()
                .filter_map(|l| l.rsplit_once(':').and_then(|(_, c)| c.parse::<u64>().ok()))
                .sum();
            out.push_str(&format!(
                "{} match(es) across {} file(s) (showing {}{}):\n",
                total_matches,
                total,
                shown.len(),
                if truncated { ", truncated" } else { "" }
            ));
        }
        Mode::Content => {
            out.push_str(&format!(
                "{} line(s){}{}:\n",
                total,
                if truncated {
                    format!(", showing {}", shown.len())
                } else {
                    String::new()
                },
                if p.definitions_only {
                    " (definitions_only)"
                } else {
                    ""
                }
            ));
        }
    }
    for l in shown {
        out.push_str(l);
        out.push('\n');
    }
    out
}

fn no_match_message(p: &Params) -> String {
    format!(
        "(no matches for /{}/ in {}; try a shorter pattern, -i, or a different path)\n",
        p.pattern,
        p.root.display()
    )
}

fn no_match_with_filter(p: &Params, filter: &str) -> String {
    format!(
        "(no matches for /{}/ in {} after {} filter; try without it to see usages)\n",
        p.pattern,
        p.root.display(),
        filter
    )
}

fn has_no_matches(s: &str) -> bool {
    s.starts_with("(no matches")
}

/// Parse a content-mode rg line: "path:line:text". Returns None for context lines.
fn split_content_line(line: &str) -> Option<(&str, &str, &str)> {
    let first = line.find(':')?;
    let rest = &line[first + 1..];
    let second = rest.find(':')?;
    let lineno = &rest[..second];
    if lineno.chars().all(|c| c.is_ascii_digit()) {
        Some((&line[..first], lineno, &rest[second + 1..]))
    } else {
        None
    }
}

fn build_def_regex(pattern: &str, ci: bool) -> Option<regex::Regex> {
    let token = sanitize_for_regex(pattern);
    if token.is_empty() {
        return None;
    }
    regex::RegexBuilder::new(&format!(
        r"^\s*(pub\s+)?(async\s+)?(fn|class|interface|impl|function|def|const|let|var|struct|enum|trait|type|export\s+(default\s+)?(class|function|const|let|var))\s+(\w*\b{}\b\w*)",
        regex::escape(&token)
    ))
    .case_insensitive(ci)
    .build()
    .ok()
}

fn sanitize_for_regex(q: &str) -> String {
    // Longest identifier-ish run, e.g. "addEventListener('click'" -> "addEventListener".
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

fn mark_seen_from_lines(lines: &[&str], mode: Mode) {
    for l in lines {
        let path_str = match mode {
            Mode::FilesWithMatches => *l,
            Mode::Count => l.rsplit_once(':').map(|(p, _)| p).unwrap_or(l),
            Mode::Content => l.find(':').map(|i| &l[..i]).unwrap_or(l),
        };
        let p = Path::new(path_str);
        if p.exists() {
            crate::read_cache::mark_seen_by_search(p);
        }
    }
}

// Minimal fallback when `rg` is not on PATH.
fn fallback_walk(p: &Params) -> Result<String> {
    let re = regex::RegexBuilder::new(&p.pattern)
        .case_insensitive(p.case_insensitive)
        .multi_line(true)
        .build()
        .map_err(|e| anyhow!("search: invalid regex: {}", e))?;
    let glob_match = p
        .glob
        .as_deref()
        .map(|g| globset::Glob::new(g).map(|gg| gg.compile_matcher()))
        .transpose()?;

    let mut content_lines: Vec<String> = Vec::new();
    let mut counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for entry in WalkBuilder::new(&p.root)
        .hidden(false)
        .git_ignore(true)
        .build()
    {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        if let Some(m) = &glob_match {
            let basename = path.file_name().map(Path::new).unwrap_or(path);
            if !m.is_match(basename) {
                continue;
            }
        }
        if entry
            .metadata()
            .map(|m| m.len() > 4 * 1024 * 1024)
            .unwrap_or(false)
        {
            continue;
        }
        let Ok(f) = std::fs::File::open(path) else {
            continue;
        };
        for (i, line) in BufReader::new(f).lines().enumerate() {
            let Ok(line) = line else { break };
            if line.len() > 4000 {
                continue;
            }
            if re.is_match(&line) {
                let s = path.display().to_string();
                *counts.entry(s.clone()).or_insert(0) += 1;
                if p.mode == Mode::Content {
                    content_lines.push(format!("{}:{}:{}", s, i + 1, line));
                }
                crate::read_cache::mark_seen_by_search(path);
            }
        }
    }
    if counts.is_empty() {
        return Ok(no_match_message(p));
    }
    let owned: Vec<String> = match p.mode {
        Mode::FilesWithMatches => {
            let mut v: Vec<String> = counts.keys().cloned().collect();
            v.sort();
            v
        }
        Mode::Count => {
            let mut v: Vec<(String, u32)> = counts.into_iter().collect();
            v.sort_by(|a, b| a.0.cmp(&b.0));
            v.into_iter().map(|(p, c)| format!("{}:{}", p, c)).collect()
        }
        Mode::Content => content_lines,
    };

    // Mirror the rg path's definitions_only post-filter so behavior is
    // identical whether ripgrep is available or not (e.g., macOS CI runners
    // ship without rg). Without this, callers see a "(definitions_only)"
    // header but receive unfiltered content lines.
    let filtered: Vec<String> = if p.definitions_only && p.mode == Mode::Content {
        if let Some(re) = build_def_regex(&p.pattern, p.case_insensitive) {
            let kept: Vec<String> = owned
                .iter()
                .filter(|l| {
                    split_content_line(l)
                        .map(|(_, _, t)| re.is_match(t))
                        .unwrap_or(false)
                })
                .cloned()
                .collect();
            if kept.is_empty() {
                return Ok(no_match_with_filter(p, "definitions_only"));
            }
            kept
        } else {
            owned
        }
    } else {
        owned
    };

    let view: Vec<&str> = filtered.iter().map(String::as_str).collect();
    Ok(format_output(
        p,
        &view,
        effective_head(p.mode, p.head_limit),
    ))
}

/// Public for tests: builds the canonical fixture file used by the test suite.
#[cfg(test)]
pub fn make_widget(dir: &Path) {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn build_fixture(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mlx-search-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        make_widget(&dir);
        dir
    }

    fn rt_run(args: serde_json::Value) -> Result<String> {
        tokio::runtime::Runtime::new().unwrap().block_on(run(args))
    }

    #[test]
    fn default_returns_files_with_matches() {
        let dir = build_fixture("default");
        let out = rt_run(json!({"pattern": "make_widget", "path": dir.to_string_lossy()})).unwrap();
        assert!(out.contains("api.rs"), "missing api.rs:\n{}", out);
        assert!(out.contains("user.rs"), "missing user.rs:\n{}", out);
        assert!(
            !out.contains("pub fn make_widget"),
            "default mode should be paths only:\n{}",
            out
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn query_alias_back_compat() {
        let dir = build_fixture("queryalias");
        let out = rt_run(json!({"query": "make_widget", "path": dir.to_string_lossy()})).unwrap();
        assert!(out.contains("api.rs"), "query alias should work:\n{}", out);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn content_mode_returns_lines() {
        let dir = build_fixture("content");
        let out = rt_run(json!({
            "pattern": "make_widget", "path": dir.to_string_lossy(), "output_mode": "content"
        }))
        .unwrap();
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
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn count_mode_reports_per_file() {
        let dir = build_fixture("countmode");
        let out = rt_run(json!({
            "pattern": "make_widget", "path": dir.to_string_lossy(), "output_mode": "count"
        }))
        .unwrap();
        assert!(out.contains("api.rs:2"), "expected api.rs:2:\n{}", out);
        assert!(out.contains("user.rs:1"), "expected user.rs:1:\n{}", out);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn files_only_alias_maps_to_files_with_matches() {
        let dir = build_fixture("filesonly");
        let out = rt_run(json!({
            "pattern": "make_widget", "path": dir.to_string_lossy(), "files_only": true
        }))
        .unwrap();
        assert!(
            out.contains("api.rs") && out.contains("user.rs"),
            "missing files:\n{}",
            out
        );
        assert!(
            !out.contains("pub fn make_widget"),
            "files_only should not include content:\n{}",
            out
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn context_lines_in_content_mode() {
        let dir = build_fixture("ctx");
        let out = rt_run(json!({
            "pattern": "make_widget", "path": dir.to_string_lossy(),
            "output_mode": "content", "-C": 1
        }))
        .unwrap();
        assert!(
            out.contains("n + 1") || out.contains("println!"),
            "expected context lines:\n{}",
            out
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn definitions_only_filters_content() {
        let dir = build_fixture("defonly");
        let out = rt_run(json!({
            "pattern": "make_widget", "path": dir.to_string_lossy(),
            "output_mode": "content", "definitions_only": true
        }))
        .unwrap();
        assert!(
            out.contains("pub fn make_widget"),
            "missing def line:\n{}",
            out
        );
        assert!(
            !out.contains("let _ = make_widget"),
            "should drop usage:\n{}",
            out
        );
        assert!(
            !out.contains("println!(\"{}\", make_widget"),
            "should drop cross-file usage:\n{}",
            out
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_matches_returns_helpful_message() {
        let dir = build_fixture("nope");
        let out =
            rt_run(json!({"pattern": "zzz_no_such_token_zzz", "path": dir.to_string_lossy()}))
                .unwrap();
        assert!(
            out.starts_with("(no matches"),
            "expected no-match prefix:\n{}",
            out
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
