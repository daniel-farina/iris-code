//! Per-invocation run log. Appends one JSONL line to
//! `~/.mlx-code/logs/runs.jsonl` per `mlx-code` run with high-level metrics
//! so we can track perf and behaviour over time.

use serde::Serialize;
use std::time::SystemTime;

#[derive(Debug, Serialize)]
pub struct RunLog {
    pub ts_unix: u64,
    pub mode: String, // "one-shot" | "agent" | "chat"
    pub session_id: String,
    pub model: String,
    pub prompt_first_120_chars: String,
    /// Absolute path of cwd at run time. Surfaced in the `--resume` picker
    /// so the user can tell which conversation belongs to which project.
    #[serde(default)]
    pub cwd: String,
    pub success: bool,
    pub error: Option<String>,
    pub rounds: Option<u32>,
    pub tool_calls: Option<u32>,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub ttft_ms: Option<u128>,
    pub total_ms: Option<u128>,
    pub decode_tok_per_s: Option<f64>,
    pub mlx_code_version: String,
}

impl RunLog {
    pub fn new(mode: &str, session: &str, model: &str, prompt: &str) -> Self {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let snippet: String = prompt.chars().take(120).collect();
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        Self {
            ts_unix: now,
            mode: mode.to_string(),
            session_id: session.to_string(),
            model: model.to_string(),
            prompt_first_120_chars: snippet,
            cwd,
            success: true,
            error: None,
            rounds: None,
            tool_calls: None,
            prompt_tokens: None,
            completion_tokens: None,
            ttft_ms: None,
            total_ms: None,
            decode_tok_per_s: None,
            mlx_code_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    pub fn write(&self) {
        // Best effort: don't fail the run if logging fails.
        let Ok(home) = std::env::var("HOME") else {
            return;
        };
        let dir = format!("{}/.mlx-code/logs", home);
        let _ = std::fs::create_dir_all(&dir);
        let path = format!("{}/runs.jsonl", dir);
        let Ok(mut line) = serde_json::to_string(self) else {
            return;
        };
        line.push('\n');
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| {
                use std::io::Write;
                f.write_all(line.as_bytes())
            });
    }
}
