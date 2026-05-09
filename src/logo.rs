//! HIPPO-CODE startup logo.
//!
//! Renders a 6-row block-letter "HIPPO" with a deep-teal -> pale-river
//! vertical gradient and a small stylized hippo-in-river glyph to the left.
//! Uses 256-color ANSI codes from the river theme palette.
//!
//! Skipped when stderr isn't a TTY, when `--quiet`/`MLX_CODE_NO_PRETTY=1`
//! is set, or when `HIPPO_NO_LOGO=1` / `IRIS_NO_LOGO=1` is set.

use crate::theme::{self, RESET, RIVER_GRADIENT};

/// Block-letter "HIPPO  CODE" - 6 rows. Each row is one slice of the vertical
/// gradient. Hand-aligned to a Standard-figlet style.
const BLOCK: &[&str] = &[
    "██╗  ██╗██╗██████╗ ██████╗  ██████╗     ██████╗ ██████╗ ██████╗ ███████╗",
    "██║  ██║██║██╔══██╗██╔══██╗██╔═══██╗   ██╔════╝██╔═══██╗██╔══██╗██╔════╝",
    "███████║██║██████╔╝██████╔╝██║   ██║   ██║     ██║   ██║██║  ██║█████╗  ",
    "██╔══██║██║██╔═══╝ ██╔═══╝ ██║   ██║   ██║     ██║   ██║██║  ██║██╔══╝  ",
    "██║  ██║██║██║     ██║     ╚██████╔╝   ╚██████╗╚██████╔╝██████╔╝███████╗",
    "╚═╝  ╚═╝╚═╝╚═╝     ╚═╝      ╚═════╝     ╚═════╝ ╚═════╝ ╚═════╝ ╚══════╝",
];

/// Stylized hippo glyph - a hippo head poking out of river ripples. 6 rows,
/// aligned with the block letters. Per-row coloring keeps the eyes/snout
/// readable while the water blends with the dim theme.
const GLYPH: &[(&str, &str)] = &[
    ("  ~~~~~  ", "ripple"), // distant ripples
    ("  _____  ", "head"),   // top of head emerging
    (" /◐ ◐\\  ", "head"),   // eyes
    ("│ ●●● │  ", "snout"),  // nostrils + snout
    (" \\___/   ", "head"),  // chin / lower jaw
    (" ~~~~~~~ ", "water"),  // water surface
];

fn role_to_ansi(role: &str) -> &'static str {
    match role {
        "ripple" => "\x1b[38;5;152m", // pale river-mist
        "head" => "\x1b[38;5;102m",   // hippo gray
        "snout" => "\x1b[38;5;138m",  // warm gray-pink (snout flesh)
        "water" => "\x1b[38;5;67m",   // steel blue river
        _ => "",
    }
}

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
    for (i, block_row) in BLOCK.iter().enumerate() {
        let (glyph_text, glyph_role) = GLYPH[i];
        let block_color = RIVER_GRADIENT.get(i).copied().unwrap_or("");
        eprintln!(
            "    {gc}{gtext}{r}    {bc}{btext}{r}",
            gc = role_to_ansi(glyph_role),
            gtext = glyph_text,
            bc = block_color,
            btext = block_row,
            r = r,
        );
    }
    eprintln!();
    eprintln!(
        "                  {dim}─ a lean coding agent · {a}MTPLX{dim} · qwen3.6-27b ─{r}",
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
    fn block_has_6_rows() {
        assert_eq!(BLOCK.len(), 6);
    }

    #[test]
    fn glyph_has_6_rows_matching_block() {
        assert_eq!(
            GLYPH.len(),
            BLOCK.len(),
            "glyph must align with block letters"
        );
    }

    #[test]
    fn hippo_no_logo_env_disables_render() {
        std::env::set_var("HIPPO_NO_LOGO", "1");
        let r = enabled();
        std::env::remove_var("HIPPO_NO_LOGO");
        assert!(!r);
    }

    #[test]
    fn role_to_ansi_known_roles_return_nonempty() {
        for role in &["ripple", "head", "snout", "water"] {
            assert!(
                !role_to_ansi(role).is_empty(),
                "expected ANSI for role {}",
                role
            );
        }
        assert_eq!(role_to_ansi("unknown_role"), "");
    }
}
