//! First-run setup wizard with interactive prompts.
//!
//! Decision tree:
//!   probe MTPLX server
//!     reachable?
//!       yes -> check for loaded model
//!         yes -> proceed
//!         no  -> print model-pull hint, exit
//!       no  -> ask: install MTPLX from daniel-farina/MTPLX fork? (Y/n)
//!         yes -> git clone + pip install -e .
//!                ask: start in background or new terminal? (B/n/skip)
//!                  bg     -> spawn detached, write PID + log paths
//!                  new    -> print exact command and exit
//!                  skip   -> print command for later
//!         no  -> print manual setup instructions, exit
//!
//! State: marker file at `~/.mlx-code/.welcomed` suppresses the wizard on
//! subsequent runs. `iris --setup` re-runs it regardless.

use anyhow::{anyhow, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::theme::{self, RESET};

const MARKER_PATH: &str = "~/.mlx-code/.welcomed";
// As of 2026-05-10: upstream merged all 8 of our perf PRs (the original
// 4 — #32, #33, #35, #37 — plus the second wave #38 TurboQuant fallback,
// #39 SessionBank 16K cliff fix, #40 entry-cap env, #41 postcommit
// prefix-reuse + miss-reason). Fresh installs can clone upstream main
// directly. The runtime env vars below (MTPLX_SESSION_BANK_*) still need
// to be set per-instance because they're env-overridable, not baked-in
// defaults.
// MTPLX repo + branch defaults. Override via env vars to test against
// a fork or feature branch:
//   HIP_MTPLX_REPO=https://github.com/me/MTPLX.git hip --setup
//   HIP_MTPLX_BRANCH=daniel/dev-stack hip --setup
// Useful when validating local fixes (e.g. our daniel-farina/MTPLX
// daniel/dev-stack branch carries fixes that aren't merged upstream
// yet) without committing to upstream main as the only install path.
const MTPLX_REPO_URL_DEFAULT: &str = "https://github.com/youssofal/MTPLX";
const MTPLX_BRANCH_DEFAULT: &str = "main";

fn mtplx_repo_url() -> String {
    std::env::var("HIP_MTPLX_REPO").unwrap_or_else(|_| MTPLX_REPO_URL_DEFAULT.to_string())
}
fn mtplx_branch() -> String {
    std::env::var("HIP_MTPLX_BRANCH").unwrap_or_else(|_| MTPLX_BRANCH_DEFAULT.to_string())
}
const MTPLX_DEFAULT_INSTALL_DIR: &str = "~/code/MTPLX";
const MTPLX_PID_FILE: &str = "~/.mlx-code/mtplx.pid";
const MTPLX_LOG_FILE: &str = "~/.mlx-code/mtplx.log";

// Default model the wizard ensures is downloaded before starting MTPLX.
// HuggingFace repo id; resolves to ~/.mtplx/models/Youssofal--<repo>/ via
// MTPLX's local cache convention.
const DEFAULT_MODEL_HF_ID: &str = "Youssofal/Qwen3.6-27B-MTPLX-Optimized-Speed";
const MODEL_CACHE_BASE: &str = "~/.mtplx/models";

/// True if this is the first invocation on this machine.
pub fn is_first_run() -> bool {
    !expand(MARKER_PATH).exists()
}

pub fn mark_welcomed() {
    let p = expand(MARKER_PATH);
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&p, b"1\n");
}

/// Probe MTPLX, walk the user through any missing pieces. Returns Ok(true)
/// if the model is up and the caller should proceed; Ok(false) if we printed
/// instructions and the caller should exit cleanly.
pub async fn run_wizard(url: &str) -> Result<bool> {
    let d = theme::dim();
    let a = theme::accent();
    let g = theme::good();
    let w = theme::warn();
    let r = RESET;

    eprintln!();
    eprintln!("{a}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━{r}");
    eprintln!("  {a}🌊 Welcome to hippo-code{r}");
    eprintln!("{a}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━{r}");
    eprintln!();
    eprintln!("  Running first-time setup. Re-run anytime with {a}hip --setup{r}.");
    eprintln!();

    eprint!("  {d}1/3{r} probing MTPLX server at {a}{url}{r}");
    let _ = std::io::stderr().flush();
    if let Err(e) = probe_server(url).await {
        eprintln!(" {w}NOT REACHABLE{r}");
        eprintln!("    {d}{e}{r}");
        eprintln!();
        let installed_and_running = offer_install_mtplx(url).await?;
        if installed_and_running {
            // Background-start succeeded and the server is up; mark welcomed
            // and let the caller proceed into chat mode.
            mark_welcomed();
        }
        return Ok(installed_and_running);
    }
    eprintln!(" {g}OK{r}");

    eprint!("  {d}2/3{r} listing models");
    let _ = std::io::stderr().flush();
    let models = list_models(url).await.unwrap_or_default();
    if models.is_empty() {
        eprintln!(" {w}NO MODELS{r}");
        eprintln!();
        eprintln!("  {w}!{r} MTPLX is up but no models are loaded.");
        eprintln!("    Pull a quantized Qwen3-27B model:");
        eprintln!("      {a}huggingface-cli download mlx-community/Qwen3-27B-Instruct-4bit{r}");
        eprintln!("    Then load it via the MTPLX dashboard or CLI.");
        eprintln!();
        return Ok(false);
    }
    eprintln!(" {g}OK{r}  ({} model(s))", models.len());

    eprintln!("  {d}3/3{r} ready");
    eprintln!();
    eprintln!("  models:");
    for (i, m) in models.iter().take(5).enumerate() {
        let mark = if i == 0 {
            format!("{g}*{r}")
        } else {
            " ".to_string()
        };
        eprintln!("    {} {a}{m}{r}", mark, a = a, r = r, m = m);
    }
    if models.len() > 5 {
        eprintln!("    {d}... +{} more{r}", models.len() - 5);
    }
    eprintln!();
    eprintln!("  {g}✓{r} run {a}hip{r} to chat or {a}hip 'your prompt'{r}");
    eprintln!();
    mark_welcomed();
    Ok(true)
}

/// Interactive: offer to clone+install MTPLX from daniel-farina/MTPLX, then
/// optionally start it. Always returns Ok(()).
/// Interactive: clone+install+optionally start MTPLX. Returns Ok(true) when
/// the server is up and responding so the caller can proceed into chat
/// mode without re-launching `hip`. Returns Ok(false) when the user picked
/// a non-blocking option (new-terminal / skip) or background-start failed
/// to come up within the timeout - in those cases the caller should exit
/// and the user re-runs `hip` after starting MTPLX themselves.
async fn offer_install_mtplx(url: &str) -> Result<bool> {
    let d = theme::dim();
    let a = theme::accent();
    let g = theme::good();
    let w = theme::warn();
    let r = RESET;

    eprintln!("  {w}MTPLX server isn't running.{r}");
    eprintln!();
    eprintln!("  {a}hippo-code{r} talks to a local MTPLX server. We can install our fork");
    eprintln!(
        "  ({a}{}{r}, branch {a}{}{r})",
        mtplx_repo_url(), mtplx_branch()
    );
    eprintln!("  and have it ready in a few steps.");
    eprintln!();

    if !ask_yes_no("install MTPLX now?", true) {
        print_manual_setup();
        return Ok(false);
    }

    // Verify build/runtime prerequisites BEFORE we start cloning multi-MB
    // repos. Auto-install what we safely can (pip via ensurepip); for
    // heavier tools (git/python3) print platform-specific install commands.
    if !ensure_prerequisites() {
        return Ok(false);
    }

    let install_dir = expand(MTPLX_DEFAULT_INSTALL_DIR);
    eprintln!();
    eprintln!("  {a}install dir{r}: {}", install_dir.display());
    if install_dir.exists() {
        eprintln!(
            "  {w}!{r} {} already exists; will skip clone and reuse.",
            install_dir.display()
        );
    } else {
        if let Some(parent) = install_dir.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        eprintln!();
        eprintln!("  {d}cloning...{r}");
        let repo_url = mtplx_repo_url();
        let branch = mtplx_branch();
        let status = std::process::Command::new("git")
            .args([
                "clone",
                "--depth",
                "1",
                "--branch",
                &branch,
                &repo_url,
                install_dir.to_string_lossy().as_ref(),
            ])
            .status();
        match status {
            Ok(s) if s.success() => eprintln!("  {g}✓{r} cloned"),
            Ok(s) => {
                eprintln!(
                    "  {w}!{r} git clone exited {}; falling back to manual instructions.",
                    s
                );
                print_manual_setup();
                return Ok(false);
            }
            Err(e) => {
                eprintln!("  {w}!{r} git clone failed: {} (is git installed?)", e);
                print_manual_setup();
                return Ok(false);
            }
        }
    }

    // Create a virtualenv inside the MTPLX checkout and install into it.
    // Modern Homebrew Python (and Debian's python3) refuse system-wide pip
    // installs (PEP 668 / "externally-managed-environment"); a venv sidesteps
    // that without --break-system-packages. We also remember the venv's
    // python so the later start step uses it.
    let venv_python = match ensure_venv(&install_dir) {
        Some(p) => p,
        None => return Ok(false),
    };

    // Pip can be noisy (version-check banners, build progress lines) and the
    // output bleeds into the wizard prompts. Capture everything to a log so
    // the wizard UI stays clean; surface the log path on failure.
    let install_log = expand("~/.mlx-code/mtplx-install.log");
    if let Some(parent) = install_log.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    eprintln!();
    eprintln!(
        "  {d}installing python dependencies into .venv (logging to {a}{}{d})...{r}",
        install_log.display()
    );
    let log_handle = match std::fs::File::create(&install_log) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("  {w}!{r} cannot create install log: {}", e);
            return Ok(false);
        }
    };
    let log_err = log_handle.try_clone().ok();
    let status = std::process::Command::new(&venv_python)
        .args([
            "-m",
            "pip",
            "install",
            "--disable-pip-version-check",
            "--quiet",
            "-e",
            ".",
        ])
        .current_dir(&install_dir)
        .stdout(std::process::Stdio::from(log_handle))
        .stderr(
            log_err
                .map(std::process::Stdio::from)
                .unwrap_or(std::process::Stdio::null()),
        )
        .status();
    match status {
        Ok(s) if s.success() => eprintln!("  {g}✓{r} installed"),
        Ok(s) => {
            eprintln!("  {w}!{r} pip install (in venv) exited {}", s);
            // Surface the actual pip error inline so the user doesn't have
            // to `cat` the log file to see why it failed (e.g. Python
            // version mismatch, missing system header, network error).
            print_log_tail(&install_log, 30);
            eprintln!();
            eprintln!("    Full log: {a}{}{r}", install_log.display());
            eprintln!("    To retry manually:");
            eprintln!(
                "      {a}cd {} && .venv/bin/python -m pip install -e .{r}",
                install_dir.display()
            );
            return Ok(false);
        }
        Err(e) => {
            eprintln!("  {w}!{r} could not invoke venv python: {}", e);
            return Ok(false);
        }
    }

    // Make sure the model weights are downloaded before we try to start
    // the server. MTPLX's load step will hang for a while pulling weights
    // on first run; doing it explicitly here lets the user see a real
    // progress bar from huggingface-cli rather than a silent stall.
    if !ensure_model_weights(&venv_python).await {
        return Ok(false);
    }

    eprintln!();
    let mode = ask_choice(
        "start MTPLX",
        &[
            "[B]ackground (logs to file, runs detached)",
            "[N]ew terminal window (you run it)",
            "[s]kip",
        ],
        'b',
    );
    match mode {
        'b' => {
            // Spawn detached, then wait for /v1/models to respond. If it
            // comes up, we return true so the caller proceeds into chat
            // mode without exiting. If the timeout hits, we return false
            // and the user can run `hip --setup` to recheck.
            start_mtplx_background_and_wait(&install_dir, url).await
        }
        'n' => {
            let port = port_from_url(url).unwrap_or(8088);
            eprintln!();
            eprintln!("  Open a new terminal and run:");
            eprintln!("    {a}cd {}{r}", install_dir.display());
            eprintln!(
                "    {a}.venv/bin/mtplx quickstart --port {} --model {}{r}",
                port, DEFAULT_MODEL_HF_ID
            );
            eprintln!();
            eprintln!("  Then re-run {a}hip{r}.");
            Ok(false)
        }
        _ => {
            let port = port_from_url(url).unwrap_or(8088);
            eprintln!();
            eprintln!("  Skipping start. When you're ready:");
            eprintln!("    {a}cd {}{r}", install_dir.display());
            eprintln!(
                "    {a}.venv/bin/mtplx quickstart --port {} --model {}{r}",
                port, DEFAULT_MODEL_HF_ID
            );
            Ok(false)
        }
    }
}

/// Parse the port number out of a URL like `http://127.0.0.1:8088/v1`.
/// Returns None if the URL doesn't have an explicit port.
fn port_from_url(url: &str) -> Option<u16> {
    let after_scheme = url.split("://").nth(1)?;
    let host_port = after_scheme.split('/').next()?;
    host_port.split(':').nth(1)?.parse().ok()
}

/// Spawn MTPLX in the background, then poll the configured URL until the
/// server responds (model load can take 30-90s for a cold start). Returns
/// Ok(true) if the server came up within the timeout, Ok(false) if it
/// didn't (the user should check ~/.mlx-code/mtplx.log and re-run).
async fn start_mtplx_background_and_wait(install_dir: &Path, url: &str) -> Result<bool> {
    let d = theme::dim();
    let a = theme::accent();
    let g = theme::good();
    let w = theme::warn();
    let r = RESET;

    let log_path = expand(MTPLX_LOG_FILE);
    let pid_path = expand(MTPLX_PID_FILE);
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let log_file = match std::fs::File::create(&log_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "  {w}!{r} cannot create log file {}: {}",
                log_path.display(),
                e
            );
            return Ok(false);
        }
    };
    let log_err = match log_file.try_clone() {
        Ok(f) => f,
        Err(e) => {
            eprintln!("  {w}!{r} dup log fd failed: {}", e);
            return Ok(false);
        }
    };

    // Prefer the venv's `mtplx` console script (installed by `pip install -e .`).
    // Fall back to running the openai server module directly via the venv
    // python if the console script isn't on disk for some reason.
    let venv_bin = install_dir.join(".venv").join("bin");
    let mtplx_bin = venv_bin.join("mtplx");
    let venv_python = venv_bin.join("python");

    let port = port_from_url(url).unwrap_or(8088);
    let port_str = port.to_string();

    use std::os::unix::process::CommandExt;
    let mut cmd = if mtplx_bin.exists() {
        let mut c = std::process::Command::new(&mtplx_bin);
        c.args([
            "quickstart",
            "--port",
            &port_str,
            "--model",
            DEFAULT_MODEL_HF_ID,
            "--yes",
        ]);
        c
    } else if venv_python.exists() {
        // Last-resort path: invoke the server module directly. Note: this is
        // `mtplx.server.openai` (the actual module with main()); plain
        // `mtplx.server` is a package without __main__.py and won't run.
        let mut c = std::process::Command::new(&venv_python);
        c.args([
            "-m",
            "mtplx.server.openai",
            "--port",
            &port_str,
            "--model",
            DEFAULT_MODEL_HF_ID,
        ]);
        c
    } else {
        eprintln!(
            "  {w}!{r} no venv at {}; cannot start MTPLX",
            venv_bin.display()
        );
        return Ok(false);
    };
    // Ship the SessionBank cap overrides every fresh install needs to avoid
    // hitting the small-default eviction wall on >16K-token agent sessions.
    // These match the values the patched fork's run script uses (validated
    // against >50K-token hippo-code conversations).
    cmd.env("MTPLX_SESSION_BANK_PER_SESSION_BYTES", "16G")
        .env("MTPLX_SESSION_BANK_MAX_BYTES", "32G")
        .env("MTPLX_SESSION_BANK_MAX_ENTRIES", "24")
        .current_dir(install_dir)
        .stdout(std::process::Stdio::from(log_file))
        .stderr(std::process::Stdio::from(log_err))
        .stdin(std::process::Stdio::null());
    let child = unsafe {
        cmd.pre_exec(|| {
            // Become a new session leader so this child survives our exit.
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        })
        .spawn()
    };
    let pid = match child {
        Ok(c) => {
            let pid = c.id();
            let _ = std::fs::write(&pid_path, format!("{}\n", pid));
            pid
        }
        Err(e) => {
            eprintln!("  {w}!{r} failed to spawn MTPLX: {}", e);
            return Ok(false);
        }
    };

    eprintln!();
    eprintln!("  {g}✓{r} MTPLX spawned in background");
    eprintln!("    {d}pid:{r}  {a}{pid}{r}  ({})", pid_path.display());
    eprintln!("    {d}logs:{r} {a}tail -f {}{r}", log_path.display());
    eprintln!("    {d}stop:{r} {a}kill {pid}{r}");
    eprintln!();

    // Poll /v1/models until the server responds. Cold model load on a 27B
    // can take ~30-90s; we give 5 min of budget. Alongside the spinner we
    // tail the MTPLX log so the user can see what the server is actually
    // doing (loading weights, binding port, running warmup) and recognize
    // a stuck state vs slow progress.
    let timeout = std::time::Duration::from_secs(300);
    let poll_interval = std::time::Duration::from_millis(800);
    let started_at = std::time::Instant::now();

    eprintln!(
        "  {d}waiting for MTPLX to bind {a}{url}{d} (model load can take 30-90s on a 27B){r}"
    );
    eprintln!("  {d}live log tail of {a}{}{d}:{r}", log_path.display());
    eprintln!();
    let spinner_chars = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let mut tick: usize = 0;
    let mut last_log_line = String::from("(waiting for first log line...)");
    let mut last_log_size: u64 = 0;
    let mut last_log_change = std::time::Instant::now();

    loop {
        if probe_server(url).await.is_ok() {
            // Final clear of the spinner row.
            eprint!("\r\x1b[2K");
            eprintln!(
                "  {g}✓{r} MTPLX is responding ({:.1}s)",
                started_at.elapsed().as_secs_f64()
            );
            return Ok(true);
        }

        // Refresh log tail. Track size so we can detect stuck state.
        if let Ok(meta) = std::fs::metadata(&log_path) {
            let size = meta.len();
            if size != last_log_size {
                last_log_size = size;
                last_log_change = std::time::Instant::now();
                if let Some(line) = tail_last_line(&log_path) {
                    last_log_line = line;
                }
            }
        }

        if started_at.elapsed() > timeout {
            eprint!("\r\x1b[2K");
            eprintln!(
                "  {w}!{r} MTPLX didn't respond within {}s",
                timeout.as_secs()
            );
            eprintln!("    Last log line: {d}{}{r}", last_log_line);
            eprintln!("    Full log:      {a}tail -50 {}{r}", log_path.display());
            eprintln!("    The process may still be loading. Try {a}hip --setup{r} in a moment.");
            return Ok(false);
        }

        // Stuck-detection hint: log idle for >60s and we've waited >90s.
        let stuck_hint = if last_log_change.elapsed() > std::time::Duration::from_secs(60)
            && started_at.elapsed() > std::time::Duration::from_secs(90)
        {
            format!(
                "  {w}(log idle {}s){r}",
                last_log_change.elapsed().as_secs()
            )
        } else {
            String::new()
        };

        // Render the spinner + truncated log line on a single row that
        // doesn't wrap. Use ESC[2K to clear-line each tick so a longer
        // previous line doesn't leave trailing junk after a shorter one.
        let term_width = terminal_size::terminal_size()
            .map(|(w, _)| w.0 as usize)
            .unwrap_or(120);
        // Reserve characters for: "  ⠋ (123s) " plus the stuck-hint visible width.
        let prefix_visible = 2 + 1 + 1 + 1 + 1 + 4 + 1; // "  ⠋ (123s) "
        let stuck_visible = strip_ansi_len_local(&stuck_hint);
        let log_budget = term_width
            .saturating_sub(prefix_visible)
            .saturating_sub(stuck_visible)
            .saturating_sub(2);
        let truncated_log = truncate_log_line(&last_log_line, log_budget);

        eprint!(
            "\r\x1b[2K  {a}{}{r} {d}({:.0}s){r} {d}{}{r}{}",
            spinner_chars[tick % spinner_chars.len()],
            started_at.elapsed().as_secs_f64(),
            truncated_log,
            stuck_hint,
        );
        let _ = std::io::stderr().flush();
        tick += 1;
        tokio::time::sleep(poll_interval).await;
    }
}

/// Print the last `n` lines of a captured log file inline, prefixed with
/// a dim "│" so the user sees the real pip / hf-cli / mtplx error without
/// having to `cat` the log themselves. Silent on read failure.
fn print_log_tail(path: &Path, n: usize) {
    let d = theme::dim();
    let r = RESET;
    let body = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return,
    };
    let lines: Vec<&str> = body.lines().collect();
    if lines.is_empty() {
        return;
    }
    let start = lines.len().saturating_sub(n);
    let shown = &lines[start..];
    eprintln!();
    eprintln!(
        "    {d}─── last {} line(s) of {} ───{r}",
        shown.len(),
        path.display()
    );
    for line in shown {
        eprintln!("    {d}│{r} {}", line);
    }
    eprintln!("    {d}─────────────────────────────────{r}");
}

/// Read the last non-empty line of a file. Used by the wait spinner to
/// surface MTPLX's most recent log message.
fn tail_last_line(path: &Path) -> Option<String> {
    let body = std::fs::read_to_string(path).ok()?;
    body.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|s| s.to_string())
}

/// Truncate a log line to `max_chars`, preserving the tail (which is where
/// the most informative progress lives, e.g. "loading weights: shard-7/8").
fn truncate_log_line(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    let head = max_chars.saturating_sub(3);
    let skip = count - head;
    let mut out = String::with_capacity(max_chars);
    out.push_str("...");
    out.push_str(&s.chars().skip(skip).collect::<String>());
    out
}

/// Cheap ANSI escape stripper for visible-width math.
fn strip_ansi_len_local(s: &str) -> usize {
    let mut out = 0usize;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c2 in chars.by_ref() {
                if c2 == 'm' {
                    break;
                }
            }
        } else {
            out += 1;
        }
    }
    out
}

/// Check git, python3, and pip. Auto-install pip via `python3 -m ensurepip`
/// when missing (low-risk built-in path). For git / python3, print
/// platform-specific install commands and return false so the caller can
/// abort cleanly. Returns true when all three are available (or were just
/// successfully installed).
fn ensure_prerequisites() -> bool {
    let d = theme::dim();
    let a = theme::accent();
    let g = theme::good();
    let w = theme::warn();
    let r = RESET;

    eprintln!();
    eprintln!("  {d}checking prerequisites:{r}");

    let mut missing: Vec<&'static str> = Vec::new();

    // git
    eprint!("    {d}git    {r}");
    let _ = std::io::stderr().flush();
    if which_exists("git") {
        eprintln!("{g}OK{r}");
    } else {
        eprintln!("{w}MISSING{r}");
        missing.push("git");
    }

    // python3 - MTPLX requires >=3.11. We don't just check that *some* python
    // is on PATH; we check that one of them is new enough, otherwise pip
    // install -e . will fail late with "requires a different Python: 3.10.x".
    eprint!("    {d}python3{r}");
    let _ = std::io::stderr().flush();
    let mut py311 = find_compatible_python();
    match &py311 {
        Some(name) => {
            if let Some((maj, min)) = python_version_of(name) {
                eprintln!(" {g}OK{r} {d}({} - {}.{}.x){r}", name, maj, min);
            } else {
                eprintln!(" {g}OK{r} {d}({}){r}", name);
            }
        }
        None => {
            // Python is installed but too old, OR no python at all.
            let any_python = which_exists("python3") || which_exists("python");
            if any_python {
                let cur = python_version_of(python_cmd())
                    .map(|(a, b)| format!("{}.{}", a, b))
                    .unwrap_or_else(|| "unknown".into());
                eprintln!(
                    " {w}TOO OLD{r} {d}(found {} {}, MTPLX needs >=3.{}){r}",
                    python_cmd(),
                    cur,
                    MTPLX_MIN_PY_MINOR
                );
                if cfg!(target_os = "macos") && which_exists("brew") {
                    eprintln!();
                    if ask_yes_no(
                        "install Python 3.12 via Homebrew now? (`brew install python@3.12`)",
                        true,
                    ) {
                        py311 = try_brew_install_python();
                    }
                }
                if py311.is_none() {
                    eprintln!();
                    eprintln!(
                        "    {d}MTPLX needs Python >=3.{}. Options:{r}",
                        MTPLX_MIN_PY_MINOR
                    );
                    if cfg!(target_os = "macos") {
                        eprintln!("      {a}brew install python@3.12{r}");
                        eprintln!(
                            "      {a}curl -LsSf https://astral.sh/uv/install.sh | sh && uv python install 3.12{r}"
                        );
                    } else {
                        eprintln!("      {a}sudo apt-get install -y python3.12 python3.12-venv{r}");
                        eprintln!("      {a}sudo dnf install -y python3.12{r}");
                        eprintln!(
                            "      {a}curl -LsSf https://astral.sh/uv/install.sh | sh && uv python install 3.12{r}"
                        );
                    }
                    return false;
                }
            } else {
                eprintln!(" {w}MISSING{r}");
                missing.push("python3");
            }
        }
    }

    // pip - check via `python3 -m pip --version` since `pip` may not be on PATH
    // even when it's installed as a python module
    eprint!("    {d}pip    {r}");
    let _ = std::io::stderr().flush();
    let pip_ok = std::process::Command::new(python_cmd())
        .args(["-m", "pip", "--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if pip_ok {
        eprintln!("{g}OK{r}");
    } else if which_exists("python3") || which_exists("python") {
        // python is present but pip isn't; offer to bootstrap via ensurepip
        eprintln!("{w}MISSING{r}");
        eprintln!();
        eprintln!("    {d}python is installed but pip isn't.{r}");
        if !ask_yes_no("install pip now via `python -m ensurepip --upgrade`?", true) {
            eprintln!();
            eprintln!("  {w}!{r} pip is required to install MTPLX. Aborting.");
            return false;
        }
        eprintln!("    {d}running ensurepip...{r}");
        let status = std::process::Command::new(python_cmd())
            .args(["-m", "ensurepip", "--upgrade"])
            .status();
        let installed = match status {
            Ok(s) if s.success() => true,
            Ok(s) => {
                eprintln!("    {w}!{r} ensurepip exited {}", s);
                false
            }
            Err(e) => {
                eprintln!("    {w}!{r} ensurepip failed: {}", e);
                false
            }
        };
        if !installed {
            // Fall back: get-pip.py from the official source
            eprintln!();
            eprintln!("    Try the bootstrap installer:");
            eprintln!(
                "      {a}curl -sSL https://bootstrap.pypa.io/get-pip.py | {} -{r}",
                python_cmd()
            );
            return false;
        }
        eprintln!("    {g}✓{r} pip installed");
    } else {
        eprintln!("{w}MISSING (python missing too){r}");
        missing.push("pip");
    }

    if !missing.is_empty() {
        eprintln!();
        eprintln!("  {w}!{r} cannot proceed - missing: {}", missing.join(", "));
        eprintln!();
        print_install_hints(&missing);
        return false;
    }

    true
}

fn which_exists(cmd: &str) -> bool {
    std::process::Command::new("sh")
        .args(["-c", &format!("command -v {} >/dev/null 2>&1", cmd)])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn python_cmd() -> &'static str {
    if which_exists("python3") {
        "python3"
    } else {
        "python"
    }
}

/// Minimum Python version MTPLX accepts (matches the `requires-python = ">=3.11"`
/// in MTPLX's pyproject.toml). When this is bumped upstream, bump it here too
/// so the wizard's preflight matches what pip will accept.
const MTPLX_MIN_PY_MINOR: u32 = 11;

/// Run `<binary> --version`, parse output like "Python 3.12.4", return (major, minor).
/// Returns None on any failure (binary missing, garbled output, parse error).
fn python_version_of(binary: &str) -> Option<(u32, u32)> {
    let out = std::process::Command::new(binary)
        .arg("--version")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // `python --version` writes to stdout on 3.4+, but older versions wrote to
    // stderr. Concatenate both to be safe.
    let mut s = String::from_utf8_lossy(&out.stdout).to_string();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    let s = s.trim();
    let rest = s.strip_prefix("Python ")?;
    let mut parts = rest.split('.');
    let maj: u32 = parts.next()?.parse().ok()?;
    let min: u32 = parts.next()?.split_whitespace().next()?.parse().ok()?;
    Some((maj, min))
}

/// Find a Python ≥ 3.{MTPLX_MIN_PY_MINOR} on PATH. Probes minor-versioned
/// binaries first (`python3.13`, `python3.12`, `python3.11`) since those are
/// stable identifiers, then falls back to whatever `python3` / `python` resolve
/// to and version-checks them.
///
/// Returns the binary name (e.g. `"python3.12"`) or None if nothing on PATH
/// satisfies the minimum.
fn find_compatible_python() -> Option<String> {
    // Probe newest-to-oldest so we prefer the freshest interpreter when the
    // user has multiple installed (common with `brew install python@3.11`
    // alongside an older system python). Cap at 3.20 so the loop terminates
    // even if Python 4 ships before someone updates this.
    for minor in (MTPLX_MIN_PY_MINOR..=20).rev() {
        let name = format!("python3.{}", minor);
        if which_exists(&name) {
            if let Some((3, m)) = python_version_of(&name) {
                if m >= MTPLX_MIN_PY_MINOR {
                    return Some(name);
                }
            }
        }
    }
    // Fallback: maybe `python3` or `python` is fresh enough but not exposed
    // under a minor-versioned name (some distros do this).
    for cand in ["python3", "python"] {
        if which_exists(cand) {
            if let Some((3, m)) = python_version_of(cand) {
                if m >= MTPLX_MIN_PY_MINOR {
                    return Some(cand.to_string());
                }
            }
        }
    }
    None
}

/// Try to install Python ≥ 3.11 via `brew install python@3.12`. macOS only.
/// Returns Some(binary_name) on success, None on failure or non-macos.
fn try_brew_install_python() -> Option<String> {
    if !cfg!(target_os = "macos") || !which_exists("brew") {
        return None;
    }
    let d = theme::dim();
    let g = theme::good();
    let w = theme::warn();
    let r = RESET;
    eprintln!();
    eprintln!("  {d}running `brew install python@3.12` (this may take a few minutes)...{r}");
    let status = std::process::Command::new("brew")
        .args(["install", "python@3.12"])
        .status();
    match status {
        Ok(s) if s.success() => {
            eprintln!("  {g}✓{r} python@3.12 installed");
            // brew installs the keg under /opt/homebrew/opt/python@3.12 on
            // arm64 (or /usr/local on x86_64) and links python3.12 onto PATH.
            // Re-probe to confirm it's actually visible.
            find_compatible_python()
        }
        Ok(s) => {
            eprintln!("  {w}!{r} brew install exited {}", s);
            None
        }
        Err(e) => {
            eprintln!("  {w}!{r} could not invoke brew: {}", e);
            None
        }
    }
}

/// Ensure `<install_dir>/.venv` exists with a working `bin/python`. Creates
/// it via `python3 -m venv` if missing. Returns the path to the venv's
/// python executable, or None if creation failed.
///
/// Why a venv: modern Homebrew Python and Debian's python3 reject system-wide
/// pip installs (PEP 668). A venv is the portable way to install MTPLX
/// without --break-system-packages or sudo.
/// Ensure the default model is downloaded into MTPLX's cache. If the
/// canonical `<cache>/Youssofal--<model>/config.json` file is present we
/// assume the weights are there. Otherwise we run
/// `<venv>/bin/huggingface-cli download <repo> --local-dir <dir>`,
/// letting hf-cli's progress bars stream straight to the user. Returns
/// false if the download fails or hf-cli isn't installed.
async fn ensure_model_weights(venv_python: &Path) -> bool {
    let d = theme::dim();
    let a = theme::accent();
    let g = theme::good();
    let w = theme::warn();
    let r = RESET;

    // Convert "Youssofal/Qwen3.6-27B-MTPLX-Optimized-Speed" to the local
    // cache directory name MTPLX expects ("Youssofal--Qwen3.6-..."). MTPLX
    // does this transform internally; we mirror it for the existence check.
    let model_dir_name = DEFAULT_MODEL_HF_ID.replace('/', "--");
    let cache_root = expand(MODEL_CACHE_BASE);
    let model_dir = cache_root.join(&model_dir_name);
    let config_marker = model_dir.join("config.json");

    if config_marker.exists() {
        eprintln!();
        eprintln!(
            "  {g}✓{r} model already downloaded at {a}{}{r}",
            model_dir.display()
        );
        return true;
    }

    eprintln!();
    eprintln!(
        "  {d}model weights missing at {a}{}{d} - downloading...{r}",
        model_dir.display()
    );
    eprintln!(
        "  {d}this is a multi-GB download (~16GB for the 4-bit qwen3.6-27b); progress shown below.{r}"
    );
    eprintln!();

    if let Some(parent) = model_dir.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!(
                "  {w}!{r} cannot create cache root {}: {}",
                parent.display(),
                e
            );
            return false;
        }
    }

    // huggingface_hub's CLI surface has churned across versions:
    //   - older: `python -m huggingface_hub.commands.huggingface_cli` (gone)
    //   - middle: `<venv>/bin/huggingface-cli` (deprecated; in newest
    //     releases this script just prints a banner and exits non-zero)
    //   - newest: `<venv>/bin/hf` (the new entry point)
    //
    // We try `hf` first, then `huggingface-cli`, then a `python -c` that
    // calls `snapshot_download` directly (works on every version of
    // huggingface_hub since 0.10 - tqdm progress streams by default).
    let venv_bin = venv_python.parent().map(|p| p.to_path_buf());
    let hf_new = venv_bin.as_ref().map(|b| b.join("hf"));
    let hf_old = venv_bin.as_ref().map(|b| b.join("huggingface-cli"));

    let status = if let Some(cli) = hf_new.as_ref().filter(|p| p.exists()) {
        std::process::Command::new(cli)
            .args([
                "download",
                DEFAULT_MODEL_HF_ID,
                "--local-dir",
                model_dir.to_string_lossy().as_ref(),
            ])
            .status()
    } else if let Some(cli) = hf_old.as_ref().filter(|p| p.exists()) {
        // The deprecated wrapper exits non-zero on newest huggingface_hub
        // releases, so this branch effectively only succeeds on older ones.
        // It's still worth trying for users who pinned an older version.
        std::process::Command::new(cli)
            .args([
                "download",
                DEFAULT_MODEL_HF_ID,
                "--local-dir",
                model_dir.to_string_lossy().as_ref(),
            ])
            .status()
    } else {
        // Last resort: drive snapshot_download from a one-liner.
        let py_snippet = format!(
            "from huggingface_hub import snapshot_download; \
             snapshot_download(repo_id={:?}, local_dir={:?})",
            DEFAULT_MODEL_HF_ID,
            model_dir.to_string_lossy().as_ref(),
        );
        std::process::Command::new(venv_python)
            .args(["-c", &py_snippet])
            .status()
    };

    // If the first attempt failed AND we hit the deprecated `huggingface-cli`
    // wrapper, retry via the snapshot_download fallback. This catches the
    // exact case the user hit: hf binary not on PATH (older venv) but
    // `huggingface-cli` exists and only prints a deprecation banner.
    let status = match status {
        Ok(s) if !s.success() && hf_new.as_ref().is_none_or(|p| !p.exists()) => {
            eprintln!("  {d}retrying via snapshot_download (CLI shim was deprecated){r}");
            let py_snippet = format!(
                "from huggingface_hub import snapshot_download; \
                 snapshot_download(repo_id={:?}, local_dir={:?})",
                DEFAULT_MODEL_HF_ID,
                model_dir.to_string_lossy().as_ref(),
            );
            std::process::Command::new(venv_python)
                .args(["-c", &py_snippet])
                .status()
        }
        other => other,
    };

    match status {
        Ok(s) if s.success() => {
            if config_marker.exists() {
                eprintln!();
                eprintln!("  {g}✓{r} model downloaded");
                true
            } else {
                eprintln!(
                    "  {w}!{r} download succeeded but {} is missing",
                    config_marker.display()
                );
                false
            }
        }
        Ok(s) => {
            eprintln!("  {w}!{r} model download exited {}", s);
            eprintln!(
                "    Try manually: {a}<venv>/bin/huggingface-cli download {} --local-dir {}{r}",
                DEFAULT_MODEL_HF_ID,
                model_dir.display()
            );
            false
        }
        Err(e) => {
            eprintln!("  {w}!{r} could not invoke huggingface downloader: {}", e);
            eprintln!(
                "    The MTPLX install should have brought it in; try {a}<venv>/bin/python -m pip install huggingface_hub{r}"
            );
            false
        }
    }
}

fn ensure_venv(install_dir: &Path) -> Option<PathBuf> {
    let d = theme::dim();
    let g = theme::good();
    let w = theme::warn();
    let a = theme::accent();
    let r = RESET;

    let venv_dir = install_dir.join(".venv");
    let venv_python = venv_dir.join("bin").join("python");

    // The venv interpreter is locked to whatever python created it. If a
    // previous run on this machine created a 3.10.x venv (e.g. from before
    // we required 3.11+), reusing it just hits the same `requires-python`
    // error during pip install. Detect that and offer to rebuild.
    if venv_python.exists() {
        let venv_v = python_version_of(venv_python.to_string_lossy().as_ref());
        let too_old = matches!(venv_v, Some((3, m)) if m < MTPLX_MIN_PY_MINOR);
        if too_old {
            let (vmaj, vmin) = venv_v.unwrap();
            eprintln!();
            eprintln!(
                "  {w}!{r} existing venv at {a}{}{r} uses Python {}.{} (MTPLX needs >=3.{})",
                venv_dir.display(),
                vmaj,
                vmin,
                MTPLX_MIN_PY_MINOR
            );
            if ask_yes_no("recreate the venv with a newer Python?", true) {
                if let Err(e) = std::fs::remove_dir_all(&venv_dir) {
                    eprintln!("  {w}!{r} could not remove {}: {}", venv_dir.display(), e);
                    return None;
                }
                eprintln!("  {d}removed stale venv; creating a fresh one...{r}");
                // Fall through to creation below.
            } else {
                eprintln!("  {w}!{r} keeping the old venv; pip install will likely fail.");
                return Some(venv_python);
            }
        } else {
            eprintln!();
            eprintln!(
                "  {d}venv exists at {a}{}{d}; reusing{r}",
                venv_dir.display()
            );
            return Some(venv_python);
        }
    }

    // Pick the freshest Python ≥ MTPLX_MIN_PY_MINOR for the venv base, falling
    // back to plain `python_cmd()` only if nothing fresher is on PATH (in
    // which case the prerequisites check would have already prompted to fix
    // it; we still try, in case the user said "no" but wants the venv anyway).
    let py_base = find_compatible_python().unwrap_or_else(|| python_cmd().to_string());

    eprintln!();
    eprintln!(
        "  {d}creating virtualenv at {a}{}{d} using {a}{}{d}...{r}",
        venv_dir.display(),
        py_base
    );
    let status = std::process::Command::new(&py_base)
        .args(["-m", "venv", ".venv"])
        .current_dir(install_dir)
        .status();
    match status {
        Ok(s) if s.success() => {
            if !venv_python.exists() {
                eprintln!(
                    "  {w}!{r} venv created but {a}{}{r} is missing - aborting",
                    venv_python.display()
                );
                return None;
            }
            eprintln!("  {g}✓{r} venv ready");
            Some(venv_python)
        }
        Ok(s) => {
            eprintln!("  {w}!{r} `{} -m venv .venv` exited {}", py_base, s);
            eprintln!(
                "    On Debian/Ubuntu you may need: {a}sudo apt-get install -y python3-venv{r}"
            );
            eprintln!(
                "    On macOS Homebrew Python this is usually pre-installed; check {a}{} -m venv --help{r}",
                py_base
            );
            None
        }
        Err(e) => {
            eprintln!("  {w}!{r} could not invoke {} -m venv: {}", py_base, e);
            None
        }
    }
}

fn print_install_hints(missing: &[&str]) {
    let a = theme::accent();
    let r = RESET;
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "other"
    };
    for m in missing {
        match (*m, os) {
            ("git", "macos") => {
                eprintln!("  install git:");
                eprintln!("    {a}xcode-select --install{r}            # built-in");
                eprintln!("    {a}brew install git{r}                  # if you have Homebrew");
            }
            ("git", "linux") => {
                eprintln!("  install git:");
                eprintln!("    {a}sudo apt-get install -y git{r}       # debian/ubuntu");
                eprintln!("    {a}sudo dnf install -y git{r}           # fedora/rhel");
                eprintln!("    {a}sudo pacman -S git{r}                # arch");
            }
            ("python3", "macos") => {
                eprintln!("  install python3:");
                eprintln!(
                    "    {a}brew install python@3.12{r}          # via Homebrew (recommended)"
                );
                eprintln!("    {a}xcode-select --install{r}            # ships an older python3");
            }
            ("python3", "linux") => {
                eprintln!("  install python3:");
                eprintln!("    {a}sudo apt-get install -y python3 python3-pip python3-venv{r}");
                eprintln!("    {a}sudo dnf install -y python3 python3-pip{r}");
            }
            ("pip", _) => {
                eprintln!("  install pip:");
                eprintln!("    {a}python3 -m ensurepip --upgrade{r}");
                eprintln!("    {a}curl -sSL https://bootstrap.pypa.io/get-pip.py | python3 -{r}");
            }
            _ => {
                eprintln!("  install {m}: see your system's package manager");
            }
        }
    }
}

fn print_manual_setup() {
    let a = theme::accent();
    let r = RESET;
    eprintln!();
    eprintln!("  Manual setup:");
    eprintln!(
        "    {a}git clone -b {} {} ~/code/MTPLX{r}",
        mtplx_branch(),
        mtplx_repo_url()
    );
    eprintln!("    {a}cd ~/code/MTPLX && python -m venv .venv && .venv/bin/pip install -e .{r}");
    eprintln!(
        "    {a}.venv/bin/mtplx quickstart --port 8088 --model {}{r}",
        DEFAULT_MODEL_HF_ID
    );
    eprintln!();
    eprintln!("  Then re-run {a}hip{r}.");
}

fn ask_yes_no(question: &str, default_yes: bool) -> bool {
    use std::io::IsTerminal;
    let a = theme::accent();
    let r = RESET;
    let suffix = if default_yes { "(Y/n)" } else { "(y/N)" };
    eprint!("  {a}?{r} {} {} ", question, suffix);
    let _ = std::io::stderr().flush();
    if !std::io::stdin().is_terminal() {
        eprintln!("{}", if default_yes { "y" } else { "n" });
        return default_yes;
    }
    let mut s = String::new();
    if std::io::stdin().read_line(&mut s).is_err() {
        return default_yes;
    }
    let t = s.trim().to_ascii_lowercase();
    if t.is_empty() {
        return default_yes;
    }
    t.starts_with('y')
}

fn ask_choice(question: &str, options: &[&str], default_char: char) -> char {
    use std::io::IsTerminal;
    let a = theme::accent();
    let r = RESET;
    eprintln!("  {a}?{r} {}:", question);
    for opt in options {
        eprintln!("       {}", opt);
    }
    eprint!("  {a}>{r} ");
    let _ = std::io::stderr().flush();
    if !std::io::stdin().is_terminal() {
        eprintln!("{}", default_char);
        return default_char;
    }
    let mut s = String::new();
    if std::io::stdin().read_line(&mut s).is_err() {
        return default_char;
    }
    let t = s.trim();
    if t.is_empty() {
        return default_char;
    }
    t.chars()
        .next()
        .unwrap_or(default_char)
        .to_ascii_lowercase()
}

async fn probe_server(url: &str) -> Result<()> {
    let endpoint = if url.ends_with('/') {
        format!("{}models", url)
    } else {
        format!("{}/models", url)
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()?;
    let r = client.get(&endpoint).send().await?;
    if !r.status().is_success() {
        return Err(anyhow!("HTTP {}", r.status()));
    }
    Ok(())
}

async fn list_models(url: &str) -> Result<Vec<String>> {
    let endpoint = if url.ends_with('/') {
        format!("{}models", url)
    } else {
        format!("{}/models", url)
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    let r = client.get(&endpoint).send().await?;
    let body: serde_json::Value = r.json().await?;
    let arr = body
        .get("data")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(arr
        .iter()
        .filter_map(|m| m.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .collect())
}

#[allow(dead_code)]
pub async fn download_with_progress(url: &str, dest: &PathBuf) -> Result<u64> {
    use futures_util::StreamExt;
    let client = reqwest::Client::new();
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        return Err(anyhow!("HTTP {} for {}", resp.status(), url));
    }
    let total = resp.content_length();
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::File::create(dest)?;
    let mut stream = resp.bytes_stream();
    let mut downloaded: u64 = 0;
    let mut last_render = std::time::Instant::now();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk)?;
        downloaded += chunk.len() as u64;
        if last_render.elapsed() > Duration::from_millis(150) {
            render_progress(downloaded, total);
            last_render = std::time::Instant::now();
        }
    }
    render_progress(downloaded, total);
    eprintln!();
    Ok(downloaded)
}

#[allow(dead_code)]
fn render_progress(done: u64, total: Option<u64>) {
    use crate::theme::{accent, dim, good, RESET};
    let blocks = ['█', '▉', '▊', '▋', '▌', '▍', '▎', '▏'];
    let width = 40usize;
    let pct = match total {
        Some(t) if t > 0 => (done as f64 / t as f64).min(1.0),
        _ => 0.0,
    };
    let filled = (pct * width as f64) as usize;
    let bar: String = (0..width)
        .map(|i| if i < filled { blocks[0] } else { ' ' })
        .collect();
    let mb = done as f64 / 1024.0 / 1024.0;
    let total_str = total
        .map(|t| format!("/{:.1} MB", t as f64 / 1024.0 / 1024.0))
        .unwrap_or_default();
    eprint!(
        "\r  {d}[{a}{bar}{d}]{r} {g}{pct:5.1}%{r} {mb:.1} MB{total_str}",
        d = dim(),
        a = accent(),
        g = good(),
        r = RESET,
        bar = bar,
        pct = pct * 100.0,
        mb = mb,
        total_str = total_str,
    );
    let _ = std::io::stderr().flush();
}

fn expand(s: &str) -> PathBuf {
    PathBuf::from(shellexpand::tilde(s).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_path_expands_to_home_dot_mlx_code() {
        let p = expand(MARKER_PATH);
        assert!(p.to_string_lossy().contains(".mlx-code"));
        assert!(p.to_string_lossy().ends_with(".welcomed"));
    }

    #[test]
    fn render_progress_handles_unknown_total() {
        render_progress(1024, None);
        render_progress(0, Some(0));
    }

    #[test]
    fn ask_yes_no_falls_back_to_default_when_stdin_not_tty() {
        assert!(ask_yes_no("?", true));
        assert!(!ask_yes_no("?", false));
    }
}
