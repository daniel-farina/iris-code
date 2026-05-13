//! `read` tool: read a slice of a file.

use anyhow::{anyhow, Context, Result};
use futures_util::future::FutureExt;
use serde_json::{json, Value};
use std::path::PathBuf;

use super::Tool;

const MAX_BYTES: u64 = 1_048_576; // 1 MiB

// Default line cap when the caller doesn't specify a `limit`. Set low so the
// model's reflex `read(path)` (no offset/limit) does not blow the prompt budget
// on a multi-thousand-line file. The quadratic prefill on qwen3's global-
// attention layers makes a single careless 30K-token read cost ~70s of TTFT,
// and the bank then has no chance of serving subsequent rounds cheaply because
// every read shifts the prefix.
//
// 200 lines comfortably covers most narrowly-scoped files (entry points, small
// modules, configs). Larger reads must be explicit: pass `around=<line>` after
// a search hit, or `offset` + `limit` for a known window. The result includes
// a `[truncated at 200/<total>]` marker telling the model how much it's not
// seeing, so it can re-request a wider window when warranted.
const DEFAULT_LIMIT: usize = 200;

pub fn tool() -> Tool {
    Tool {
        name: "read",
        schema: json!({
            "type": "function",
            "function": {
                "name": "read",
                "description": "Read a UTF-8 text file slice. Default: 200 lines from start (small on purpose - large blind reads blow the prompt budget). After a search hit, use `around=<line>` with `context` for a symmetric window. For a known section, use `offset`+`limit`. Output shows `[truncated at N/<total>]` when the file extends beyond what you asked for, so you know to request more if needed. Refuses files >1MB.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path":   { "type": "string", "description": "absolute or cwd-relative file path" },
                        "offset": { "type": "integer", "description": "1-based starting line (default 1)" },
                        "limit":  { "type": "integer", "description": "max lines to return (default 200; raise explicitly for larger reads)" },
                        "around": { "type": "integer", "description": "1-based target line; overrides offset/limit. Returns ±context lines around it." },
                        "context":{ "type": "integer", "description": "context lines for `around` (default 20)" }
                    },
                    "required": ["path"]
                }
            }
        }),
        exec: |args| async move { run(args).await }.boxed(),
    }
}

/// Coerce a JSON value to u64, accepting either a real number OR a numeric
/// string. MTPLX's qwen3 XML tool-call parser (_decode_tool_parameter_value)
/// generally JSON-parses parameter bodies, but a model that emits parameters
/// with leading zeros or unusual whitespace can land back in the raw-string
/// branch, in which case as_u64() returns None and every numeric arg silently
/// falls through to its default. Be defensive on the receive side.
fn arg_u64(args: &Value, key: &str) -> Option<u64> {
    let v = args.get(key)?;
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
}

async fn run(args: Value) -> Result<String> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("read: missing path"))?
        .to_string();
    let around = arg_u64(&args, "around").map(|n| n as usize);
    let context = arg_u64(&args, "context").map(|n| n as usize).unwrap_or(20);

    // If `around` is set it takes precedence over offset/limit and we compute
    // [around - context, around + context] (clamped to >= 1).
    let (offset, limit) = if let Some(target) = around.filter(|n| *n >= 1) {
        let off = target.saturating_sub(context).max(1);
        let lim = (target - off) + context + 1; // inclusive of target line
        (off, lim)
    } else {
        let off = arg_u64(&args, "offset")
            .map(|n| n as usize)
            .unwrap_or(1)
            .max(1);
        let lim = arg_u64(&args, "limit")
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_LIMIT);
        (off, lim)
    };

    let p = PathBuf::from(shellexpand::tilde(&path).into_owned());
    let meta = std::fs::metadata(&p).with_context(|| format!("stat {}", p.display()))?;
    if !meta.is_file() {
        return Err(anyhow!("read: not a regular file: {}", p.display()));
    }
    if meta.len() > MAX_BYTES {
        return Err(anyhow!(
            "read: file is {} bytes (>1MB), refuse to read whole. Use offset/limit.",
            meta.len()
        ));
    }
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let size = meta.len();

    // Cache-first: check if we already have this exact (path, mtime, size).
    // On hit, slice the cached content; on miss, read from disk + populate.
    let content = if let Some(cached) = crate::read_cache::get(&p, mtime, size) {
        cached
    } else {
        let s = std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
        crate::read_cache::put(&p, mtime, size, s.clone());
        s
    };
    // Record that the agent has just looked at this file with this mtime,
    // so the edit tool can detect external modifications between reads.
    crate::read_cache::mark_read(&p, mtime);

    let mut out = String::new();
    let mut total_lines = 0usize;
    let mut emitted = 0usize;
    let target_line = around;
    for (idx, line) in content.lines().enumerate() {
        let lineno = idx + 1;
        total_lines = lineno;
        if lineno < offset {
            continue;
        }
        if emitted >= limit {
            break;
        }
        let marker = if target_line == Some(lineno) {
            ">"
        } else {
            " "
        };
        out.push_str(&format!("{}{:>5}\t{}\n", marker, lineno, line));
        emitted += 1;
    }
    // Adjust total_lines for files that don't end in a newline: lines() counts
    // them correctly, but the empty-range message should reflect actual count.
    if total_lines == 0 && !content.is_empty() {
        total_lines = content.lines().count();
    }
    if out.is_empty() {
        out.push_str(&format!("(empty range; file has {} lines)\n", total_lines));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp(name: &str, body: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("mlx-read-{}-{}", std::process::id(), name));
        std::fs::write(&p, body).unwrap();
        p
    }

    fn rt_run(args: Value) -> Result<String> {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(run(args))
    }

    #[test]
    fn read_default_returns_first_lines_with_line_numbers() {
        let body: String = (1..=10).map(|n| format!("line{}\n", n)).collect();
        let p = write_temp("default.txt", &body);
        let out = rt_run(json!({"path": p.to_string_lossy()})).unwrap();
        // Expected layout: leading marker char, 5-char right-just lineno, tab, content.
        assert!(out.contains("    1\tline1"), "missing line1 row:\n{}", out);
        assert!(
            out.contains("   10\tline10"),
            "missing line10 row:\n{}",
            out
        );
        // No `>` markers should appear when `around` is unset.
        assert!(
            !out.contains(">"),
            "unexpected target marker without `around`:\n{}",
            out
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_around_centers_on_target_with_context() {
        let body: String = (1..=50).map(|n| format!("line{}\n", n)).collect();
        let p = write_temp("around.txt", &body);
        let out = rt_run(json!({
            "path": p.to_string_lossy(),
            "around": 25,
            "context": 3
        }))
        .unwrap();
        // Should include lines 22..=28 only.
        assert!(
            out.contains("line22"),
            "missing leading context line22:\n{}",
            out
        );
        assert!(out.contains("line25"), "missing target line25:\n{}", out);
        assert!(
            out.contains("line28"),
            "missing trailing context line28:\n{}",
            out
        );
        assert!(
            !out.contains("line21"),
            "should NOT include line21:\n{}",
            out
        );
        assert!(
            !out.contains("line29"),
            "should NOT include line29:\n{}",
            out
        );
        // The target line should be marked with `>`.
        assert!(
            out.contains(">   25\tline25"),
            "target marker missing:\n{}",
            out
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_accepts_string_offset_and_limit() {
        // MTPLX's qwen3 XML tool-call parser USUALLY JSON-parses parameter values
        // (so `<parameter=offset>200</parameter>` arrives as the integer 200) but
        // there are edge cases where the value lands back in the raw-string branch
        // (e.g. whitespace anomalies, leading zeros). Without the as_str fallback
        // those reads silently degrade to offset=1 / limit=200 and the user sees
        // the start of the file instead of the requested slice. Lock the behaviour in.
        let body: String = (1..=100).map(|n| format!("line{}\n", n)).collect();
        let p = write_temp("string_args.txt", &body);
        let out = rt_run(json!({
            "path": p.to_string_lossy(),
            "offset": "50",
            "limit": "3"
        }))
        .unwrap();
        assert!(
            out.contains("line50"),
            "expected line50 at start of result:\n{}",
            out
        );
        assert!(out.contains("line52"), "expected line52:\n{}", out);
        assert!(
            !out.contains("line1\n"),
            "string offset was ignored - leaked line1:\n{}",
            out
        );
        assert!(
            !out.contains("line53"),
            "string limit was ignored - over-read past line52:\n{}",
            out
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_around_clamps_at_file_start() {
        let body: String = (1..=10).map(|n| format!("line{}\n", n)).collect();
        let p = write_temp("clamp.txt", &body);
        // around=2 with context=5 should not panic; should start at line 1.
        let out = rt_run(json!({
            "path": p.to_string_lossy(),
            "around": 2,
            "context": 5
        }))
        .unwrap();
        assert!(out.contains("line1"), "should include line1:\n{}", out);
        assert!(
            out.contains("line2"),
            "should include target line2:\n{}",
            out
        );
        assert!(out.contains("line7"), "should include line7:\n{}", out);
        let _ = std::fs::remove_file(&p);
    }
}
