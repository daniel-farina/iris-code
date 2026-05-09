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
const MTPLX_BRANCH: &str = "perf/main-aligned-2026-05-08";
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
    eprintln!("  {a}🌸 Welcome to iris{r}");
    eprintln!("{a}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━{r}");
    eprintln!();
    eprintln!("  Running first-time setup. Re-run anytime with {a}iris --setup{r}.");
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
    eprintln!("  {g}✓{r} run {a}iris{r} to chat or {a}iris 'your prompt'{r}");
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
    eprintln!("  {a}iris{r} talks to a local MTPLX server. We can install our fork");
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

    eprintln!();
    eprintln!("  {d}installing python dependencies (pip install -e .)...{r}");
    let status = std::process::Command::new("pip")
        .args(["install", "-e", "."])
        .current_dir(&install_dir)
        .status();
    match status {
        Ok(s) if s.success() => eprintln!("  {g}✓{r} installed"),
        Ok(s) => {
            eprintln!(
                "  {w}!{r} pip install exited {}; you may need a venv first.",
                s
            );
            eprintln!(
                "    {a}cd {} && python -m venv .venv && source .venv/bin/activate && pip install -e .{r}",
                install_dir.display()
            );
            return Ok(());
        }
        Err(e) => {
            eprintln!("  {w}!{r} pip not found: {}", e);
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
            eprintln!("    {a}python -m mtplx.server{r}");
            eprintln!();
            eprintln!("  Then re-run {a}iris{r}.");
        }
        _ => {
            eprintln!();
            eprintln!("  Skipping start. When you're ready:");
            eprintln!("    {a}cd {}{r}", install_dir.display());
            eprintln!("    {a}python -m mtplx.server{r}");
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

    use std::os::unix::process::CommandExt;
    let child = unsafe {
        std::process::Command::new("python")
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
            eprintln!(
                "  Give it ~10s to load the model, then re-run {a}iris --setup{r} to verify.",
            );
        }
        Err(e) => {
            eprintln!("  {w}!{r} failed to spawn MTPLX: {}", e);
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
    eprintln!("  Then re-run {a}iris{r}.");
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
