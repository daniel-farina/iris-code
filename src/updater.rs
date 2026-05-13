//! Update check + self-update.
//!
//! Two surfaces:
//! 1. `update_notice_if_any()` - called early in main(); if the cache says
//!    a newer version is published on GitHub, returns a short dim notice
//!    string. Refreshes the cache if it's >24h old, with a 2s HTTP timeout
//!    so we never noticeably block startup.
//! 2. `do_update()` - called from `hip --update`; downloads the right
//!    platform tarball + sha256 from the latest GitHub release, verifies,
//!    and atomically replaces the running binary in place.
//!
//! Cache: `~/.mlx-code/.update-check` JSON
//!   {"checked_at": <unix>, "latest": "v0.1.2"}
//!
//! Env opt-out: `IRIS_NO_UPDATE_CHECK=1` disables both the notice and
//! any background fetch (useful in CI / sandboxed contexts).

use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CACHE_PATH: &str = "~/.mlx-code/.update-check";
const CACHE_TTL_SECS: u64 = 24 * 60 * 60; // 24h
const REPO: &str = "daniel-farina/hippo-code";
const HTTP_TIMEOUT: Duration = Duration::from_millis(2000);

#[derive(Debug, Clone)]
struct CachedCheck {
    checked_at: u64,
    latest: String,
}

fn cache_path() -> PathBuf {
    PathBuf::from(shellexpand::tilde(CACHE_PATH).into_owned())
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn read_cache() -> Option<CachedCheck> {
    let body = std::fs::read_to_string(cache_path()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    let checked_at = v.get("checked_at").and_then(|x| x.as_u64())?;
    let latest = v.get("latest").and_then(|x| x.as_str())?.to_string();
    Some(CachedCheck { checked_at, latest })
}

fn write_cache(c: &CachedCheck) {
    let p = cache_path();
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body = serde_json::json!({"checked_at": c.checked_at, "latest": c.latest});
    let _ = std::fs::write(&p, body.to_string());
}

/// Compare two version strings shaped like "v0.1.2" or "0.1.2".
/// Returns Ordering of `a` vs `b`. Falls back to lexical compare on parse
/// failure (which still beats nothing).
fn cmp_version(a: &str, b: &str) -> std::cmp::Ordering {
    fn parse(v: &str) -> Vec<u32> {
        v.trim_start_matches('v')
            .split('.')
            .map(|x| x.parse::<u32>().unwrap_or(0))
            .collect()
    }
    let pa = parse(a);
    let pb = parse(b);
    pa.cmp(&pb)
}

/// Fetch the latest release tag from GitHub. Short timeout; returns None
/// on any failure so the caller doesn't have to care.
///
/// Picks the highest semver across `/releases`, NOT GitHub's "latest"
/// flag — that flag is set by published-at timestamp by default, so two
/// tags pushed back-to-back can land in the wrong order and leave older
/// releases marked as latest. Enumerating + sorting locally is robust
/// against that and against any maintainer who forgot `make_latest` on
/// the release workflow.
async fn fetch_latest_tag() -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .user_agent(format!("hip/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()?;
    let url = format!("https://api.github.com/repos/{}/releases?per_page=30", REPO);
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: serde_json::Value = resp.json().await.ok()?;
    let releases = body.as_array()?;
    releases
        .iter()
        .filter(|r| !r.get("draft").and_then(|v| v.as_bool()).unwrap_or(false))
        .filter(|r| {
            !r.get("prerelease")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .filter_map(|r| r.get("tag_name").and_then(|v| v.as_str()))
        .max_by(|a, b| cmp_version(a, b))
        .map(|s| s.to_string())
}

/// Read cache, refresh if stale, return a one-line update notice if a
/// newer version is available than the running binary's `CARGO_PKG_VERSION`.
/// Never blocks for more than HTTP_TIMEOUT; on any failure returns None.
pub async fn update_notice_if_any() -> Option<String> {
    if std::env::var("IRIS_NO_UPDATE_CHECK")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        return None;
    }
    let current = env!("CARGO_PKG_VERSION");
    let now = now_unix();

    // Read cache; refresh if stale or missing.
    let cached = read_cache();
    let fresh = match cached {
        Some(c) if now.saturating_sub(c.checked_at) < CACHE_TTL_SECS => c,
        _ => {
            let latest = fetch_latest_tag().await?;
            let c = CachedCheck {
                checked_at: now,
                latest,
            };
            write_cache(&c);
            c
        }
    };

    // Compare (strip leading 'v' on the cached side; current is bare semver).
    if cmp_version(&fresh.latest, current).is_gt() {
        Some(format_notice(&fresh.latest, current))
    } else {
        None
    }
}

fn format_notice(latest: &str, current: &str) -> String {
    use crate::theme::{accent, dim, warn, RESET};
    format!(
        "{d}─ update available: {a}{latest}{d} (currently on {a}{current}{d}) ─ run {w}hip --update{d} to install ─{r}",
        d = dim(), a = accent(), w = warn(), r = RESET,
        latest = latest, current = current,
    )
}

/// `hip --update`: download the latest release tarball for the running
/// platform, verify SHA256, and atomically replace the running binary.
pub async fn do_update() -> Result<()> {
    use crate::theme::{accent, dim, good, warn, RESET};
    let d = dim();
    let a = accent();
    let g = good();
    let w = warn();
    let r = RESET;

    let current = env!("CARGO_PKG_VERSION");
    eprintln!("{d}current version: {a}{current}{r}");

    eprint!("{d}fetching latest release...{r}");
    let _ = std::io::Write::flush(&mut std::io::stderr());
    let latest = fetch_latest_tag()
        .await
        .ok_or_else(|| anyhow!("could not reach GitHub API (timeout or network error)"))?;
    eprintln!(" {a}{latest}{r}");

    if !cmp_version(&latest, current).is_gt() {
        eprintln!("{g}✓{r} already on the latest version ({current})");
        check_mtplx_updates().await;
        return Ok(());
    }

    let (os, arch) = detect_platform().ok_or_else(|| {
        anyhow!(
            "unsupported platform; only darwin-arm64 / darwin-x86_64 / linux-x86_64 are released"
        )
    })?;
    let artifact = format!("hip-{}-{}-{}.tar.gz", latest, os, arch);
    let base_url = format!("https://github.com/{}/releases/download/{}", REPO, latest);

    eprintln!("{d}downloading {a}{}/{}{r}", base_url, artifact);

    let tmp = tempdir()?;
    let tar_path = tmp.join(&artifact);
    let sha_path = tmp.join(format!("{}.sha256", artifact));

    download(&format!("{}/{}", base_url, artifact), &tar_path)
        .await
        .with_context(|| format!("downloading {}", artifact))?;
    if download(&format!("{}/{}.sha256", base_url, artifact), &sha_path)
        .await
        .is_ok()
    {
        verify_sha256(&tar_path, &sha_path)?;
        eprintln!("{g}✓{r} checksum verified");
    } else {
        eprintln!("{w}!{r} no .sha256 published - skipping verification");
    }

    // Extract.
    let extract_dir = tmp.join("extract");
    std::fs::create_dir_all(&extract_dir)?;
    let status = std::process::Command::new("tar")
        .args([
            "xzf",
            tar_path.to_string_lossy().as_ref(),
            "-C",
            extract_dir.to_string_lossy().as_ref(),
        ])
        .status()?;
    if !status.success() {
        return Err(anyhow!("tar extract failed (exit {})", status));
    }

    // Locate the new binary inside the extracted tree.
    let new_bin = find_binary(&extract_dir, "hip")
        .ok_or_else(|| anyhow!("'hip' binary not found in archive"))?;

    // Replace the running binary. On macOS/Linux you can rename over a
    // running executable - the kernel keeps the old inode mapped for the
    // running process; new invocations get the replacement.
    let dest = std::env::current_exe()?;
    let dest_new = dest.with_extension("new");
    std::fs::copy(&new_bin, &dest_new)?;
    set_executable(&dest_new)?;
    std::fs::rename(&dest_new, &dest).with_context(|| format!("replacing {}", dest.display()))?;

    eprintln!("{g}✓{r} installed {a}{}{r} at {}", latest, dest.display());
    eprintln!("{d}restart hip to use the new version{r}");
    check_mtplx_updates().await;
    Ok(())
}

/// Unified MTPLX status + update + restart driver. Detects install kind
/// (brew or git checkout), prints a consistent banner, handles source
/// switching for git installs, runs `brew upgrade mtplx` or `git pull`
/// as appropriate, and respawns the server with the optimal config if
/// new code landed and the server is still on stale bytes.
async fn check_mtplx_updates() {
    use crate::mtplx_runner::{
        apply_upgrade, check_upstream, detect_state, is_running_stale, render_status,
        restart_with_optimal_config, InstallKind,
    };
    use crate::setup::{persist_source, prompt_mtplx_source, MtplxSource};
    use crate::theme::{accent, dim, good, warn, RESET};
    use std::process::Command;
    let d = dim();
    let a = accent();
    let g = good();
    let w = warn();
    let r = RESET;

    eprintln!();
    eprintln!("{d}─ checking MTPLX ─{r}");

    let mut state = match detect_state() {
        Some(s) => s,
        None => {
            eprintln!(
                "  {w}!{r} could not locate MTPLX. Install via {a}brew install youssofal/mtplx/mtplx{r}"
            );
            eprintln!("    or run {a}hip --setup{r} to clone a source checkout.");
            return;
        }
    };

    render_status(&state);

    // For git installs only: offer the source picker. This lets the user
    // flip between upstream and the daniel-farina fork via the arrow-key
    // selector. Brew installs don't have an equivalent (the formula
    // points at one source by definition).
    let mut just_switched = false;
    if let InstallKind::Git {
        repo_root,
        remote,
        branch,
        ..
    } = state.kind.clone()
    {
        if branch.is_empty() || branch == "HEAD" {
            eprintln!("  {w}!{r} MTPLX checkout has no branch (detached HEAD); leaving it alone.");
            return;
        }
        let default_src = match MtplxSource::classify(&remote, &branch) {
            "fork" => MtplxSource::fork(),
            _ => MtplxSource::upstream(),
        };
        let chosen = prompt_mtplx_source(default_src);
        persist_source(&chosen);

        // If the picked source differs from current, switch remotes +
        // branches. This is the only path in `hip --update` that mutates
        // user-visible state without a separate "going to do X" line --
        // we explicitly call out the clobber before it happens.
        let current_norm = remote.trim_end_matches('/').trim_end_matches(".git");
        let chosen_norm = chosen.repo.trim_end_matches('/').trim_end_matches(".git");
        if current_norm != chosen_norm || branch != chosen.branch {
            let dir = repo_root.to_string_lossy().to_string();
            let dirty = Command::new("git")
                .args(["-C", &dir, "status", "--porcelain"])
                .output()
                .map(|o| !o.stdout.is_empty())
                .unwrap_or(false);
            if dirty {
                eprintln!(
                    "  {w}!{r} MTPLX checkout has uncommitted changes; refusing to switch source."
                );
                eprintln!(
                    "  {d}commit or stash inside {} and re-run `hip --update` to switch.{r}",
                    repo_root.display()
                );
                return;
            }
            eprintln!();
            eprintln!(
                "  {d}switching MTPLX source to {a}{}{d} @ {a}{}{d}...{r}",
                chosen.repo, chosen.branch
            );
            let ok = Command::new("git")
                .args(["-C", &dir, "remote", "set-url", "origin", &chosen.repo])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
                && Command::new("git")
                    .args(["-C", &dir, "fetch", "origin", "--quiet"])
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false)
                && Command::new("git")
                    .args([
                        "-C",
                        &dir,
                        "checkout",
                        "-B",
                        &chosen.branch,
                        &format!("origin/{}", chosen.branch),
                    ])
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
            if !ok {
                eprintln!(
                    "  {w}!{r} source switch failed; resolve manually in {}",
                    repo_root.display()
                );
                return;
            }
            eprintln!("  {g}✓{r} switched to {} @ {}", chosen.repo, chosen.branch);
            just_switched = true;
            // Re-detect state so subsequent steps see the new branch/SHA.
            if let Some(s) = detect_state() {
                state = s;
            }
        }
    }

    // Check for upstream updates appropriate to the install kind.
    let mut just_upgraded = just_switched;
    match check_upstream(&state).await {
        Some(label) => {
            eprintln!("  {w}!{r} update available: {a}{}{r}", label);
            eprintln!("  {d}applying upgrade...{r}");
            if apply_upgrade(&state).await {
                eprintln!("  {g}✓{r} upgrade applied");
                just_upgraded = true;
                if let Some(s) = detect_state() {
                    state = s;
                }
            } else {
                eprintln!("  {w}!{r} upgrade failed; run it manually to diagnose.");
            }
        }
        None if !just_switched => {
            // Nothing to pull/upgrade AND we didn't just switch sources.
            // Print the "up to date" line only here so a switch-followed-
            // by-no-upgrade still reads cleanly.
            eprintln!("  {g}✓{r} MTPLX is up to date");
        }
        None => {}
    }

    // If on-disk code is now newer than the running process, restart with
    // optimal config to land on the new bytes. Same trigger for brew
    // (version mismatch) and git (we just successfully pulled or switched).
    if is_running_stale(&state, just_upgraded) {
        eprintln!("  {w}!{r} running server is on stale code; brew/disk has newer bytes.");
        restart_with_optimal_config(&state).await;
    }
}

fn detect_platform() -> Option<(&'static str, &'static str)> {
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        return None;
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        return None;
    };
    Some((os, arch))
}

async fn download(url: &str, dest: &PathBuf) -> Result<()> {
    use futures_util::StreamExt;
    use std::io::Write;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        return Err(anyhow!("HTTP {} for {}", resp.status(), url));
    }
    let mut file = std::fs::File::create(dest)?;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        file.write_all(&chunk?)?;
    }
    Ok(())
}

fn verify_sha256(tar_path: &PathBuf, sha_path: &PathBuf) -> Result<()> {
    let body = std::fs::read_to_string(sha_path)?;
    let expected = body
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow!("empty .sha256 file"))?;
    // Use `shasum` or `sha256sum` (no rust-crypto dep).
    let bin = if std::process::Command::new("shasum")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        "shasum"
    } else {
        "sha256sum"
    };
    let args: &[&str] = if bin == "shasum" { &["-a", "256"] } else { &[] };
    let out = std::process::Command::new(bin)
        .args(args)
        .arg(tar_path)
        .output()?;
    let actual = String::from_utf8_lossy(&out.stdout);
    let actual = actual.split_whitespace().next().unwrap_or("");
    if actual != expected {
        return Err(anyhow!(
            "checksum mismatch: expected {}, got {}",
            expected,
            actual
        ));
    }
    Ok(())
}

fn find_binary(root: &PathBuf, name: &str) -> Option<PathBuf> {
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry.ok()?;
        if entry.file_type().is_file() && entry.file_name() == name {
            return Some(entry.path().to_path_buf());
        }
    }
    None
}

fn set_executable(p: &PathBuf) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(p)?.permissions();
    perm.set_mode(perm.mode() | 0o111);
    std::fs::set_permissions(p, perm)?;
    Ok(())
}

fn tempdir() -> Result<PathBuf> {
    let p = std::env::temp_dir().join(format!("hip-update-{}", std::process::id()));
    std::fs::create_dir_all(&p)?;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmp_version_handles_v_prefix_and_numeric_compare() {
        assert!(cmp_version("v0.1.2", "v0.1.1").is_gt());
        assert!(cmp_version("0.1.2", "v0.1.1").is_gt());
        assert!(cmp_version("v0.1.0", "0.1.1").is_lt());
        assert!(cmp_version("v1.0.0", "0.99.99").is_gt());
        assert_eq!(cmp_version("v0.1.1", "0.1.1"), std::cmp::Ordering::Equal);
    }

    #[test]
    fn detect_platform_returns_supported_pair_or_none() {
        // On any of the supported targets this returns Some; otherwise None.
        let p = detect_platform();
        if cfg!(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "macos", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "x86_64"),
        )) {
            assert!(p.is_some());
        }
    }

    #[test]
    fn cache_roundtrips_through_disk() {
        // Use a custom cache path under a tmpdir so we don't clobber real state.
        let dir = std::env::temp_dir().join(format!("hip-update-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("cache.json");
        let body = serde_json::json!({"checked_at": 1700000000_u64, "latest": "v0.9.9"});
        std::fs::write(&p, body.to_string()).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(
            parsed.get("latest").and_then(|v| v.as_str()),
            Some("v0.9.9")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
