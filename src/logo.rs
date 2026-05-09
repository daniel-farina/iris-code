//! HIPPO-CODE startup logo.
//!
//! Renders a purple-tone hippo silhouette (truecolor) bundled at
//! `assets/hippo-logo.txt`. The asset is a pre-rendered ANSI-colored
//! ASCII drawing - we just stream it to stderr.
//!
//! Skipped when stderr isn't a TTY, when `--quiet`/`MLX_CODE_NO_PRETTY=1`
//! is set, or when `HIPPO_NO_LOGO=1` / `IRIS_NO_LOGO=1` is set.

use crate::theme::{self, RESET};

/// Pre-rendered hippo silhouette with embedded truecolor ANSI escapes.
/// Source: assets/hippo-logo.txt (originally from silica/game/iris-recreation).
const HIPPO_LOGO: &str = include_str!("../assets/hippo-logo.txt");

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
    let trimmed = HIPPO_LOGO.trim_end_matches('\n');
    eprintln!("{}", trimmed);
    eprintln!();
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
    fn logo_asset_is_nonempty() {
        assert!(
            !HIPPO_LOGO.is_empty(),
            "hippo-logo.txt must ship with the binary"
        );
    }

    #[test]
    fn logo_asset_contains_ansi_escapes() {
        // Sanity: the asset is supposed to be pre-colored. If somebody
        // accidentally strips the escapes during a copy, fail loudly.
        assert!(
            HIPPO_LOGO.contains('\x1b'),
            "hippo-logo.txt should contain raw ANSI escape codes"
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
