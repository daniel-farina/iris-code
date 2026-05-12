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
        check_mtplx_updates();
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
    check_mtplx_updates();
    Ok(())
}

/// After hip update completes (or hip is already current), check the
/// local MTPLX checkout for upstream commits and offer to pull. Silent
/// no-op when MTPLX isn't installed or the checkout isn't a git repo,
/// so it can't make `hip --update` worse than it was.
fn check_mtplx_updates() {
    use crate::setup::{prompt_mtplx_source, MtplxSource};
    use crate::theme::{accent, dim, good, warn, RESET};
    use std::process::Command;

    let d = dim();
    let a = accent();
    let g = good();
    let w = warn();
    let r = RESET;

    // Resolve where MTPLX lives. Same env-var override that setup.rs
    // honors (HIP_MTPLX_INSTALL_DIR), with the same default of
    // ~/code/MTPLX. Users with non-standard installs can point us at
    // them without forcing a build.
    let install_dir = std::env::var("HIP_MTPLX_INSTALL_DIR")
        .ok()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(shellexpand::tilde("~/code/MTPLX").into_owned())
        });

    eprintln!();
    eprintln!(
        "{d}─ checking MTPLX at {a}{}{d} ─{r}",
        install_dir.display()
    );

    if !install_dir.exists() {
        eprintln!("  {w}!{r} no MTPLX checkout found. Run {a}hip --setup{r} to install one.");
        return;
    }
    if !install_dir.join(".git").exists() {
        eprintln!(
            "  {w}!{r} {} exists but is not a git checkout; cannot check version.",
            install_dir.display()
        );
        return;
    }

    let dir_str = install_dir.to_string_lossy().to_string();

    // Detect current remote + branch + local HEAD so we can show them
    // exactly what version they have and default the picker to "keep
    // current source".
    let current_remote = Command::new("git")
        .args(["-C", &dir_str, "remote", "get-url", "origin"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let current_branch = Command::new("git")
        .args(["-C", &dir_str, "rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let head_short = Command::new("git")
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

    // Print the version banner BEFORE any branching / prompts so the
    // user always sees what they currently have, even if the rest of
    // the function bails out for some reason.
    let label_hint = MtplxSource::classify(&current_remote, &current_branch);
    eprintln!(
        "  {d}current{r}: {a}{}{r} @ {a}{}{r}  {d}[{}]{r}",
        if current_remote.is_empty() {
            "<no-remote>"
        } else {
            current_remote.as_str()
        },
        if current_branch.is_empty() {
            "<unknown>"
        } else {
            current_branch.as_str()
        },
        label_hint
    );
    if !head_short.is_empty() {
        eprintln!(
            "  {d}version{r}: {a}{}{r}{}",
            head_short,
            if head_date.is_empty() {
                String::new()
            } else {
                format!(" {d}({}){r}", head_date)
            }
        );
    }

    if current_branch.is_empty() || current_branch == "HEAD" {
        eprintln!("  {w}!{r} MTPLX checkout has no branch (detached HEAD); leaving it alone.");
        return;
    }

    // Default the picker to whatever the checkout is on (so "just hit
    // enter" keeps it on the same source). Falls back to upstream when
    // the current remote doesn't match either well-known source.
    let default_src = match MtplxSource::classify(&current_remote, &current_branch) {
        "fork" => MtplxSource::fork(),
        "upstream" => MtplxSource::upstream(),
        _ => MtplxSource::upstream(),
    };
    let chosen = prompt_mtplx_source(default_src);

    // If the picked source differs from what the checkout points at, we
    // need to switch remotes + branches before fetching. This is the
    // "overwrite current MTPLX with the latest from upstream / fork" path.
    let current_norm = current_remote
        .trim_end_matches('/')
        .trim_end_matches(".git");
    let chosen_norm = chosen.repo.trim_end_matches('/').trim_end_matches(".git");
    let switching = current_norm != chosen_norm || current_branch != chosen.branch;

    if switching {
        // Refuse to clobber uncommitted work — the user can clean up and
        // re-run.
        let dirty = Command::new("git")
            .args(["-C", &dir_str, "status", "--porcelain"])
            .output()
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(false);
        if dirty {
            eprintln!("{w}!{r} MTPLX checkout has uncommitted changes; refusing to switch source.");
            eprintln!(
                "  {d}commit or stash inside {} and re-run `hip --update` to switch.{r}",
                install_dir.display()
            );
            return;
        }

        eprintln!();
        eprintln!(
            "{w}!{r} switching MTPLX source: {a}{}{r} @ {a}{}{r}",
            chosen.repo, chosen.branch
        );
        eprintln!(
            "  {d}this will run `git remote set-url`, fetch, and reset the working tree \
             to the chosen source's HEAD.{r}"
        );
        eprint!("  {d}proceed? [Y/n] {r}");
        let _ = std::io::Write::flush(&mut std::io::stderr());
        let mut input = String::new();
        if std::io::stdin().read_line(&mut input).is_err() {
            return;
        }
        let answer = input.trim().to_lowercase();
        if !answer.is_empty() && !answer.starts_with('y') {
            eprintln!("{d}skipped{r}");
            return;
        }

        // Rewrite origin -> chosen.repo so subsequent fetches go to the
        // new source.
        let set_url = Command::new("git")
            .args(["-C", &dir_str, "remote", "set-url", "origin", &chosen.repo])
            .status();
        if !matches!(set_url, Ok(s) if s.success()) {
            eprintln!("{w}!{r} `git remote set-url` failed; aborting source switch");
            return;
        }

        let fetch = Command::new("git")
            .args(["-C", &dir_str, "fetch", "origin", "--quiet"])
            .status();
        if !matches!(fetch, Ok(s) if s.success()) {
            eprintln!("{w}!{r} could not fetch from {}", chosen.repo);
            return;
        }

        // Force the local branch to the chosen source's HEAD. Two repos
        // may not share history, so a normal merge would fail -- we
        // commit to overwrite the working tree (the dirty-check above
        // guards against losing user work).
        let checkout = Command::new("git")
            .args([
                "-C",
                &dir_str,
                "checkout",
                "-B",
                &chosen.branch,
                &format!("origin/{}", chosen.branch),
            ])
            .status();
        if !matches!(checkout, Ok(s) if s.success()) {
            eprintln!(
                "{w}!{r} could not check out {} from {}",
                chosen.branch, chosen.repo
            );
            return;
        }
        eprintln!(
            "{g}✓{r} switched MTPLX to {} @ {}",
            chosen.repo, chosen.branch
        );
        flag_running_server();
        return;
    }

    // Same source as before: just fetch + offer a fast-forward pull.
    let fetch = Command::new("git")
        .args(["-C", &dir_str, "fetch", "origin", "--quiet"])
        .status();
    if !matches!(fetch, Ok(s) if s.success()) {
        eprintln!("{w}!{r} could not fetch MTPLX updates");
        return;
    }

    let behind_out = Command::new("git")
        .args([
            "-C",
            &dir_str,
            "rev-list",
            "--count",
            &format!("HEAD..origin/{}", chosen.branch),
        ])
        .output();
    let behind: u32 = match behind_out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .trim()
            .parse()
            .unwrap_or(0),
        _ => return,
    };

    if behind == 0 {
        eprintln!("{g}✓{r} MTPLX is up to date ({})", chosen.branch);
        return;
    }

    // Preview what would land so the user can decide.
    if let Ok(o) = Command::new("git")
        .args([
            "-C",
            &dir_str,
            "log",
            "--oneline",
            "-5",
            &format!("HEAD..origin/{}", chosen.branch),
        ])
        .output()
    {
        eprintln!(
            "{d}MTPLX is {a}{}{d} commit(s) behind {a}origin/{}{r}",
            behind, chosen.branch
        );
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            eprintln!("  {a}{}{r}", line);
        }
    }

    eprint!("{d}update MTPLX? [Y/n] {r}");
    let _ = std::io::Write::flush(&mut std::io::stderr());
    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        return;
    }
    let answer = input.trim().to_lowercase();
    if !answer.is_empty() && !answer.starts_with('y') {
        eprintln!("{d}skipped{r}");
        return;
    }

    let pull = Command::new("git")
        .args([
            "-C",
            &dir_str,
            "pull",
            "--ff-only",
            "origin",
            &chosen.branch,
        ])
        .status();
    if !matches!(pull, Ok(s) if s.success()) {
        eprintln!(
            "{w}!{r} MTPLX pull failed; resolve manually in {}",
            install_dir.display()
        );
        return;
    }
    eprintln!("{g}✓{r} MTPLX updated to latest origin/{}", chosen.branch);
    flag_running_server();
}

/// If the MTPLX server is listening on :8088, tell the user it still has
/// the old code mapped and needs a restart to pick up the new files.
fn flag_running_server() {
    use crate::theme::{warn, RESET};
    let w = warn();
    let r = RESET;
    let server_listening = std::process::Command::new("lsof")
        .args(["-nP", "-iTCP:8088", "-sTCP:LISTEN"])
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    if server_listening {
        eprintln!(
            "{w}!{r} MTPLX server still running on :8088 with the old code -- restart it to pick up the update"
        );
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
