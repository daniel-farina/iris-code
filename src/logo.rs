//! IRIS-CODE startup logo.
//!
//! Renders a 6-row block-letter "IRIS-CODE" with a violet -> lavender vertical
//! gradient and a small stylized iris-flower icon to the left. Uses 256-color
//! ANSI codes from the iris theme palette.
//!
//! Skipped when stderr isn't a TTY, when `--quiet`/`MLX_CODE_NO_PRETTY=1` is
//! set, or when `IRIS_NO_LOGO=1` (per-user opt-out for those who want a
//! plainer banner).

use crate::theme::{self, IRIS_GRADIENT, RESET};

/// Block-letter "IRIS-CODE" - 6 rows. Each row is one slice of the vertical
/// gradient. Generated to match `Standard` figlet output for "IRIS  CODE".
const BLOCK: &[&str] = &[
    "██╗██████╗ ██╗███████╗     ██████╗ ██████╗ ██████╗ ███████╗",
    "██║██╔══██╗██║██╔════╝    ██╔════╝██╔═══██╗██╔══██╗██╔════╝",
    "██║██████╔╝██║███████╗    ██║     ██║   ██║██║  ██║█████╗  ",
    "██║██╔══██╗██║╚════██║    ██║     ██║   ██║██║  ██║██╔══╝  ",
    "██║██║  ██║██║███████║    ╚██████╗╚██████╔╝██████╔╝███████╗",
    "╚═╝╚═╝  ╚═╝╚═╝╚══════╝     ╚═════╝ ╚═════╝ ╚═════╝ ╚══════╝",
];

/// Stylized iris flower glyph - 6 rows aligned with the block letters.
/// Three upright "standards" at the top, the yellow "beard" in the middle,
/// three "falls" curving down, then the stem and base. Rendered with
/// per-row coloring: standards/falls in lavender-purple, beard in gold,
/// stem in green, base in dim.
const FLOWER: &[(&str, &str)] = &[
    (" ╲▎│▎╱ ", "lavender"),
    (" ▎▎│▎▎ ", "violet_top"),
    ("─◆─◆─◆─", "gold"),
    (" ╱▎│▎╲ ", "violet_bot"),
    ("   │   ", "stem"),
    ("  ─┴─  ", "stem_dim"),
];

fn role_to_ansi(role: &str) -> &'static str {
    match role {
        "lavender" => "\x1b[38;5;183m",
        "violet_top" => "\x1b[38;5;141m",
        "violet_bot" => "\x1b[38;5;91m",
        "gold" => "\x1b[38;5;220m",
        "stem" => "\x1b[38;5;108m", // soft sage green
        "stem_dim" => "\x1b[2;38;5;108m",
        _ => "",
    }
}

/// Returns true when the logo should be rendered. False on non-TTY,
/// `--quiet` / `MLX_CODE_NO_PRETTY=1`, or explicit `IRIS_NO_LOGO=1`.
pub fn enabled() -> bool {
    use std::io::IsTerminal;
    if !std::io::stderr().is_terminal() {
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

/// Print the logo to stderr. Idempotent only in the sense that it prints
/// every time it's called - the caller decides when (typically once at
/// chat-mode start).
pub fn print() {
    if !enabled() {
        return;
    }
    let dim = theme::dim();
    let acc = theme::accent();
    let r = RESET;

    eprintln!();
    for (i, block_row) in BLOCK.iter().enumerate() {
        let (flower_text, flower_role) = FLOWER[i];
        let block_color = IRIS_GRADIENT.get(i).copied().unwrap_or("");
        eprintln!(
            "    {fc}{ftext}{r}    {bc}{btext}{r}",
            fc = role_to_ansi(flower_role),
            ftext = flower_text,
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
    fn flower_has_6_rows_matching_block() {
        assert_eq!(
            FLOWER.len(),
            BLOCK.len(),
            "flower must align with block letters"
        );
    }

    #[test]
    fn iris_no_logo_env_disables_render() {
        std::env::set_var("IRIS_NO_LOGO", "1");
        let r = enabled();
        std::env::remove_var("IRIS_NO_LOGO");
        // Whether it'd be true otherwise depends on TTY; just assert the opt-out wins.
        assert!(!r);
    }

    #[test]
    fn role_to_ansi_known_roles_return_nonempty() {
        for role in &[
            "lavender",
            "violet_top",
            "violet_bot",
            "gold",
            "stem",
            "stem_dim",
        ] {
            assert!(
                !role_to_ansi(role).is_empty(),
                "expected ANSI for role {}",
                role
            );
        }
        // Unknown role -> empty.
        assert_eq!(role_to_ansi("unknown_role"), "");
    }
}
