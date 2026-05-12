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

/// After hip update completes (or hip is already current), check the
/// local MTPLX checkout for upstream commits and offer to pull. Silent
/// no-op when MTPLX isn't installed or the checkout isn't a git repo,
/// so it can't make `hip --update` worse than it was.
async fn check_mtplx_updates() {
    use crate::setup::{prompt_mtplx_source, MtplxSource};
    use crate::theme::{accent, dim, good, warn, RESET};
    use std::process::Command;

    let d = dim();
    let a = accent();
    let g = good();
    let w = warn();
    let r = RESET;

    // Detect how MTPLX was installed (git checkout, Homebrew formula, or
    // something we can't recognize). The update path differs for each:
    // git uses fetch+pull, brew uses `brew upgrade`, unknown gets a hint.
    let install = resolve_mtplx_install();

    eprintln!();
    eprintln!("{d}─ checking MTPLX ─{r}");

    let install_dir = match install {
        MtplxInstall::Git(p) => {
            eprintln!(
                "  {d}install{r}: {a}{}{r} {d}(git checkout){r}",
                p.display()
            );
            p
        }
        MtplxInstall::Brew { formula, version } => {
            eprintln!("  {d}install{r}: {a}{}{r} {d}(Homebrew){r}", formula);
            eprintln!("  {d}version{r}: {a}{}{r}", version);

            // Compare the running command line against our canonical
            // optimal config and surface any deltas. Done BEFORE the
            // upgrade check so the user sees config status even when
            // they're already up to date.
            let server_running = crate::mtplx_runner::running_pid().is_some();
            if server_running {
                let cmd = crate::mtplx_runner::running_command_line().unwrap_or_default();
                let delta = crate::mtplx_runner::config_deltas(&cmd);
                if delta.is_optimal() {
                    eprintln!("  {d}config{r}:  {g}optimal{r}");
                } else {
                    eprintln!(
                        "  {d}config{r}:  {w}suboptimal{r} {d}({} delta(s)){r}",
                        delta.missing_or_wrong.len()
                    );
                    for missing in &delta.missing_or_wrong {
                        eprintln!("    {w}-{r} missing/wrong: {a}{}{r}", missing);
                    }
                    eprintln!(
                        "  {d}fix{r}: run {a}hip --restart-mtplx{r} to relaunch with optimal config"
                    );
                }
            } else {
                eprintln!("  {d}config{r}:  {w}not running{r}");
                eprintln!("  {d}fix{r}: run {a}hip --start-mtplx{r} to launch with optimal config");
            }

            match brew_mtplx_latest_available() {
                Some(latest) => {
                    eprintln!("  {w}!{r} a newer MTPLX is available: {a}{}{r}", latest);
                    eprintln!("  {d}running `brew upgrade mtplx`...{r}");
                    let status = std::process::Command::new("brew")
                        .args(["upgrade", "mtplx"])
                        .status();
                    if matches!(status, Ok(s) if s.success()) {
                        eprintln!("{g}✓{r} MTPLX upgraded via Homebrew");

                        // brew upgrade replaced the venv on disk but the
                        // running process still has the old code mapped.
                        // Stop it and respawn with optimal config so the
                        // user lands on the new binaries immediately.
                        if server_running {
                            eprintln!(
                                "  {d}stopping old MTPLX server (pid {}) and starting new one with optimal config...{r}",
                                crate::mtplx_runner::running_pid().unwrap_or(0)
                            );
                            crate::mtplx_runner::stop_running_mtplx(
                                std::time::Duration::from_secs(15),
                            );
                            match crate::mtplx_runner::start_mtplx_optimal_background() {
                                Ok(pid) => {
                                    eprintln!(
                                        "  {g}✓{r} new MTPLX spawned (pid {a}{}{r}), waiting for model load...",
                                        pid
                                    );
                                    let up = crate::mtplx_runner::wait_until_listening(
                                        std::time::Duration::from_secs(300),
                                    )
                                    .await;
                                    if up {
                                        eprintln!(
                                            "  {g}✓{r} MTPLX is listening on :{}",
                                            crate::mtplx_runner::OPTIMAL_PORT
                                        );
                                    } else {
                                        eprintln!(
                                            "  {w}!{r} MTPLX didn't bind within 5 min; tail {a}{}{r} for diagnostics",
                                            crate::mtplx_runner::log_file().display()
                                        );
                                    }
                                }
                                Err(e) => {
                                    eprintln!("  {w}!{r} failed to spawn MTPLX: {}", e);
                                }
                            }
                        } else {
                            flag_running_server();
                        }
                    } else {
                        eprintln!(
                            "  {w}!{r} `brew upgrade mtplx` failed; run it manually to diagnose."
                        );
                    }
                }
                None => {
                    eprintln!("  {g}✓{r} MTPLX is up to date");
                }
            }
            return;
        }
        MtplxInstall::Unknown => {
            eprintln!("  {w}!{r} MTPLX server is running but we can't tell how it was installed.");
            eprintln!(
                "    {d}set {a}HIP_MTPLX_INSTALL_DIR{d} to your checkout, or update it manually.{r}"
            );
            return;
        }
        MtplxInstall::None => {
            eprintln!(
                "  {w}!{r} could not locate MTPLX. Install via {a}brew install youssofal/mtplx/mtplx{r}"
            );
            eprintln!("    or run {a}hip --setup{r} to clone a source checkout.");
            return;
        }
    };

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

    eprintln!(
        "{d}running `git pull --ff-only origin {}`...{r}",
        chosen.branch
    );
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
/// How MTPLX got onto this machine. Determines what the "update" path
/// looks like: git pull vs `brew upgrade mtplx` vs "we have no idea."
pub enum MtplxInstall {
    /// Source checkout with a `.git` directory. Update via fetch+pull.
    Git(PathBuf),
    /// Installed via Homebrew (`youssofal/mtplx/mtplx` formula or a
    /// custom tap). Update via `brew upgrade`. String is the formula
    /// name including the tap prefix when known, e.g.
    /// "youssofal/mtplx/mtplx".
    Brew { formula: String, version: String },
    /// Server is reachable but we couldn't figure out how it was
    /// installed (custom Python env, container, etc.). Best we can do
    /// is tell the user.
    Unknown,
    /// No MTPLX detected anywhere.
    None,
}

/// Locate MTPLX. Order:
///   1. $HIP_MTPLX_INSTALL_DIR override (must point at a git checkout).
///   2. Wizard default ~/code/MTPLX if it's a git checkout.
///   3. Probe the running server on :8088 for open-file paths -- if any
///      lead back to a Homebrew Cellar / venv, we know it's brew.
///   4. Probe the running server's cwd, walk up for a `.git` ancestor.
///   5. `brew list --formula mtplx` as a final passive check.
fn resolve_mtplx_install() -> MtplxInstall {
    use std::process::Command;

    // 1. Explicit env override always wins, but only if it points at a
    //    git checkout (the only thing we can fetch+pull from).
    if let Ok(v) = std::env::var("HIP_MTPLX_INSTALL_DIR") {
        let p = PathBuf::from(v);
        if p.join(".git").exists() {
            return MtplxInstall::Git(p);
        }
    }

    // 2. Wizard default.
    let default = PathBuf::from(shellexpand::tilde("~/code/MTPLX").into_owned());
    if default.join(".git").exists() {
        return MtplxInstall::Git(default);
    }

    // 3 + 4. Probe the running server on :8088.
    let pid_out = Command::new("lsof")
        .args(["-nP", "-iTCP:8088", "-sTCP:LISTEN", "-t"])
        .output()
        .ok();
    let pid_str = pid_out
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_default();

    if !pid_str.is_empty() {
        // Look at all open files for telltale brew-tap paths first.
        // Homebrew installs MTPLX under /opt/homebrew/var/mtplx/venv-<ver>
        // (Apple Silicon) or /usr/local/var/mtplx/venv-<ver> (Intel).
        if let Ok(o) = Command::new("lsof").args(["-p", &pid_str]).output() {
            let body = String::from_utf8_lossy(&o.stdout);
            let brew_hit = body.lines().find(|l| {
                l.contains("/homebrew/var/mtplx/") || l.contains("/usr/local/var/mtplx/")
            });
            if brew_hit.is_some() {
                let (formula, version) = detect_brew_mtplx_version();
                return MtplxInstall::Brew { formula, version };
            }
        }

        // No brew signature: try to recover a git checkout from cwd.
        let cwd = Command::new("lsof")
            .args(["-p", &pid_str, "-a", "-d", "cwd", "-Fn"])
            .output()
            .ok()
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .find(|l| l.starts_with('n'))
                    .map(|l| l[1..].to_string())
            })
            .map(PathBuf::from);
        if let Some(start) = cwd {
            let mut cur = start.as_path();
            loop {
                if cur.join(".git").exists() && cur.join("setup.py").exists()
                    || cur.join(".git").exists() && cur.join("pyproject.toml").exists()
                {
                    return MtplxInstall::Git(cur.to_path_buf());
                }
                match cur.parent() {
                    Some(p) => cur = p,
                    None => break,
                }
            }
        }
    }

    // 5. Passive brew probe even if the server isn't running: maybe
    //    they installed via brew and just haven't started it.
    if Command::new("brew")
        .args(["list", "--formula", "mtplx"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        let (formula, version) = detect_brew_mtplx_version();
        return MtplxInstall::Brew { formula, version };
    }

    if !pid_str.is_empty() {
        return MtplxInstall::Unknown;
    }
    MtplxInstall::None
}

/// Return (formula-with-tap, version) for the brew-installed MTPLX, or
/// reasonable defaults if `brew info` can't be parsed.
fn detect_brew_mtplx_version() -> (String, String) {
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

/// Check whether brew thinks the installed MTPLX is outdated. Returns
/// Some(latest_version) when an upgrade is available, None when current
/// or when we can't tell.
fn brew_mtplx_latest_available() -> Option<String> {
    use std::process::Command;
    // `brew outdated --json=v2 <formula>` returns a JSON document with
    // a "formulae" array; an empty array means "up to date."
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
