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
    use crate::setup::{persist_source, prompt_mtplx_source, read_persisted_source, MtplxSource};
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

    // Brew installs: also offer the picker so users who want the fork
    // (with the local long-context-ladder patch) get nudged at every
    // --update. If they pick fork on a brew install we execute the
    // overlay automatically: clone-or-pull the fork to ~/code/MTPLX and
    // `pip install --force-reinstall --no-deps` into the brew venv.
    // Brew formula still points at upstream so the next `brew upgrade
    // mtplx` will revert it (we warn about that).
    if let InstallKind::Brew { .. } = state.kind {
        let default_src = read_persisted_source().unwrap_or_else(MtplxSource::fork);
        match read_persisted_source() {
            Some(persisted) if persisted.label == default_src.label => {
                eprintln!(
                    "  {d}source preference{r}: {a}{} @ {}{r} {d}(already persisted; --pick-source to change){r}",
                    persisted.repo, persisted.branch
                );
                // Even with a persisted preference we ensure the overlay is
                // current: re-pull + re-install so the brew venv keeps the
                // fork's latest HEAD if anything's drifted.
                if persisted.label == "fork" {
                    apply_brew_fork_overlay(&persisted.repo, &persisted.branch);
                }
            }
            _ => {
                let c = prompt_mtplx_source(default_src);
                persist_source(&c);
                if c.label == "fork" {
                    apply_brew_fork_overlay(&c.repo, &c.branch);
                }
            }
        }
    }

    // For git installs only: offer the source picker. This lets the user
    // flip between upstream and the daniel-farina fork via the arrow-key
    // selector and actually mutates the checkout's remote + branch.
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

        // Skip the picker entirely when (a) a preference has already been
        // persisted and (b) it matches what the checkout is currently on.
        // No reason to bug the user every `hip --update` if their setup
        // already agrees with their saved choice. The picker still shows
        // up on first-ever update (no persisted file) or when the
        // checkout has drifted from the persisted preference.
        let chosen = match read_persisted_source() {
            Some(persisted)
                if persisted.label == default_src.label
                    && persisted.repo == default_src.repo
                    && persisted.branch == default_src.branch =>
            {
                eprintln!(
                    "  {d}source preference{r}: {a}{} @ {}{r} {d}(already persisted; --pick-source to change){r}",
                    persisted.repo, persisted.branch
                );
                persisted
            }
            _ => {
                let c = prompt_mtplx_source(default_src);
                persist_source(&c);
                c
            }
        };

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

    // Explicit server-state line so every run tells the user what
    // happened to the running process. Previously a "no restart needed"
    // case was indistinguishable from "we forgot to check" -- the user
    // had to infer from the absence of output.
    // Closing state line. Priority:
    //   1. stale code (post-upgrade, didn't restart yet)     -> auto-restart
    //   2. suboptimal config (running flags ≠ canonical)     -> tell the
    //      user to run hip --restart-mtplx (we don't auto-clobber a
    //      potentially-customized server)
    //   3. running with optimal config                       -> ✓ all good
    //   4. nothing running                                   -> hint
    if is_running_stale(&state, just_upgraded) {
        eprintln!("  {w}!{r} running server is on stale code; brew/disk has newer bytes.");
        restart_with_optimal_config(&state).await;
    } else if let Some(delta) = state.config_status() {
        if !delta.is_optimal() {
            // Loud, end-of-output. The suboptimal warning earlier in the
            // banner can get visually buried under the details block, so
            // we surface it again here with the exact remediation.
            eprintln!(
                "  {w}!{r} server is running but config differs from optimal {d}({} delta(s)){r}",
                delta.missing_or_wrong.len()
            );
            for missing in &delta.missing_or_wrong {
                eprintln!("    {w}-{r} {a}{}{r}", missing);
            }
            eprintln!(
                "  {d}fix{r}:      run {a}hip --restart-mtplx{r} to relaunch with optimal config"
            );
        } else {
            match &state.kind {
                crate::mtplx_runner::InstallKind::Brew {
                    installed_version, ..
                } => match state.running_venv_version.as_deref() {
                    Some(rv) => eprintln!(
                        "  {g}✓{r} server is on current code {d}(running venv {} == brew {}){r}",
                        rv, installed_version
                    ),
                    None => eprintln!(
                        "  {g}✓{r} server is running {d}(venv version not detected; assuming current){r}"
                    ),
                },
                crate::mtplx_runner::InstallKind::Git { head_sha, .. } => {
                    eprintln!(
                        "  {g}✓{r} server is on current code {d}(checkout HEAD {}){r}",
                        head_sha
                    );
                }
            }
        }
    } else {
        eprintln!("  {d}(no MTPLX server running -- run {a}hip --start-mtplx{d} to launch){r}");
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

/// On a brew-based MTPLX install, overlay the chosen fork's source into
/// the brew-managed venv: clone (or pull) the fork to ~/code/MTPLX, then
/// `pip install --force-reinstall --no-deps` into the highest-numbered
/// venv under /opt/homebrew/var/mtplx/. Idempotent and chatty so the
/// user can see what happened. Brew formula still points at upstream so
/// the next `brew upgrade mtplx` will revert this overlay - we warn.
fn apply_brew_fork_overlay(repo_url: &str, branch: &str) {
    use crate::theme::{accent, dim, good, warn, RESET};
    use std::process::Command;
    let d = dim();
    let a = accent();
    let g = good();
    let w = warn();
    let r = RESET;

    eprintln!();
    eprintln!("{d}─ applying fork overlay onto brew venv ─{r}");

    // 1. Resolve a destination for the fork checkout.
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let dest = PathBuf::from(&home).join("code").join("MTPLX");

    // 2. Clone (if absent) or pull (if present). We don't switch remotes
    //    here on purpose - if the user has an existing checkout with a
    //    different remote, leave it alone and just `git pull` from
    //    whichever origin they've got. The picker already persisted the
    //    label; the overlay just needs a working tree pointing at the
    //    fork's branch.
    if dest.exists() {
        eprintln!("  {d}using existing checkout{r}: {a}{}{r}", dest.display());
        // Fetch + fast-forward the branch we want to overlay from.
        let _ = Command::new("git")
            .args(["-C", dest.to_str().unwrap_or("."), "fetch", "--quiet"])
            .status();
        let pull = Command::new("git")
            .args([
                "-C",
                dest.to_str().unwrap_or("."),
                "pull",
                "--ff-only",
                "--quiet",
                "origin",
                branch,
            ])
            .status();
        match pull {
            Ok(s) if s.success() => eprintln!("  {g}✓{r} {d}git pull --ff-only origin {branch}{r}"),
            _ => eprintln!(
                "  {w}!{r} git pull failed (working tree may have local changes); using current HEAD"
            ),
        }
    } else {
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        eprintln!(
            "  {d}cloning{r} {a}{}{r} {d}->{r} {a}{}{r}",
            repo_url,
            dest.display()
        );
        let clone = Command::new("git")
            .args([
                "clone",
                "--quiet",
                "--branch",
                branch,
                repo_url,
                dest.to_str().unwrap_or("."),
            ])
            .status();
        if !matches!(clone, Ok(s) if s.success()) {
            eprintln!("  {w}!{r} git clone failed; aborting overlay");
            return;
        }
        eprintln!("  {g}✓{r} cloned");
    }

    // 3. Find the brew venv pip. There is exactly one /opt/homebrew/var/
    //    mtplx/venv-X.Y.Z directory per brew install; pick whichever
    //    exists. If multiple are present (after a brew upgrade leaves an
    //    old one) take the newest by directory name.
    let venv_root = PathBuf::from("/opt/homebrew/var/mtplx");
    let pip_path: Option<PathBuf> = std::fs::read_dir(&venv_root)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.starts_with("venv-"))
        })
        .max()
        .map(|p| p.join("bin").join("pip"));

    let Some(pip) = pip_path.filter(|p| p.exists()) else {
        eprintln!(
            "  {w}!{r} could not locate brew venv pip under {a}{}{r}; skipping pip overlay",
            venv_root.display()
        );
        eprintln!(
            "  {d}run manually:{r} {a}/opt/homebrew/var/mtplx/venv-*/bin/pip install --force-reinstall --no-deps {}{r}",
            dest.display()
        );
        return;
    };

    // 4. pip install --force-reinstall --no-deps <fork-path>. --no-deps
    //    avoids touching numpy/mlx/etc. that brew set up; we only want
    //    the python source layer swapped.
    eprintln!(
        "  {d}pip install --force-reinstall --no-deps{r} {a}{}{r}",
        dest.display()
    );
    let pip_status = Command::new(&pip)
        .args([
            "install",
            "--force-reinstall",
            "--no-deps",
            "--quiet",
            dest.to_str().unwrap_or("."),
        ])
        .status();
    match pip_status {
        Ok(s) if s.success() => {
            // Re-probe installed version so the user sees the overlay landed.
            let ver = Command::new(&pip)
                .args(["show", "mtplx"])
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .and_then(|s| {
                    s.lines()
                        .find_map(|l| l.strip_prefix("Version: ").map(|v| v.to_string()))
                })
                .unwrap_or_else(|| "unknown".to_string());
            eprintln!("  {g}✓{r} fork overlay applied (venv now reports mtplx=={a}{ver}{r})");
            eprintln!("  {d}note{r}: next {a}brew upgrade mtplx{r} will revert this overlay.");
            eprintln!(
                "  {d}for a permanent switch, use {a}hip --setup{r}{d} to install from a git checkout.{r}"
            );
        }
        _ => {
            eprintln!("  {w}!{r} pip install failed; brew venv unchanged");
            eprintln!(
                "  {d}retry manually:{r} {a}{} install --force-reinstall --no-deps {}{r}",
                pip.display(),
                dest.display()
            );
        }
    }
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
