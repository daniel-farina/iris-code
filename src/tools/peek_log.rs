//! `peek_log` tool: surface recent run history so the agent (or user) can
//! answer "what's been happening in this project?" without leaving the model.
//! Reads ~/.mlx-code/logs/runs.jsonl tail-style.

use anyhow::{anyhow, Result};
use futures_util::future::FutureExt;
use serde_json::{json, Value};

use super::Tool;

const DEFAULT_N: usize = 10;
const MAX_N: usize = 50;

pub fn tool() -> Tool {
    Tool {
        name: "peek_log",
        schema: json!({
            "type": "function",
            "function": {
                "name": "peek_log",
                "description": "Return last N entries from ~/.mlx-code/logs/runs.jsonl (default 10, max 50). Useful to ask 'what's been failing?' or 'did my last edit succeed?'",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "n": { "type": "integer", "description": "default 10, max 50" },
                        "failed_only": { "type": "boolean", "description": "only failed runs; default false" }
                    }
                }
            }
        }),
        exec: |args| async move { run(args).await }.boxed(),
    }
}

async fn run(args: Value) -> Result<String> {
    let n = args
        .get("n")
        .and_then(|v| v.as_u64())
        .map(|x| x as usize)
        .unwrap_or(DEFAULT_N)
        .min(MAX_N)
        .max(1);
    let failed_only = args
        .get("failed_only")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let path = shellexpand::tilde("~/.mlx-code/logs/runs.jsonl").into_owned();
    let body = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => return Err(anyhow!("peek_log: cannot read {}: {}", path, e)),
    };

    let mut rows: Vec<Value> = body
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect();
    if failed_only {
        rows.retain(|r| !r.get("success").and_then(|v| v.as_bool()).unwrap_or(true));
    }
    let total = rows.len();
    let take = rows.into_iter().rev().take(n).collect::<Vec<_>>();
    let take = take.into_iter().rev().collect::<Vec<_>>();

    if take.is_empty() {
        let suffix = if failed_only {
            " (failed_only filter; use without to see all)"
        } else {
            ""
        };
        return Ok(format!("(no entries in {}{})\n", path, suffix));
    }

    let mut out = String::new();
    out.push_str(&format!(
        "─ peek_log ─ showing {} of {} row(s){}{}\n",
        take.len(),
        total,
        if failed_only { " (failed_only)" } else { "" },
        if total > take.len() {
            format!(" (file has more)")
        } else {
            String::new()
        },
    ));
    for r in &take {
        let ts = r.get("ts_unix").and_then(|v| v.as_u64()).unwrap_or(0);
        let mode = r.get("mode").and_then(|v| v.as_str()).unwrap_or("?");
        let success = r.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
        let snippet = r
            .get("prompt_first_120_chars")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let rounds = r.get("rounds").and_then(|v| v.as_u64()).unwrap_or(0);
        let tool_calls = r.get("tool_calls").and_then(|v| v.as_u64()).unwrap_or(0);
        let total_ms = r.get("total_ms").and_then(|v| v.as_u64()).unwrap_or(0);
        let tps = r.get("decode_tok_per_s").and_then(|v| v.as_f64());
        let err = r
            .get("error")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        let tag = if success { "ok " } else { "ERR" };
        let ts_str = format_ts(ts);
        let prompt_preview: String = snippet.chars().take(60).collect();
        let tps_str = match tps {
            Some(v) if v > 0.0 && v < 200.0 => format!("{:.0} t/s", v),
            _ => "-".into(),
        };
        out.push_str(&format!(
            "  {} [{}] {:<13} {}ms r={} tc={} {}\n",
            tag, ts_str, mode, total_ms, rounds, tool_calls, tps_str
        ));
        out.push_str(&format!("       \"{}\"\n", prompt_preview));
        if let Some(e) = err {
            let e_short: String = e.chars().take(120).collect();
            out.push_str(&format!("       err: {}\n", e_short));
        }
    }
    Ok(out)
}

fn format_ts(ts: u64) -> String {
    if ts == 0 {
        return "????-??-?? ??:??".into();
    }
    use std::time::{Duration, UNIX_EPOCH};
    let epoch = UNIX_EPOCH + Duration::from_secs(ts);
    // Cheap formatter without chrono: use system time arithmetic.
    let secs_since_epoch = ts;
    // Days since 1970-01-01; 86400s/day.
    let days = secs_since_epoch / 86400;
    let secs_in_day = secs_since_epoch % 86400;
    let hours = secs_in_day / 3600;
    let minutes = (secs_in_day % 3600) / 60;
    // Convert days-since-epoch to YYYY-MM-DD (Howard Hinnant's algorithm).
    let z = days as i64 + 719468;
    let era = if z >= 0 {
        z / 146097
    } else {
        (z - 146096) / 146097
    };
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let _ = epoch;
    format!("{:04}-{:02}-{:02} {:02}:{:02}", y, m, d, hours, minutes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt_run(args: Value) -> Result<String> {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(run(args))
    }

    /// We can't easily mock $HOME/~/.mlx-code/logs/runs.jsonl from a unit
    /// test without changing global state, so this test only verifies the
    /// tool errors cleanly when no log exists OR returns a parseable result
    /// when one does. Detailed format is exercised by the hand-run end-user
    /// path.
    #[test]
    fn peek_log_handles_missing_or_present_log() {
        let res = rt_run(json!({"n": 1}));
        // Either: (a) the user has a runs.jsonl - in which case we get a
        // formatted table OR a "(no entries" message; (b) they don't, and we
        // get an Err. Both are acceptable - we just want no panics.
        match res {
            Ok(s) => {
                assert!(
                    s.contains("─ peek_log ─") || s.contains("(no entries"),
                    "unexpected ok output:\n{}",
                    s
                );
            }
            Err(_) => {
                // Missing file - acceptable.
            }
        }
    }

    #[test]
    fn format_ts_returns_iso_like_string() {
        // 2024-01-01 00:00:00 UTC = 1704067200
        let s = format_ts(1704067200);
        assert!(
            s.starts_with("2024-01-01"),
            "expected 2024-01-01 prefix, got: {}",
            s
        );
    }

    #[test]
    fn format_ts_zero_returns_placeholder() {
        let s = format_ts(0);
        assert!(s.contains("?"));
    }
}
