//! First-run setup wizard + health check.
//!
//! Runs once on the first invocation that's not a one-shot. Probes the local
//! MTPLX server at the configured URL, lists available models, and points
//! the user at the right setup commands if anything is missing.
//!
//! State: a one-byte marker file at `~/.mlx-code/.welcomed`. Presence of
//! that file suppresses the wizard on subsequent runs. `iris-code --setup`
//! re-runs the check regardless.
//!
//! Keep this small: it should NEVER auto-download multi-GB models. It only
//! orchestrates - the actual model fetch is delegated to huggingface-cli
//! (or whatever the user's MTPLX setup uses), which has its own progress
//! handling. We surface a `download_with_progress` helper for future use
//! and as a building block for any small artifacts we might fetch directly.

use anyhow::{anyhow, Result};
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use crate::theme::{self, RESET};

const MARKER_PATH: &str = "~/.mlx-code/.welcomed";

/// True if this is the first iris-code invocation on this machine
/// (no marker file present).
pub fn is_first_run() -> bool {
    let p = expand(MARKER_PATH);
    !p.exists()
}

/// Drop the marker file so subsequent runs skip the wizard.
pub fn mark_welcomed() {
    let p = expand(MARKER_PATH);
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&p, b"1\n");
}

/// Probe the local MTPLX server and print a friendly setup wizard. Returns
/// Ok(true) if the model is up and we should proceed, Ok(false) if we
/// printed instructions and the user should restart, Err on unexpected
/// failure.
pub async fn run_wizard(url: &str) -> Result<bool> {
    let d = theme::dim();
    let a = theme::accent();
    let g = theme::good();
    let w = theme::warn();
    let r = RESET;

    eprintln!();
    eprintln!("{a}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━{r}");
    eprintln!("  {a}🌸 Welcome to iris-code{r}");
    eprintln!("{a}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━{r}");
    eprintln!();
    eprintln!("  Running first-time setup. This is a one-shot check; subsequent");
    eprintln!("  runs will skip it. Re-run anytime with {a}iris-code --setup{r}.");
    eprintln!();

    eprint!("  {d}1/2{r} probing MTPLX server at {a}{url}{r}");
    let _ = std::io::stderr().flush();
    let server = probe_server(url).await;
    match &server {
        Ok(_) => eprintln!(" {g}OK{r}"),
        Err(e) => {
            eprintln!(" {w}NOT REACHABLE{r}");
            eprintln!();
            eprintln!("  {w}!{r} The MTPLX server isn't responding at {url}.");
            eprintln!("    error: {d}{e}{r}");
            eprintln!();
            eprintln!("  To start MTPLX:");
            eprintln!("    {a}cd /path/to/MTPLX && python -m mtplx.server{r}");
            eprintln!("  Or via the dashboard:");
            eprintln!("    {a}open http://localhost:9099{r}");
            eprintln!();
            eprintln!("  See the project README for full setup steps.");
            eprintln!();
            return Ok(false);
        }
    }

    eprint!("  {d}2/2{r} checking for a loaded model");
    let _ = std::io::stderr().flush();
    let models = list_models(url).await.unwrap_or_default();
    if models.is_empty() {
        eprintln!(" {w}NO MODELS{r}");
        eprintln!();
        eprintln!("  {w}!{r} MTPLX is up but has no models loaded.");
        eprintln!();
        eprintln!("  Pull and load a quantized Qwen3-27B (recommended):");
        eprintln!("    {a}huggingface-cli download mlx-community/Qwen3-27B-Instruct-4bit{r}");
        eprintln!("  Then load it via the MTPLX dashboard or CLI.");
        eprintln!();
        eprintln!("  Iris-code will use whichever model is exposed at /v1/models.");
        eprintln!();
        return Ok(false);
    }

    eprintln!(" {g}OK{r}  ({} model(s) available)", models.len());
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
    eprintln!("  {g}✓{r} ready. Run {a}iris-code{r} to chat or {a}iris-code 'your prompt'{r}");
    eprintln!("    type {a}:help{r} once in the REPL for commands.");
    eprintln!();

    mark_welcomed();
    Ok(true)
}

async fn probe_server(url: &str) -> Result<()> {
    // GET <url>/models with a short timeout.
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

/// Stream a download to a path with a Unicode-block progress bar on stderr.
/// Returns the number of bytes written. Used as a building block for future
/// model-download workflows; the wizard itself does not call this since the
/// model lives in MTPLX, not iris-code.
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
        // Should not panic when total is None.
        render_progress(1024, None);
        render_progress(0, Some(0));
    }
}
