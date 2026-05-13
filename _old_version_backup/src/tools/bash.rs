//! `bash` tool: run a shell command in zsh, capture stdout+stderr+exit.

use anyhow::{anyhow, Result};
use futures_util::future::FutureExt;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

use super::Tool;

pub fn tool() -> Tool {
    Tool {
        name: "bash",
        schema: json!({
            "type": "function",
            "function": {
                "name": "bash",
                "description": "Run a shell command in zsh. Returns combined stdout+stderr and exit code. Default timeout 30s.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" },
                        "timeout_s": { "type": "integer", "description": "max seconds (default 30)" }
                    },
                    "required": ["command"]
                }
            }
        }),
        exec: |args| async move { run(args).await }.boxed(),
    }
}

async fn run(args: Value) -> Result<String> {
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("bash: missing command"))?
        .to_string();
    let secs = args.get("timeout_s").and_then(|v| v.as_u64()).unwrap_or(30);

    // Agent-loop dry_run: bash command runs are mutation-shaped from the
    // model's perspective (could touch the filesystem, network, processes).
    // Skip execution and report what would have run.
    if std::env::var("MLX_CODE_DRY_RUN")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        crate::dry_run_log::record("bash", command.clone());
        return Ok(format!(
            "(dry_run) would run: {}\n[exit dry_run]\n",
            command
        ));
    }

    let mut cmd = Command::new("zsh");
    cmd.arg("-lc").arg(&command);
    cmd.kill_on_drop(true);

    let fut = cmd.output();
    let out = match timeout(Duration::from_secs(secs), fut).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(anyhow!("bash: spawn failed: {}", e)),
        Err(_) => return Ok(format!("[timed out after {}s]\n$ {}\n", secs, command)),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let code = out
        .status
        .code()
        .map(|c| c.to_string())
        .unwrap_or_else(|| "signal".into());
    let mut buf = String::new();
    buf.push_str(&format!("$ {}\n", command));
    buf.push_str(&stdout);
    if !stderr.is_empty() {
        if !buf.ends_with('\n') {
            buf.push('\n');
        }
        buf.push_str("--- stderr ---\n");
        buf.push_str(&stderr);
    }
    if !buf.ends_with('\n') {
        buf.push('\n');
    }
    buf.push_str(&format!("[exit {}]\n", code));
    // Truncate huge outputs to keep context manageable.
    if buf.len() > 16_000 {
        let head = &buf[..8_000];
        let tail = &buf[buf.len() - 4_000..];
        buf = format!(
            "{}\n... [truncated {} bytes] ...\n{}",
            head,
            buf.len() - 12_000,
            tail
        );
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt_run(args: Value) -> Result<String> {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(run(args))
    }

    #[test]
    fn bash_dry_run_env_var_skips_execution() {
        let _guard = crate::dry_run_log::ENV_LOCK.lock().unwrap();
        // Use a unique sentinel command that, if executed, would fail noisily.
        // With MLX_CODE_DRY_RUN=1 we should see only the (dry_run) preview.
        std::env::set_var("MLX_CODE_DRY_RUN", "1");
        let res = rt_run(serde_json::json!({"command": "exit 7"})).unwrap();
        std::env::remove_var("MLX_CODE_DRY_RUN");
        assert!(
            res.contains("(dry_run) would run: exit 7"),
            "unexpected dry_run output:\n{}",
            res
        );
        // Crucially: the actual exit-7 marker that bash would emit must NOT be present.
        assert!(
            !res.contains("[exit 7]"),
            "command was actually executed:\n{}",
            res
        );
        assert!(
            res.contains("[exit dry_run]"),
            "missing dry_run exit marker:\n{}",
            res
        );
    }
}
