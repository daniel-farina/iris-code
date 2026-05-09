//! Color theme for hippo-code's TUI surfaces.
//!
//! Three themes:
//! - `River` (default): slate gray + steel blue + mossy green + tusk ivory.
//!   Built around 256-color ANSI codes.
//! - `Light`: muted accents tuned for light terminal backgrounds.
//! - `Mono`: no color at all - bold/dim/reset only.
//!
//! Resolution order:
//! 1. Runtime override (set by `:theme` REPL command -> `set_runtime`)
//! 2. `HIPPO_THEME` / `IRIS_THEME` / `MLX_CODE_THEME` env var
//! 3. Default = `River`
//!
//! Public surface is a small set of named accessors (`accent()`, `good()`,
//! `dim()`, etc.). Each returns the ANSI prefix; callers must append `\x1b[0m`
//! to reset, or use the `paint()` helper.

use once_cell::sync::Lazy;
use std::sync::Mutex;

/// Themes:
/// - `River` (default): hippo / river-water palette - slate gray, steel
///   blue, mossy green, tusk ivory. 256-color ANSI codes.
/// - `Light`: muted accents tuned for light terminal backgrounds.
/// - `Mono`: no color at all - bold/dim/reset only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    River,
    Light,
    Mono,
}

static RUNTIME_OVERRIDE: Lazy<Mutex<Option<Theme>>> = Lazy::new(|| Mutex::new(None));

impl Theme {
    pub fn parse(s: &str) -> Option<Theme> {
        match s.trim().to_ascii_lowercase().as_str() {
            "river" | "dark" | "hippo" | "blue" | "slate" => Some(Theme::River),
            "light" => Some(Theme::Light),
            "mono" | "none" | "off" => Some(Theme::Mono),
            _ => None,
        }
    }
    pub fn name(&self) -> &'static str {
        match self {
            Theme::River => "river",
            Theme::Light => "light",
            Theme::Mono => "mono",
        }
    }
}

pub fn current() -> Theme {
    if let Some(t) = *RUNTIME_OVERRIDE.lock().unwrap() {
        return t;
    }
    std::env::var("HIPPO_THEME")
        .or_else(|_| std::env::var("IRIS_THEME"))
        .or_else(|_| std::env::var("MLX_CODE_THEME"))
        .ok()
        .and_then(|s| Theme::parse(&s))
        .unwrap_or(Theme::River)
}

pub fn set_runtime(t: Theme) {
    *RUNTIME_OVERRIDE.lock().unwrap() = Some(t);
}

/// Reset code: ends any open color sequence.
pub const RESET: &str = "\x1b[0m";

// River-hippo 256-color palette (River theme):
//   24  = deep teal-blue (#005f87)  - depth / shadow
//   67  = steel blue (#5f87af)      - main accent (river surface)
//   109 = gray-cyan (#87afaf)       - lighter river / mist
//   102 = medium gray (#878787)     - hippo body
//   65  = mossy green (#5f875f)     - bank vegetation / good
//   178 = mustard / dust (#d7af00)  - warn
//   229 = pale ivory (#ffffaf)      - tusks / highlight
//
// Use `\x1b[38;5;Nm` for foreground.

pub fn dim() -> &'static str {
    match current() {
        Theme::River | Theme::Light | Theme::Mono => "\x1b[2m",
    }
}
pub fn accent() -> &'static str {
    match current() {
        Theme::River => "\x1b[38;5;67m", // steel blue - the river
        Theme::Light => "\x1b[0;34m",    // dark blue (legible on white)
        Theme::Mono => "\x1b[1m",
    }
}
pub fn good() -> &'static str {
    match current() {
        Theme::River => "\x1b[38;5;65m", // mossy green
        Theme::Light => "\x1b[0;32m",
        Theme::Mono => "",
    }
}
pub fn warn() -> &'static str {
    match current() {
        Theme::River => "\x1b[38;5;178m", // mustard / sun-on-water
        Theme::Light => "\x1b[0;33m",
        Theme::Mono => "",
    }
}
#[allow(dead_code)]
pub fn bad() -> &'static str {
    match current() {
        Theme::River => "\x1b[38;5;167m", // soft red, sits well on slate
        Theme::Light => "\x1b[0;31m",
        Theme::Mono => "\x1b[1m",
    }
}
pub fn highlight() -> &'static str {
    match current() {
        Theme::River => "\x1b[38;5;109m", // gray-cyan, lighter river
        Theme::Light => "\x1b[0;35m",
        Theme::Mono => "",
    }
}
#[allow(dead_code)]
pub fn thinking() -> &'static str {
    match current() {
        Theme::River => "\x1b[2;38;5;102m", // dim + medium hippo gray
        Theme::Light => "\x1b[2;90m",
        Theme::Mono => "\x1b[2m",
    }
}

/// River-gradient stops for the logo block letters: deep teal-blue at the
/// top, fading through steel blue to pale gray-cyan at the bottom. Six
/// stops match the 6-line block font.
pub const RIVER_GRADIENT: &[&str] = &[
    "\x1b[38;5;24m",  // deep teal-blue (river depth)
    "\x1b[38;5;25m",  // mid teal
    "\x1b[38;5;67m",  // steel blue
    "\x1b[38;5;73m",  // lighter steel
    "\x1b[38;5;109m", // gray-cyan
    "\x1b[38;5;152m", // pale river-mist
];

/// Wrap `inner` with `prefix` and RESET. Convenience for short colored text.
#[allow(dead_code)]
pub fn paint(prefix: &str, inner: &str) -> String {
    if prefix.is_empty() {
        return inner.to_string();
    }
    format!("{}{}{}", prefix, inner, RESET)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests touch a process-global override; serialize so they don't race.
    static TEST_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

    #[test]
    fn parse_accepts_aliases() {
        assert_eq!(Theme::parse("river"), Some(Theme::River));
        assert_eq!(Theme::parse("dark"), Some(Theme::River)); // dark aliases
        assert_eq!(Theme::parse("hippo"), Some(Theme::River));
        assert_eq!(Theme::parse("slate"), Some(Theme::River));
        assert_eq!(Theme::parse("Light"), Some(Theme::Light));
        assert_eq!(Theme::parse("MONO"), Some(Theme::Mono));
        assert_eq!(Theme::parse("none"), Some(Theme::Mono));
        assert_eq!(Theme::parse("rainbow"), None);
    }

    #[test]
    fn runtime_override_beats_env() {
        let _g = TEST_LOCK.lock().unwrap();
        std::env::set_var("HIPPO_THEME", "river");
        set_runtime(Theme::Light);
        assert_eq!(current(), Theme::Light);
        *RUNTIME_OVERRIDE.lock().unwrap() = None;
        std::env::remove_var("HIPPO_THEME");
    }

    #[test]
    fn mono_emits_no_color_codes_for_good_warn() {
        let _g = TEST_LOCK.lock().unwrap();
        set_runtime(Theme::Mono);
        assert_eq!(good(), "");
        assert_eq!(warn(), "");
        assert_eq!(highlight(), "");
        assert!(accent().contains("\x1b[1m"));
        *RUNTIME_OVERRIDE.lock().unwrap() = None;
    }

    #[test]
    fn river_emits_256_color_codes() {
        let _g = TEST_LOCK.lock().unwrap();
        set_runtime(Theme::River);
        assert!(
            accent().contains("\x1b[38;5;67m"),
            "expected steel-blue accent, got {:?}",
            accent()
        );
        assert!(
            highlight().contains("\x1b[38;5;109m"),
            "expected gray-cyan highlight, got {:?}",
            highlight()
        );
        assert!(
            warn().contains("\x1b[38;5;178m"),
            "expected mustard warn, got {:?}",
            warn()
        );
        *RUNTIME_OVERRIDE.lock().unwrap() = None;
    }

    #[test]
    fn paint_wraps_with_reset() {
        assert_eq!(paint("\x1b[1m", "hi"), "\x1b[1mhi\x1b[0m");
        assert_eq!(paint("", "plain"), "plain");
    }
}
