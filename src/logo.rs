//! HIPPO-CODE startup logo.
//!
//! Renders a purple-tone hippo silhouette (truecolor) bundled at
//! `assets/hippo-logo.txt`. The asset is a pre-rendered ANSI-colored
//! ASCII drawing - we just stream it to stderr.
//!
//! Skipped when stderr isn't a TTY, when `--quiet`/`MLX_CODE_NO_PRETTY=1`
//! is set, or when `HIPPO_NO_LOGO=1` / `IRIS_NO_LOGO=1` is set.

use crate::theme::{self, RESET};

/// Full banner: hippo silhouette + "hippo / code" wordmark to the right.
/// Max visible width: ~83 cols. Used when the terminal is wide enough.
const HIPPO_LOGO_FULL: &str = include_str!("../assets/hippo-logo.txt");

/// Compact banner: hippo silhouette only, no wordmark.
/// Max visible width: ~46 cols. Used when the terminal is too narrow for
/// the full banner but still wide enough for the art alone.
const HIPPO_LOGO_COMPACT: &str = include_str!("../assets/hippo-only.txt");

/// Minimum visible width (in cols) for the full banner.
const FULL_MIN_COLS: u16 = 83;
/// Minimum visible width (in cols) for the compact (art-only) banner.
const COMPACT_MIN_COLS: u16 = 46;

/// Returns true when the logo should be rendered. False on non-TTY,
/// `--quiet` / `MLX_CODE_NO_PRETTY=1`, or explicit `HIPPO_NO_LOGO=1` /
/// `IRIS_NO_LOGO=1`.
pub fn enabled() -> bool {
    use std::io::IsTerminal;
    if !std::io::stderr().is_terminal() {
        return false;
    }
    if std::env::var("HIPPO_NO_LOGO")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        return false;
    }
    if std::env::var("IRIS_NO_LOGO")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        return false;
    }
    if std::env::var("MLX_CODE_NO_PRETTY")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        return false;
    }
    true
}

/// Pick the banner variant for the current terminal width. Returns `None`
/// when the terminal is too narrow for either variant (caller should skip
/// rendering the silhouette and just emit the tagline). When width detection
/// fails, defaults to the compact variant — safer than gambling that the
/// terminal is wide enough for the full banner.
fn pick_variant() -> Option<&'static str> {
    let cols = terminal_size::terminal_size()
        .map(|(terminal_size::Width(w), _)| w)
        .unwrap_or(COMPACT_MIN_COLS);
    if cols >= FULL_MIN_COLS {
        Some(HIPPO_LOGO_FULL)
    } else if cols >= COMPACT_MIN_COLS {
        Some(HIPPO_LOGO_COMPACT)
    } else {
        None
    }
}

/// Print the logo to stderr.
pub fn print() {
    if !enabled() {
        return;
    }
    let dim = theme::dim();
    let acc = theme::accent();
    let r = RESET;

    eprintln!();
    // The asset already ends each row with a reset, but trim trailing
    // blank lines so we own the spacing around the tagline.
    if let Some(art) = pick_variant() {
        eprintln!("{}", art.trim_end_matches('\n'));
        eprintln!();
    }
    eprintln!(
        "          {dim}─ a lean coding agent · {a}MTPLX{dim} · qwen3.6-27b ─{r}",
        dim = dim,
        a = acc,
        r = r,
    );
    eprintln!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logo_assets_are_nonempty() {
        assert!(
            !HIPPO_LOGO_FULL.is_empty(),
            "hippo-logo.txt must ship with the binary"
        );
        assert!(
            !HIPPO_LOGO_COMPACT.is_empty(),
            "hippo-only.txt must ship with the binary"
        );
    }

    #[test]
    fn logo_assets_contain_ansi_escapes() {
        // Sanity: both assets are supposed to be pre-colored. If somebody
        // accidentally strips the escapes during a copy, fail loudly.
        assert!(
            HIPPO_LOGO_FULL.contains('\x1b'),
            "hippo-logo.txt should contain raw ANSI escape codes"
        );
        assert!(
            HIPPO_LOGO_COMPACT.contains('\x1b'),
            "hippo-only.txt should contain raw ANSI escape codes"
        );
    }

    #[test]
    fn hippo_no_logo_env_disables_render() {
        std::env::set_var("HIPPO_NO_LOGO", "1");
        let r = enabled();
        std::env::remove_var("HIPPO_NO_LOGO");
        assert!(!r);
    }
}
