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

// ---------- Unified install state ----------

/// How MTPLX got onto this machine. Each variant carries the info needed
/// to (a) print a sensible status banner and (b) drive the right upgrade
/// path (`brew upgrade` vs `git pull`).
#[derive(Debug, Clone)]
pub enum InstallKind {
    Brew {
        formula: String,
        installed_version: String,
    },
    Git {
        repo_root: PathBuf,
        remote: String,
        branch: String,
        head_sha: String,
        head_date: String,
    },
}

/// Everything `hip --update` needs to know about the local MTPLX install
/// in one place. Built once at the top of the check; passed through the
/// render/upgrade/restart pipeline so each step doesn't re-probe state.
#[derive(Debug, Clone)]
pub struct MtplxState {
    pub kind: InstallKind,
    /// The Python interpreter we'd respawn the server with. For brew this
    /// is the latest venv-*/bin/python (which changes after `brew upgrade`).
    /// For git checkouts it's `<repo_root>/.venv/bin/python`.
    pub python_path: PathBuf,
    pub server_pid: Option<u32>,
    pub running_command: Option<String>,
    /// For brew: the venv version the running process's open files point
    /// at -- used to detect "you upgraded brew but never restarted."
    pub running_venv_version: Option<String>,
}

impl MtplxState {
    pub fn is_running(&self) -> bool {
        self.server_pid.is_some()
    }
    /// Compare running flags against canonical. Returns None when the
    /// server isn't running (no command line to compare).
    pub fn config_status(&self) -> Option<ConfigDelta> {
        self.running_command.as_deref().map(config_deltas)
    }
}

/// Detect the local MTPLX install (brew or git checkout) plus everything
/// about the running server, if any. Returns None when no install can be
/// located and nothing is listening on :8088 -- the caller should suggest
/// `hip --setup` or `brew install youssofal/mtplx/mtplx`.
pub fn detect_state() -> Option<MtplxState> {
    use std::process::Command;

    let pid = running_pid();
    let cmd = running_command_line();
    let running_venv = running_venv_version();

    // 1. Explicit env override pointing at a git checkout.
    if let Ok(v) = std::env::var("HIP_MTPLX_INSTALL_DIR") {
        let p = PathBuf::from(&v);
        if p.join(".git").exists() {
            if let Some(git_kind) = build_git_kind(&p) {
                return Some(MtplxState {
                    kind: git_kind,
                    python_path: p.join(".venv").join("bin").join("python"),
                    server_pid: pid,
                    running_command: cmd,
                    running_venv_version: running_venv,
                });
            }
        }
    }

    // 2. Wizard default git checkout.
    let default_checkout = PathBuf::from(shellexpand::tilde("~/code/MTPLX").into_owned());
    if default_checkout.join(".git").exists() {
        if let Some(git_kind) = build_git_kind(&default_checkout) {
            return Some(MtplxState {
                kind: git_kind,
                python_path: default_checkout.join(".venv").join("bin").join("python"),
                server_pid: pid,
                running_command: cmd,
                running_venv_version: running_venv,
            });
        }
    }

    // 3. Brew install (passive check via `brew list`).
    let brew_installed = Command::new("brew")
        .args(["list", "--formula", "mtplx"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if brew_installed {
        let (formula, installed_version) = detect_brew_mtplx_version();
        if let Some(python_path) = find_brew_python() {
            return Some(MtplxState {
                kind: InstallKind::Brew {
                    formula,
                    installed_version,
                },
                python_path,
                server_pid: pid,
                running_command: cmd,
                running_venv_version: running_venv,
            });
        }
    }

    // 4. Last-ditch: probe the running server's cwd for an ancestor with
    //    .git + setup.py/pyproject. Same heuristic we used before.
    if let Some(probe_pid) = pid {
        if let Some(cwd) = process_cwd(probe_pid) {
            let mut cur = cwd.as_path();
            loop {
                if (cur.join(".git").exists() && cur.join("setup.py").exists())
                    || (cur.join(".git").exists() && cur.join("pyproject.toml").exists())
                {
                    if let Some(git_kind) = build_git_kind(cur) {
                        return Some(MtplxState {
                            kind: git_kind,
                            python_path: cur.join(".venv").join("bin").join("python"),
                            server_pid: pid,
                            running_command: cmd,
                            running_venv_version: running_venv,
                        });
                    }
                    break;
                }
                match cur.parent() {
                    Some(p) => cur = p,
                    None => break,
                }
            }
        }
    }

    None
}

fn build_git_kind(repo_root: &std::path::Path) -> Option<InstallKind> {
    use std::process::Command;
    let dir_str = repo_root.to_string_lossy().to_string();
    let remote = Command::new("git")
        .args(["-C", &dir_str, "remote", "get-url", "origin"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let branch = Command::new("git")
        .args(["-C", &dir_str, "rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let head_sha = Command::new("git")
        .args(["-C", &dir_str, "rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let head_date = Command::new("git")
        .args(["-C", &dir_str, "log", "-1", "--format=%cs", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if branch.is_empty() {
        return None;
    }
    Some(InstallKind::Git {
        repo_root: repo_root.to_path_buf(),
        remote,
        branch,
        head_sha,
        head_date,
    })
}

fn process_cwd(pid: u32) -> Option<PathBuf> {
    use std::process::Command;
    Command::new("lsof")
        .args(["-p", &pid.to_string(), "-a", "-d", "cwd", "-Fn"])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .find(|l| l.starts_with('n'))
                .map(|l| PathBuf::from(&l[1..]))
        })
}

/// (formula-with-tap, installed-version) for the brew-installed MTPLX,
/// or sensible defaults if brew info can't be parsed.
pub fn detect_brew_mtplx_version() -> (String, String) {
    use std::process::Command;
    let info = Command::new("brew")
        .args(["info", "--json=v2", "mtplx"])
        .output()
        .ok();
    let mut version = "unknown".to_string();
    let mut formula = "youssofal/mtplx/mtplx".to_string();
    if let Some(out) = info {
        if let Ok(body) = String::from_utf8(out.stdout) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                if let Some(formulae) = v.get("formulae").and_then(|x| x.as_array()) {
                    if let Some(f) = formulae.first() {
                        if let Some(t) = f.get("tap").and_then(|x| x.as_str()) {
                            if let Some(n) = f.get("name").and_then(|x| x.as_str()) {
                                formula = format!("{}/{}", t, n);
                            }
                        }
                        if let Some(installed) = f
                            .get("installed")
                            .and_then(|x| x.as_array())
                            .and_then(|a| a.first())
                        {
                            if let Some(vstr) = installed.get("version").and_then(|x| x.as_str()) {
                                version = vstr.to_string();
                            }
                        }
                    }
                }
            }
        }
    }
    (formula, version)
}

/// Latest brew version available for upgrade, or None if up to date /
/// brew unavailable.
pub fn brew_mtplx_latest_available() -> Option<String> {
    use std::process::Command;
    let out = Command::new("brew")
        .args(["outdated", "--json=v2", "mtplx"])
        .output()
        .ok()?;
    let body = String::from_utf8(out.stdout).ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    let formulae = v.get("formulae")?.as_array()?;
    let first = formulae.first()?;
    first
        .get("current_version")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
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

/// Extract the brew MTPLX venv version that the currently-running server
/// has files mapped from. After `brew upgrade mtplx` replaces the venv
/// on disk, the running Python process still has the OLD venv's site-
/// packages held open via inode preservation -- so its open-files list
/// betrays the version the running code actually came from, even if the
/// directory on disk has been replaced.
///
/// Returns None when nothing is listening on :8088 or when no venv path
/// shows up in the open-files list (e.g., user installed via something
/// other than brew).
pub fn running_venv_version() -> Option<String> {
    use std::process::Command;
    let pid = running_pid()?;
    let out = Command::new("lsof")
        .args(["-p", &pid.to_string()])
        .output()
        .ok()?;
    let body = String::from_utf8_lossy(&out.stdout);
    for line in body.lines() {
        // `?` would short-circuit the loop on the first line without a
        // venv path -- the first match wins, so we iterate.
        if let Some(idx) = line.find("/var/mtplx/venv-") {
            let after = &line[idx + "/var/mtplx/venv-".len()..];
            // Version runs until next / or first non-version char (digits/dots only).
            let end = after
                .char_indices()
                .find(|(_, c)| !c.is_ascii_digit() && *c != '.')
                .map(|(i, _)| i)
                .unwrap_or(after.len());
            let version = &after[..end];
            if !version.is_empty() {
                return Some(version.to_string());
            }
        }
    }
    None
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

/// Extract a small set of "headline" flag values from the running
/// command line for at-a-glance display. Returns (label, value) pairs
/// in the order callers should print them. Unknown values become
/// "(not set)" so the display never silently omits a row.
pub fn summarize_running_config(running_cmd: &str) -> Vec<(&'static str, String)> {
    let tokens: Vec<&str> = running_cmd.split_whitespace().collect();
    let get = |flag: &str| -> String {
        for (i, t) in tokens.iter().enumerate() {
            if *t == flag {
                if let Some(v) = tokens.get(i + 1) {
                    return v.to_string();
                }
            }
        }
        "(not set)".to_string()
    };
    let model_path = get("--model");
    // Display just the directory name -- the full ~/.mtplx/models/... path
    // is noise in a status table.
    let model_short = std::path::Path::new(&model_path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or(model_path);
    vec![
        ("model id", get("--model-id")),
        ("model", model_short),
        ("server", format!("{}:{}", get("--host"), get("--port"))),
        (
            "decode",
            format!(
                "{} depth={} profile={}",
                get("--generation-mode"),
                get("--depth"),
                get("--profile"),
            ),
        ),
        ("verify core", get("--verify-core")),
        ("context", format!("{} tokens", get("--context-window"))),
        (
            "sampling",
            format!("temp={} top-p={}", get("--temperature"), get("--top-p"),),
        ),
        ("reasoning", get("--reasoning-parser")),
    ]
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

/// Spawn MTPLX detached with the canonical optimal config, using the
/// caller-supplied Python interpreter. Writes the PID to
/// ~/.mlx-code/mtplx.pid and logs to ~/.mlx-code/mtplx.log. Returns the
/// child PID once spawned. Does NOT wait for the server to bind -- that's
/// caller's job (see `wait_until_listening`).
pub fn start_mtplx_optimal_background_with(python: &std::path::Path) -> Result<u32> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

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

    let mut cmd = Command::new(python);
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

/// Backwards-compat wrapper: spawn using the brew Python (auto-detected).
/// Returns Err when no brew install can be found.
pub fn start_mtplx_optimal_background() -> Result<u32> {
    let python = find_brew_python().ok_or_else(|| {
        anyhow!(
            "could not find brew MTPLX venv python under /opt/homebrew/var/mtplx/ \
             or /usr/local/var/mtplx/"
        )
    })?;
    start_mtplx_optimal_background_with(&python)
}

// ---------- High-level orchestration ----------

/// Print a uniform install/version/config/details banner for the given
/// state. Same shape for brew and git so users see a consistent picture.
pub fn render_status(state: &MtplxState) {
    use crate::theme::{accent, dim, good, warn, RESET};
    let d = dim();
    let a = accent();
    let g = good();
    let w = warn();
    let r = RESET;

    match &state.kind {
        InstallKind::Brew {
            formula,
            installed_version,
        } => {
            eprintln!("  {d}install{r}:  {a}{}{r} {d}(Homebrew){r}", formula);
            eprintln!("  {d}brew ver{r}: {a}{}{r}", installed_version);
        }
        InstallKind::Git {
            repo_root,
            remote,
            branch,
            head_sha,
            head_date,
        } => {
            eprintln!(
                "  {d}install{r}:  {a}{}{r} {d}(git checkout){r}",
                repo_root.display()
            );
            let label = MtplxSource::classify(remote, branch);
            eprintln!(
                "  {d}current{r}:  {a}{}{r} @ {a}{}{r}  {d}[{}]{r}",
                if remote.is_empty() {
                    "<no-remote>"
                } else {
                    remote.as_str()
                },
                branch,
                label,
            );
            if !head_sha.is_empty() {
                eprintln!(
                    "  {d}version{r}:  {a}{}{r}{}",
                    head_sha,
                    if head_date.is_empty() {
                        String::new()
                    } else {
                        format!(" {d}({}){r}", head_date)
                    }
                );
            }
        }
    }

    // Config status (only meaningful when the server is running).
    if let Some(delta) = state.config_status() {
        if delta.is_optimal() {
            eprintln!("  {d}config{r}:   {g}optimal{r} {d}(all canonical flags match){r}");
        } else {
            eprintln!(
                "  {d}config{r}:   {w}suboptimal{r} {d}({} delta(s)){r}",
                delta.missing_or_wrong.len()
            );
            for missing in &delta.missing_or_wrong {
                eprintln!("    {w}-{r} missing/wrong: {a}{}{r}", missing);
            }
        }
        if let Some(cmd) = &state.running_command {
            let details = summarize_running_config(cmd);
            eprintln!("  {d}details{r}:");
            for (label, value) in &details {
                eprintln!("    {d}{:<11}{r} {a}{}{r}", label, value);
            }
        }
    } else if state.is_running() {
        eprintln!("  {d}config{r}:   {w}(running but no command line readable){r}");
    } else {
        eprintln!("  {d}config{r}:   {w}not running{r}");
    }
}

/// Probe for an upstream update appropriate to the install kind. Returns
/// Some(label) describing the available newer version when one exists.
pub async fn check_upstream(state: &MtplxState) -> Option<String> {
    match &state.kind {
        InstallKind::Brew { .. } => brew_mtplx_latest_available(),
        InstallKind::Git {
            repo_root, branch, ..
        } => {
            use std::process::Command;
            let dir = repo_root.to_string_lossy().to_string();
            let ok = Command::new("git")
                .args(["-C", &dir, "fetch", "origin", "--quiet"])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                return None;
            }
            let behind = Command::new("git")
                .args([
                    "-C",
                    &dir,
                    "rev-list",
                    "--count",
                    &format!("HEAD..origin/{}", branch),
                ])
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .and_then(|s| s.trim().parse::<u32>().ok())
                .unwrap_or(0);
            if behind > 0 {
                Some(format!("{} commit(s) behind origin/{}", behind, branch))
            } else {
                None
            }
        }
    }
}

/// Apply the upgrade matching the install kind. Returns true on success.
/// Stdout/stderr stream through to the user so they can watch the upgrade.
pub async fn apply_upgrade(state: &MtplxState) -> bool {
    use std::process::Command;
    match &state.kind {
        InstallKind::Brew { .. } => Command::new("brew")
            .args(["upgrade", "mtplx"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false),
        InstallKind::Git {
            repo_root, branch, ..
        } => {
            let dir = repo_root.to_string_lossy().to_string();
            Command::new("git")
                .args(["-C", &dir, "pull", "--ff-only", "origin", branch])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }
    }
}

/// Whether the running server has code that's out of sync with what's on
/// disk. For brew: running_venv_version != installed_version. For git:
/// caller must pass `just_pulled` because we can't read the running
/// process's commit SHA from outside.
pub fn is_running_stale(state: &MtplxState, just_pulled: bool) -> bool {
    if !state.is_running() {
        return false;
    }
    match &state.kind {
        InstallKind::Brew {
            installed_version, ..
        } => state
            .running_venv_version
            .as_deref()
            .map(|v| v != installed_version.as_str())
            .unwrap_or(false),
        InstallKind::Git { .. } => just_pulled,
    }
}

/// Stop the running server (if any) and respawn it with the optimal
/// config using state.python_path. Waits up to 5 min for the new server
/// to bind. Reports progress to stderr at every step.
pub async fn restart_with_optimal_config(state: &MtplxState) {
    use crate::theme::{accent, dim, good, warn, RESET};
    let d = dim();
    let a = accent();
    let g = good();
    let w = warn();
    let r = RESET;

    eprintln!();
    eprintln!("  {d}restarting MTPLX with optimal config to pick up the new code...{r}");
    stop_running_mtplx(Duration::from_secs(15));
    match start_mtplx_optimal_background_with(&state.python_path) {
        Ok(pid) => {
            eprintln!(
                "  {g}✓{r} new MTPLX spawned (pid {a}{}{r}), waiting for model load...",
                pid
            );
            let up = wait_until_listening(Duration::from_secs(300)).await;
            if up {
                eprintln!("  {g}✓{r} MTPLX is listening on :{}", OPTIMAL_PORT);
            } else {
                eprintln!(
                    "  {w}!{r} MTPLX didn't bind within 5 min; tail {a}{}{r} for diagnostics",
                    log_file().display()
                );
            }
        }
        Err(e) => {
            eprintln!("  {w}!{r} failed to spawn MTPLX: {}", e);
        }
    }
}

// Re-export MtplxSource so render_status can classify a git remote.
use crate::setup::MtplxSource;

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
    fn summarize_running_config_extracts_headline_values() {
        let cmd = "python -m mtplx.server.openai --model /m/path --host 127.0.0.1 --port 8088 \
            --depth 3 --generation-mode mtp --profile sustained \
            --verify-core linear-gdn-from-conv-tape --context-window 128000 \
            --temperature 0.6 --top-p 0.95 --reasoning-parser qwen3 \
            --model-id mtplx-qwen36-27b-optimized-speed";
        let rows = summarize_running_config(cmd);
        let map: std::collections::HashMap<_, _> = rows.into_iter().collect();
        assert_eq!(
            map.get("model id").unwrap(),
            "mtplx-qwen36-27b-optimized-speed"
        );
        assert_eq!(map.get("server").unwrap(), "127.0.0.1:8088");
        assert_eq!(map.get("decode").unwrap(), "mtp depth=3 profile=sustained");
        assert_eq!(map.get("verify core").unwrap(), "linear-gdn-from-conv-tape");
        assert_eq!(map.get("context").unwrap(), "128000 tokens");
        assert_eq!(map.get("sampling").unwrap(), "temp=0.6 top-p=0.95");
        assert_eq!(map.get("reasoning").unwrap(), "qwen3");
    }

    #[test]
    fn find_optimal_model_dir_handles_missing() {
        // The function should return None when path doesn't exist; we can't
        // assert it returns Some without depending on local state, so just
        // make sure it doesn't panic.
        let _ = find_optimal_model_dir();
    }

    /// Live-environment smoke test for the read-only detection helpers.
    /// Marked `#[ignore]` so it doesn't run in plain `cargo test` (which
    /// must remain side-effect-free). Run explicitly with:
    ///   cargo test -- --ignored live_detect_smoke --nocapture
    /// to see what hip is detecting on the current machine.
    #[test]
    #[ignore]
    fn live_detect_smoke() {
        println!("\n--- mtplx_runner live detection ---");
        println!("brew_python:        {:?}", find_brew_python());
        println!("model_dir:          {:?}", find_optimal_model_dir());
        println!("running_pid:        {:?}", running_pid());
        println!("running_venv_ver:   {:?}", running_venv_version());
        match running_command_line() {
            Some(cmd) => {
                println!("running_cmd: {}", cmd);
                let d = config_deltas(&cmd);
                println!("is_optimal: {}", d.is_optimal());
                if !d.is_optimal() {
                    println!("deltas:");
                    for x in &d.missing_or_wrong {
                        println!("  - {}", x);
                    }
                }
            }
            None => println!("running_cmd: (no MTPLX listening on :8088)"),
        }
        println!("log_file: {}", log_file().display());
        println!("--- end ---\n");
    }
}
