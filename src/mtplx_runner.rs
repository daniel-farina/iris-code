//! Canonical "optimal" MTPLX server config + helpers to start/restart it.
//!
//! Why this module exists: there's exactly one server invocation that we've
//! validated to deliver the speed + correctness profile hippo-code expects
//! (3-deep MTP, sustained profile, linear-GDN-from-conv-tape verify core,
//! 128K context, the long-context-ladder env-var workaround). This module
//! pins that config in one place so:
//!
//!   * `hip --update` can detect when the running server drifts from it,
//!   * `brew upgrade mtplx` can be followed by an automatic restart on
//!     the new code path with the same args,
//!   * future `hip --start-mtplx` / wizard auto-start can spawn it without
//!     duplicating the long flag list.
//!
//! The brew install layout is the only path supported here today (the
//! interpreter lives at `/opt/homebrew/var/mtplx/venv-<ver>/bin/python`).
//! Git-checkout installs continue to use `setup.rs::start_mtplx_background_and_wait`.

use anyhow::{anyhow, Result};
use std::path::PathBuf;
use std::time::Duration;

/// One source of truth for the validated-good MTPLX command line. Any
/// `--flag value` or boolean `--flag` we want the running server to carry
/// goes here; the comparator + spawner both read from this list.
pub const OPTIMAL_HOST: &str = "127.0.0.1";
pub const OPTIMAL_PORT: u16 = 8088;
pub const OPTIMAL_MODEL_HF_ID: &str = "Youssofal/Qwen3.6-27B-MTPLX-Optimized-Speed";
pub const OPTIMAL_MODEL_ID: &str = "mtplx-qwen36-27b-optimized-speed";

/// Server flags we always pass. Order doesn't matter for the comparator
/// but we keep it close to the validated invocation for readability.
pub const OPTIMAL_FLAGS: &[&str] = &[
    "--depth",
    "3",
    "--generation-mode",
    "mtp",
    "--profile",
    "sustained",
    "--verify-core",
    "linear-gdn-from-conv-tape",
    "--draft-lm-head-bits",
    "3",
    "--draft-lm-head-group-size",
    "64",
    "--draft-lm-head-mode",
    "affine",
    "--draft-temperature",
    "0.7",
    "--draft-top-p",
    "0.95",
    "--draft-top-k",
    "20",
    "--rate-limit",
    "0",
    "--stream-interval",
    "1",
    "--warmup-tokens",
    "16",
    "--no-strict-mlx-fork-assert",
    "--no-strict-startup-asserts",
    "--temperature",
    "0.6",
    "--top-p",
    "0.95",
    "--reasoning-parser",
    "qwen3",
    "--no-stats-footer",
    "--context-window",
    "128000",
];

/// Env vars that must be present when MTPLX is launched. The empty-string
/// LONG_CONTEXT_LADDER override is a workaround until upstream PR #46
/// lands; setting it to "" disables the ladder (the default value mis-tunes
/// 27B sustained throughput).
pub const OPTIMAL_ENV: &[(&str, &str)] = &[("MTPLX_MTP_LONG_CONTEXT_LADDER", "")];

const PID_FILE: &str = "~/.mlx-code/mtplx.pid";
const LOG_FILE: &str = "~/.mlx-code/mtplx.log";

fn expand(s: &str) -> PathBuf {
    PathBuf::from(shellexpand::tilde(s).into_owned())
}

/// Find the brew-installed MTPLX venv Python. Globs
/// `/opt/homebrew/var/mtplx/venv-*/bin/python` (Apple Silicon) and
/// `/usr/local/var/mtplx/venv-*/bin/python` (Intel) and returns the
/// highest-versioned hit. Returns None if MTPLX isn't installed via brew.
pub fn find_brew_python() -> Option<PathBuf> {
    let roots = ["/opt/homebrew/var/mtplx", "/usr/local/var/mtplx"];
    let mut candidates: Vec<PathBuf> = Vec::new();
    for root in &roots {
        let entries = match std::fs::read_dir(root) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                let py = p.join("bin").join("python");
                if py.exists() {
                    candidates.push(py);
                }
            }
        }
    }
    // Sort lexically; venv-0.1.6 < venv-0.3.3 etc. Good enough for two-digit
    // minor/patch components, and the highest-versioned hit is the right
    // pick after a brew upgrade renames the dir.
    candidates.sort();
    candidates.pop()
}

/// Resolve the optimal model directory under ~/.mtplx/models/. The naming
/// convention is HF "user/repo" -> "user--repo" with the slash flipped.
pub fn find_optimal_model_dir() -> Option<PathBuf> {
    let dir_name = OPTIMAL_MODEL_HF_ID.replace('/', "--");
    let path = expand(&format!("~/.mtplx/models/{}", dir_name));
    if path.is_dir() {
        Some(path)
    } else {
        None
    }
}

/// Result of comparing the running MTPLX server's command line against
/// the canonical optimal config.
#[derive(Debug)]
pub struct ConfigDelta {
    /// Required flags that are missing entirely (or whose value differs).
    pub missing_or_wrong: Vec<String>,
    /// Env vars from OPTIMAL_ENV that the running process is missing.
    /// (We can only detect these on Linux via /proc; on macOS this is
    /// always best-effort empty unless we have another channel.)
    pub missing_env: Vec<String>,
}

impl ConfigDelta {
    pub fn is_optimal(&self) -> bool {
        self.missing_or_wrong.is_empty() && self.missing_env.is_empty()
    }
}

/// Pull the running MTPLX server's full command line. Uses ps with the
/// "-ww" wide-output flag so long arg lists aren't truncated.
pub fn running_command_line() -> Option<String> {
    use std::process::Command;
    let pid_out = Command::new("lsof")
        .args(["-nP", "-iTCP:8088", "-sTCP:LISTEN", "-t"])
        .output()
        .ok()?;
    let pid = String::from_utf8_lossy(&pid_out.stdout)
        .lines()
        .next()
        .map(|s| s.trim().to_string())?;
    if pid.is_empty() {
        return None;
    }
    let out = Command::new("ps")
        .args(["-p", &pid, "-ww", "-o", "command="])
        .output()
        .ok()?;
    let cmd = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if cmd.is_empty() {
        None
    } else {
        Some(cmd)
    }
}

/// Pull the PID listening on the optimal port. Returns None if nothing
/// is listening.
pub fn running_pid() -> Option<u32> {
    use std::process::Command;
    let out = Command::new("lsof")
        .args(["-nP", "-iTCP:8088", "-sTCP:LISTEN", "-t"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .and_then(|s| s.trim().parse::<u32>().ok())
}

/// Compare the running command line against OPTIMAL_FLAGS. For each
/// (flag, value) pair we want, we check that the flag appears with the
/// expected value as the immediately following token. For boolean flags
/// (no value), we just check presence.
pub fn config_deltas(running_cmd: &str) -> ConfigDelta {
    let tokens: Vec<&str> = running_cmd.split_whitespace().collect();
    let mut missing_or_wrong: Vec<String> = Vec::new();

    let mut i = 0;
    while i < OPTIMAL_FLAGS.len() {
        let flag = OPTIMAL_FLAGS[i];
        let next = OPTIMAL_FLAGS.get(i + 1).copied();
        let is_boolean = next.map(|n| n.starts_with("--")).unwrap_or(true);
        if is_boolean {
            if !tokens.contains(&flag) {
                missing_or_wrong.push(flag.to_string());
            }
            i += 1;
        } else {
            let value = next.unwrap();
            // Find flag in tokens, check the next token matches value.
            let mut found = false;
            for (idx, t) in tokens.iter().enumerate() {
                if *t == flag {
                    if tokens.get(idx + 1).copied() == Some(value) {
                        found = true;
                    }
                    break;
                }
            }
            if !found {
                missing_or_wrong.push(format!("{} {}", flag, value));
            }
            i += 2;
        }
    }
    // We can't reliably inspect env vars of an already-running process on
    // macOS without root, so just report empty; spawning fresh always sets
    // them correctly.
    ConfigDelta {
        missing_or_wrong,
        missing_env: Vec::new(),
    }
}

/// Spawn MTPLX detached with the canonical optimal config. Writes the PID
/// to ~/.mlx-code/mtplx.pid and logs to ~/.mlx-code/mtplx.log. Returns the
/// child PID once spawned. Does NOT wait for the server to bind -- that's
/// caller's job (see `wait_until_listening`).
pub fn start_mtplx_optimal_background() -> Result<u32> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let python = find_brew_python()
        .ok_or_else(|| anyhow!("could not find brew MTPLX venv python under /opt/homebrew/var/mtplx/ or /usr/local/var/mtplx/"))?;
    let model_dir = find_optimal_model_dir().ok_or_else(|| {
        anyhow!(
            "optimal model not found at ~/.mtplx/models/{}",
            OPTIMAL_MODEL_HF_ID.replace('/', "--")
        )
    })?;

    let log_path = expand(LOG_FILE);
    let pid_path = expand(PID_FILE);
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let log_file = std::fs::File::create(&log_path)
        .map_err(|e| anyhow!("cannot create {}: {}", log_path.display(), e))?;
    let log_err = log_file
        .try_clone()
        .map_err(|e| anyhow!("dup log fd failed: {}", e))?;

    let port_str = OPTIMAL_PORT.to_string();
    let model_str = model_dir.to_string_lossy().to_string();

    let mut cmd = Command::new(&python);
    cmd.args([
        "-m",
        "mtplx.server.openai",
        "--model",
        &model_str,
        "--host",
        OPTIMAL_HOST,
        "--port",
        &port_str,
        "--model-id",
        OPTIMAL_MODEL_ID,
    ]);
    cmd.args(OPTIMAL_FLAGS);
    for (k, v) in OPTIMAL_ENV {
        cmd.env(k, v);
    }
    cmd.stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_err))
        .stdin(Stdio::null());

    let child = unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        })
        .spawn()
    }
    .map_err(|e| anyhow!("failed to spawn MTPLX: {}", e))?;
    let pid = child.id();
    let _ = std::fs::write(&pid_path, format!("{}\n", pid));
    Ok(pid)
}

/// Poll the optimal server URL until it responds or the deadline passes.
/// Returns true if the server came up, false on timeout.
pub async fn wait_until_listening(timeout: Duration) -> bool {
    use std::time::Instant;
    let url = format!("http://{}:{}/v1/models", OPTIMAL_HOST, OPTIMAL_PORT);
    let started = Instant::now();
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    while started.elapsed() < timeout {
        if let Ok(r) = client.get(&url).send().await {
            if r.status().is_success() {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(800)).await;
    }
    false
}

/// SIGTERM the process listening on :8088, wait up to `timeout` for it to
/// exit, escalate to SIGKILL if it's still alive. No-op if nothing's
/// listening.
pub fn stop_running_mtplx(timeout: Duration) {
    let pid = match running_pid() {
        Some(p) => p,
        None => return,
    };
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if running_pid() != Some(pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
}

/// Path to the log file, exposed so callers can print "tail -f <path>"
/// hints in their user-facing status messages.
pub fn log_file() -> PathBuf {
    expand(LOG_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_deltas_flags_missing_pair() {
        let running = "python -m mtplx.server.openai --depth 2 --generation-mode mtp \
             --profile sustained --verify-core linear-gdn-from-conv-tape \
             --draft-lm-head-bits 3 --draft-lm-head-group-size 64 \
             --draft-lm-head-mode affine --draft-temperature 0.7 \
             --draft-top-p 0.95 --draft-top-k 20 --rate-limit 0 \
             --stream-interval 1 --warmup-tokens 16 \
             --no-strict-mlx-fork-assert --no-strict-startup-asserts \
             --temperature 0.6 --top-p 0.95 --reasoning-parser qwen3 \
             --no-stats-footer --context-window 128000";
        let d = config_deltas(running);
        // --depth 3 is expected; running has --depth 2 -> should flag.
        assert!(d.missing_or_wrong.iter().any(|s| s.starts_with("--depth")));
        assert!(!d.is_optimal());
    }

    #[test]
    fn config_deltas_all_present_is_optimal() {
        let mut parts: Vec<String> = vec![
            "python".to_string(),
            "-m".to_string(),
            "mtplx.server.openai".to_string(),
        ];
        let mut i = 0;
        while i < OPTIMAL_FLAGS.len() {
            let flag = OPTIMAL_FLAGS[i];
            let next = OPTIMAL_FLAGS.get(i + 1).copied();
            let is_boolean = next.map(|n| n.starts_with("--")).unwrap_or(true);
            parts.push(flag.to_string());
            if !is_boolean {
                parts.push(next.unwrap().to_string());
                i += 2;
            } else {
                i += 1;
            }
        }
        let running = parts.join(" ");
        let d = config_deltas(&running);
        assert!(
            d.is_optimal(),
            "expected optimal, got deltas: {:?}",
            d.missing_or_wrong
        );
    }

    #[test]
    fn find_optimal_model_dir_handles_missing() {
        // The function should return None when path doesn't exist; we can't
        // assert it returns Some without depending on local state, so just
        // make sure it doesn't panic.
        let _ = find_optimal_model_dir();
    }
}
