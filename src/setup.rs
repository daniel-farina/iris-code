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
const MTPLX_REPO_URL: &str = "https://github.com/daniel-farina/MTPLX";
// Integration branch: fork/main (which already has the merged upstream
// #35 + #32 work) plus the cherry-picked #37 (postcommit-wait race fix)
// and #33 (dense/repage chunk-size split + 128k bench). Refresh this when
// new perf work lands on top of fork/main and we want fresh installs to
// get it. 569/4 pytest passing on this branch.
const MTPLX_BRANCH: &str = "share/install-2026-05-09";
const MTPLX_DEFAULT_INSTALL_DIR: &str = "~/code/MTPLX";
const MTPLX_PID_FILE: &str = "~/.mlx-code/mtplx.pid";
const MTPLX_LOG_FILE: &str = "~/.mlx-code/mtplx.log";

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
        return offer_install_mtplx().await.map(|_| false);
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
async fn offer_install_mtplx() -> Result<()> {
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
        MTPLX_REPO_URL, MTPLX_BRANCH
    );
    eprintln!("  and have it ready in a few steps.");
    eprintln!();

    if !ask_yes_no("install MTPLX now?", true) {
        print_manual_setup();
        return Ok(());
    }

    // Verify build/runtime prerequisites BEFORE we start cloning multi-MB
    // repos. Auto-install what we safely can (pip via ensurepip); for
    // heavier tools (git/python3) print platform-specific install commands.
    if !ensure_prerequisites() {
        return Ok(());
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
        let status = std::process::Command::new("git")
            .args([
                "clone",
                "--depth",
                "1",
                "--branch",
                MTPLX_BRANCH,
                MTPLX_REPO_URL,
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
                return Ok(());
            }
            Err(e) => {
                eprintln!("  {w}!{r} git clone failed: {} (is git installed?)", e);
                print_manual_setup();
                return Ok(());
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
        None => return Ok(()),
    };

    eprintln!();
    eprintln!("  {d}installing python dependencies into .venv...{r}");
    let status = std::process::Command::new(&venv_python)
        .args(["-m", "pip", "install", "-e", "."])
        .current_dir(&install_dir)
        .status();
    match status {
        Ok(s) if s.success() => eprintln!("  {g}✓{r} installed"),
        Ok(s) => {
            eprintln!("  {w}!{r} pip install (in venv) exited {}", s);
            eprintln!("    Try manually:");
            eprintln!(
                "      {a}cd {} && .venv/bin/python -m pip install -e .{r}",
                install_dir.display()
            );
            return Ok(());
        }
        Err(e) => {
            eprintln!("  {w}!{r} could not invoke venv python: {}", e);
            return Ok(());
        }
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
        'b' => start_mtplx_background(&install_dir),
        'n' => {
            eprintln!();
            eprintln!("  Open a new terminal and run:");
            eprintln!("    {a}cd {}{r}", install_dir.display());
            eprintln!("    {a}.venv/bin/python -m mtplx.server{r}");
            eprintln!();
            eprintln!("  Then re-run {a}hip{r}.");
        }
        _ => {
            eprintln!();
            eprintln!("  Skipping start. When you're ready:");
            eprintln!("    {a}cd {}{r}", install_dir.display());
            eprintln!("    {a}.venv/bin/python -m mtplx.server{r}");
        }
    }
    Ok(())
}

fn start_mtplx_background(install_dir: &Path) {
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
            return;
        }
    };
    let log_err = match log_file.try_clone() {
        Ok(f) => f,
        Err(e) => {
            eprintln!("  {w}!{r} dup log fd failed: {}", e);
            return;
        }
    };

    // Prefer the venv's python if present (we created it during install).
    // Fallback to system python only if the install was done outside this
    // wizard (e.g. user ran `iris --setup` against an existing checkout).
    let venv_python = install_dir.join(".venv").join("bin").join("python");
    let python_bin: std::path::PathBuf = if venv_python.exists() {
        venv_python
    } else {
        python_cmd().into()
    };

    use std::os::unix::process::CommandExt;
    let child = unsafe {
        std::process::Command::new(&python_bin)
            .args(["-m", "mtplx.server"])
            .current_dir(install_dir)
            .stdout(std::process::Stdio::from(log_file))
            .stderr(std::process::Stdio::from(log_err))
            .stdin(std::process::Stdio::null())
            .pre_exec(|| {
                // Become a new session leader so this child survives our exit.
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            })
            .spawn()
    };
    match child {
        Ok(c) => {
            let pid = c.id();
            let _ = std::fs::write(&pid_path, format!("{}\n", pid));
            eprintln!();
            eprintln!("  {g}✓{r} MTPLX started in background");
            eprintln!("    {d}pid:{r}  {a}{pid}{r}  ({})", pid_path.display());
            eprintln!("    {d}logs:{r} {a}tail -f {}{r}", log_path.display());
            eprintln!("    {d}stop:{r} {a}kill {pid}{r}");
            eprintln!();
            eprintln!("  Give it ~10s to load the model, then re-run {a}hip --setup{r} to verify.",);
        }
        Err(e) => {
            eprintln!("  {w}!{r} failed to spawn MTPLX: {}", e);
        }
    }
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

    // python3
    eprint!("    {d}python3{r}");
    let _ = std::io::stderr().flush();
    if which_exists("python3") || which_exists("python") {
        eprintln!(" {g}OK{r}");
    } else {
        eprintln!(" {w}MISSING{r}");
        missing.push("python3");
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

/// Ensure `<install_dir>/.venv` exists with a working `bin/python`. Creates
/// it via `python3 -m venv` if missing. Returns the path to the venv's
/// python executable, or None if creation failed.
///
/// Why a venv: modern Homebrew Python and Debian's python3 reject system-wide
/// pip installs (PEP 668). A venv is the portable way to install MTPLX
/// without --break-system-packages or sudo.
fn ensure_venv(install_dir: &Path) -> Option<PathBuf> {
    let d = theme::dim();
    let g = theme::good();
    let w = theme::warn();
    let a = theme::accent();
    let r = RESET;

    let venv_dir = install_dir.join(".venv");
    let venv_python = venv_dir.join("bin").join("python");

    if venv_python.exists() {
        eprintln!();
        eprintln!(
            "  {d}venv exists at {a}{}{d}; reusing{r}",
            venv_dir.display()
        );
        return Some(venv_python);
    }

    eprintln!();
    eprintln!(
        "  {d}creating virtualenv at {a}{}{d}...{r}",
        venv_dir.display()
    );
    let status = std::process::Command::new(python_cmd())
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
            eprintln!("  {w}!{r} `python -m venv .venv` exited {}", s);
            eprintln!(
                "    On Debian/Ubuntu you may need: {a}sudo apt-get install -y python3-venv{r}"
            );
            eprintln!(
                "    On macOS Homebrew Python this is usually pre-installed; check {a}{} -m venv --help{r}",
                python_cmd()
            );
            None
        }
        Err(e) => {
            eprintln!("  {w}!{r} could not invoke {} -m venv: {}", python_cmd(), e);
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
        MTPLX_BRANCH, MTPLX_REPO_URL
    );
    eprintln!("    {a}cd ~/code/MTPLX && pip install -e .{r}");
    eprintln!("    {a}python -m mtplx.server{r}");
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
