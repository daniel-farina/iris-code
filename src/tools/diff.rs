//! `diff` tool: compare two text files line-by-line and return a unified-style
//! patch the agent can read directly. Handy after an `edit` or `bash` run when
//! the agent wants to confirm exactly what changed without re-`read`ing both
//! files in full.

use anyhow::{anyhow, Context, Result};
use futures_util::future::FutureExt;
use serde_json::{json, Value};
use std::path::PathBuf;

use super::Tool;

const MAX_BYTES: u64 = 1_048_576; // 1 MiB per side
const DEFAULT_CONTEXT: usize = 3;
const MAX_OUTPUT_LINES: usize = 400;

pub fn tool() -> Tool {
    Tool {
        name: "diff",
        schema: json!({
            "type": "function",
            "function": {
                "name": "diff",
                "description": "Unified-style line diff between two text files. Emits `@@ hunks @@` and `-/+` lines.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path_a": { "type": "string", "description": "left side (before/reference)" },
                        "path_b": { "type": "string", "description": "right side (after/candidate)" },
                        "context": { "type": "integer", "description": "lines of context around each hunk (default 3)" }
                    },
                    "required": ["path_a", "path_b"]
                }
            }
        }),
        exec: |args| async move { run(args).await }.boxed(),
    }
}

async fn run(args: Value) -> Result<String> {
    let path_a = args
        .get("path_a")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("diff: missing path_a"))?
        .to_string();
    let path_b = args
        .get("path_b")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("diff: missing path_b"))?
        .to_string();
    let ctx = args
        .get("context")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(DEFAULT_CONTEXT);

    let a_lines = load_lines(&path_a)?;
    let b_lines = load_lines(&path_b)?;

    if a_lines == b_lines {
        return Ok(format!(
            "(identical: {} and {} both {} lines)\n",
            path_a,
            path_b,
            a_lines.len()
        ));
    }

    let ops = lcs_diff(&a_lines, &b_lines);
    let ranges = group_into_hunks(&ops, ctx);
    let mut out = String::new();
    out.push_str(&format!("--- {}\n", path_a));
    out.push_str(&format!("+++ {}\n", path_b));
    let mut emitted = 0usize;
    let mut total_added = 0usize;
    let mut total_removed = 0usize;
    for r in &ranges {
        let mut a_start = 0usize;
        let mut a_count = 0usize;
        let mut b_start = 0usize;
        let mut b_count = 0usize;
        let mut anchored = false;
        // First pass: compute hunk header counts.
        for k in r.from..r.to {
            match &ops[k] {
                Op::Eq(ai, bi) => {
                    if !anchored {
                        a_start = *ai;
                        b_start = *bi;
                        anchored = true;
                    }
                    a_count += 1;
                    b_count += 1;
                }
                Op::Del(ai) => {
                    if !anchored {
                        a_start = *ai;
                        anchored = true;
                    }
                    a_count += 1;
                }
                Op::Ins(bi) => {
                    if !anchored {
                        b_start = *bi;
                        anchored = true;
                    }
                    b_count += 1;
                }
            }
        }
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            a_start + 1,
            a_count,
            b_start + 1,
            b_count
        ));
        // Second pass: emit lines.
        for k in r.from..r.to {
            if emitted >= MAX_OUTPUT_LINES {
                out.push_str("... (output truncated)\n");
                return Ok(out);
            }
            match &ops[k] {
                Op::Eq(ai, _bi) => {
                    out.push(' ');
                    out.push_str(&a_lines[*ai]);
                    out.push('\n');
                }
                Op::Del(ai) => {
                    out.push('-');
                    out.push_str(&a_lines[*ai]);
                    out.push('\n');
                    total_removed += 1;
                }
                Op::Ins(bi) => {
                    out.push('+');
                    out.push_str(&b_lines[*bi]);
                    out.push('\n');
                    total_added += 1;
                }
            }
            emitted += 1;
        }
    }
    if ranges.is_empty() {
        out.push_str(
            "(files differ but no hunk changes detected - check trailing newline or whitespace)\n",
        );
    } else {
        out.push_str(&format!(
            "\n(summary: -{} +{} across {} hunk(s))\n",
            total_removed,
            total_added,
            ranges.len()
        ));
    }
    Ok(out)
}

fn load_lines(path: &str) -> Result<Vec<String>> {
    let p = PathBuf::from(shellexpand::tilde(path).into_owned());
    let meta = std::fs::metadata(&p).with_context(|| format!("stat {}", p.display()))?;
    if !meta.is_file() {
        return Err(anyhow!("diff: not a regular file: {}", p.display()));
    }
    if meta.len() > MAX_BYTES {
        return Err(anyhow!(
            "diff: {} is {} bytes (>1MB), refusing",
            p.display(),
            meta.len()
        ));
    }
    let s = std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
    Ok(s.split('\n').map(|s| s.to_string()).collect())
}

#[derive(Debug, Clone)]
enum Op {
    Eq(usize, usize), // (a_idx, b_idx)
    Del(usize),       // (a_idx)
    Ins(usize),       // (b_idx)
}

/// Standard LCS-based diff. O(n*m) memory for the DP table, fine for files
/// under our 1MB cap (~64K lines worst case is ~32GB; in practice files are
/// far smaller and bounded by MAX_BYTES).
fn lcs_diff(a: &[String], b: &[String]) -> Vec<Op> {
    let n = a.len();
    let m = b.len();
    // Trim common prefix/suffix to shrink the DP table.
    let mut prefix = 0usize;
    while prefix < n && prefix < m && a[prefix] == b[prefix] {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < n - prefix && suffix < m - prefix && a[n - 1 - suffix] == b[m - 1 - suffix] {
        suffix += 1;
    }
    let n2 = n - prefix - suffix;
    let m2 = m - prefix - suffix;

    let mut ops: Vec<Op> = (0..prefix).map(|i| Op::Eq(i, i)).collect();

    if n2 > 0 || m2 > 0 {
        // DP for the middle slice only.
        let stride = m2 + 1;
        let mut dp = vec![0u32; (n2 + 1) * stride];
        for i in 1..=n2 {
            for j in 1..=m2 {
                if a[prefix + i - 1] == b[prefix + j - 1] {
                    dp[i * stride + j] = dp[(i - 1) * stride + j - 1] + 1;
                } else {
                    let up = dp[(i - 1) * stride + j];
                    let lf = dp[i * stride + j - 1];
                    dp[i * stride + j] = if up >= lf { up } else { lf };
                }
            }
        }
        // Backtrack.
        let mut i = n2;
        let mut j = m2;
        let mut middle: Vec<Op> = Vec::new();
        while i > 0 && j > 0 {
            if a[prefix + i - 1] == b[prefix + j - 1] {
                middle.push(Op::Eq(prefix + i - 1, prefix + j - 1));
                i -= 1;
                j -= 1;
            } else if dp[(i - 1) * stride + j] >= dp[i * stride + j - 1] {
                middle.push(Op::Del(prefix + i - 1));
                i -= 1;
            } else {
                middle.push(Op::Ins(prefix + j - 1));
                j -= 1;
            }
        }
        while i > 0 {
            middle.push(Op::Del(prefix + i - 1));
            i -= 1;
        }
        while j > 0 {
            middle.push(Op::Ins(prefix + j - 1));
            j -= 1;
        }
        middle.reverse();
        ops.extend(middle);
    }

    for k in 0..suffix {
        ops.push(Op::Eq(n - suffix + k, m - suffix + k));
    }
    ops
}

#[derive(Debug)]
struct HunkRange {
    from: usize, // inclusive index into ops
    to: usize,   // exclusive index into ops
}

/// Group ops into hunks separated by stretches of equality larger than
/// `2*context`. Each hunk gets `context` lines of leading/trailing context.
fn group_into_hunks(ops: &[Op], context: usize) -> Vec<HunkRange> {
    if ops.is_empty() {
        return Vec::new();
    }

    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter_map(|(i, o)| matches!(o, Op::Del(_) | Op::Ins(_)).then_some(i))
        .collect();
    if changed.is_empty() {
        return Vec::new();
    }

    let mut hunks: Vec<HunkRange> = Vec::new();
    let mut i = 0usize;
    while i < changed.len() {
        let start_change = changed[i];
        let mut end_change = start_change;
        let mut j = i + 1;
        while j < changed.len() && changed[j] - end_change <= 2 * context + 1 {
            end_change = changed[j];
            j += 1;
        }
        let from = start_change.saturating_sub(context);
        let to = (end_change + context + 1).min(ops.len());
        hunks.push(HunkRange { from, to });
        i = j;
    }
    hunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn lcs_identical_returns_only_eq() {
        let a = v(&["a", "b", "c"]);
        let b = v(&["a", "b", "c"]);
        let ops = lcs_diff(&a, &b);
        assert!(ops.iter().all(|o| matches!(o, Op::Eq(_, _))));
    }

    #[test]
    fn lcs_pure_insertion() {
        let a = v(&["a", "c"]);
        let b = v(&["a", "b", "c"]);
        let ops = lcs_diff(&a, &b);
        let ins = ops.iter().filter(|o| matches!(o, Op::Ins(_))).count();
        let del = ops.iter().filter(|o| matches!(o, Op::Del(_))).count();
        assert_eq!(ins, 1);
        assert_eq!(del, 0);
    }

    #[test]
    fn lcs_pure_deletion() {
        let a = v(&["a", "b", "c"]);
        let b = v(&["a", "c"]);
        let ops = lcs_diff(&a, &b);
        let ins = ops.iter().filter(|o| matches!(o, Op::Ins(_))).count();
        let del = ops.iter().filter(|o| matches!(o, Op::Del(_))).count();
        assert_eq!(ins, 0);
        assert_eq!(del, 1);
    }

    #[test]
    fn lcs_replacement_has_one_del_one_ins() {
        let a = v(&["a", "b", "c"]);
        let b = v(&["a", "B", "c"]);
        let ops = lcs_diff(&a, &b);
        let ins = ops.iter().filter(|o| matches!(o, Op::Ins(_))).count();
        let del = ops.iter().filter(|o| matches!(o, Op::Del(_))).count();
        assert_eq!(ins, 1);
        assert_eq!(del, 1);
    }

    #[test]
    fn unified_diff_end_to_end_tmpfiles() {
        let dir = std::env::temp_dir();
        let pid = std::process::id();
        let pa = dir.join(format!("mlx-diff-test-a-{}.txt", pid));
        let pb = dir.join(format!("mlx-diff-test-b-{}.txt", pid));
        std::fs::write(&pa, "alpha\nbravo\ncharlie\ndelta\necho\n").unwrap();
        std::fs::write(&pb, "alpha\nBRAVO\ncharlie\ndelta\necho\nfoxtrot\n").unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let args = json!({
            "path_a": pa.to_string_lossy(),
            "path_b": pb.to_string_lossy(),
            "context": 1
        });
        let out = rt.block_on(run(args)).unwrap();
        let _ = std::fs::remove_file(&pa);
        let _ = std::fs::remove_file(&pb);

        // Expected: bravo replaced and foxtrot added. Both deltas should appear.
        assert!(out.contains("-bravo"), "missing -bravo line:\n{}", out);
        assert!(out.contains("+BRAVO"), "missing +BRAVO line:\n{}", out);
        assert!(out.contains("+foxtrot"), "missing +foxtrot line:\n{}", out);
        assert!(
            out.starts_with("--- "),
            "missing unified diff header:\n{}",
            out
        );
        assert!(out.contains("@@ -"), "missing hunk header:\n{}", out);
        assert!(out.contains("summary: -1 +2"), "wrong summary:\n{}", out);
    }

    #[test]
    fn unified_diff_identical_files() {
        let dir = std::env::temp_dir();
        let pid = std::process::id();
        let pa = dir.join(format!("mlx-diff-id-a-{}.txt", pid));
        let pb = dir.join(format!("mlx-diff-id-b-{}.txt", pid));
        let body = "x\ny\nz\n";
        std::fs::write(&pa, body).unwrap();
        std::fs::write(&pb, body).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let args = json!({"path_a": pa.to_string_lossy(), "path_b": pb.to_string_lossy()});
        let out = rt.block_on(run(args)).unwrap();
        let _ = std::fs::remove_file(&pa);
        let _ = std::fs::remove_file(&pb);

        assert!(
            out.starts_with("(identical:"),
            "expected identical short-circuit, got:\n{}",
            out
        );
    }
}
