//! Color theme for mlx-code's TUI surfaces.
//!
//! Three themes:
//! - `dark`  (default): bright accents on a dark terminal background
//! - `light`: muted accents that read on white/light backgrounds
//! - `mono`:  no color at all - just bold/dim/reset for accessibility,
//!            log files, or pipes that don't strip ANSI
//!
//! Resolution order:
//! 1. Runtime override (set by `:theme` REPL command -> `set_runtime`)
//! 2. `MLX_CODE_THEME` env var
//! 3. Default = `dark`
//!
//! Public surface is a small set of named accessors (`accent()`, `good()`,
//! `dim()`, etc.). Each returns the ANSI prefix; callers must append `\x1b[0m`
//! to reset, or use the `paint()` helper.

use once_cell::sync::Lazy;
use std::sync::Mutex;

/// Themes:
/// - `Iris` (default): iris-flower palette - deep violet, royal purple, lavender,
///   pale yellow ("beard"), white. Built around 256-color ANSI codes.
/// - `Light`: muted accents tuned for light terminal backgrounds.
/// - `Mono`: no color at all - bold/dim/reset only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme { Iris, Light, Mono }

static RUNTIME_OVERRIDE: Lazy<Mutex<Option<Theme>>> = Lazy::new(|| Mutex::new(None));

impl Theme {
    pub fn parse(s: &str) -> Option<Theme> {
        match s.trim().to_ascii_lowercase().as_str() {
            "iris" | "dark" | "violet" | "purple" => Some(Theme::Iris),
            "light" => Some(Theme::Light),
            "mono" | "none" | "off" => Some(Theme::Mono),
            _ => None,
        }
    }
    pub fn name(&self) -> &'static str {
        match self {
            Theme::Iris  => "iris",
            Theme::Light => "light",
            Theme::Mono  => "mono",
        }
    }
}

pub fn current() -> Theme {
    if let Some(t) = *RUNTIME_OVERRIDE.lock().unwrap() {
        return t;
    }
    std::env::var("MLX_CODE_THEME").or_else(|_| std::env::var("IRIS_THEME")).ok()
        .and_then(|s| Theme::parse(&s))
        .unwrap_or(Theme::Iris)
}

pub fn set_runtime(t: Theme) {
    *RUNTIME_OVERRIDE.lock().unwrap() = Some(t);
}

/// Reset code: ends any open color sequence.
pub const RESET: &str = "\x1b[0m";

// Iris-flower 256-color palette (Iris theme):
//   54  = deep violet (#5f0087) - shadow / stem tone
//   91  = royal purple (#8700af)
//   135 = bright violet (#af5fff)
//   141 = soft violet (#af87ff)  - main accent
//   183 = lavender (#d7afff)     - highlight
//   228 = pale yellow (#ffff87)  - the iris "beard"
//   220 = gold (#ffd700)         - warn / standout
//
// Use `\x1b[38;5;Nm` for foreground.

pub fn dim() -> &'static str {
    match current() {
        Theme::Iris | Theme::Light | Theme::Mono => "\x1b[2m",
    }
}
pub fn accent() -> &'static str {
    match current() {
        Theme::Iris  => "\x1b[38;5;141m",   // soft violet - the iris signature
        Theme::Light => "\x1b[0;35m",       // magenta (legible on white)
        Theme::Mono  => "\x1b[1m",
    }
}
pub fn good() -> &'static str {
    match current() {
        Theme::Iris  => "\x1b[38;5;156m",   // pale green-yellow, complements violet
        Theme::Light => "\x1b[0;32m",
        Theme::Mono  => "",
    }
}
pub fn warn() -> &'static str {
    match current() {
        Theme::Iris  => "\x1b[38;5;220m",   // gold - the iris beard
        Theme::Light => "\x1b[0;33m",
        Theme::Mono  => "",
    }
}
#[allow(dead_code)]
pub fn bad() -> &'static str {
    match current() {
        Theme::Iris  => "\x1b[38;5;203m",   // soft red beside violet
        Theme::Light => "\x1b[0;31m",
        Theme::Mono  => "\x1b[1m",
    }
}
pub fn highlight() -> &'static str {
    match current() {
        Theme::Iris  => "\x1b[38;5;183m",   // lavender
        Theme::Light => "\x1b[0;35m",
        Theme::Mono  => "",
    }
}
#[allow(dead_code)]
pub fn thinking() -> &'static str {
    match current() {
        Theme::Iris  => "\x1b[2;38;5;98m",  // dim + medium violet
        Theme::Light => "\x1b[2;90m",
        Theme::Mono  => "\x1b[2m",
    }
}

/// Iris-gradient stops for the logo block letters: deep violet at the top,
/// fading to lavender at the bottom. Six stops match the 6-line block font.
pub const IRIS_GRADIENT: &[&str] = &[
    "\x1b[38;5;54m",   // deep violet
    "\x1b[38;5;91m",   // royal purple
    "\x1b[38;5;135m",  // bright violet
    "\x1b[38;5;141m",  // soft violet
    "\x1b[38;5;177m",  // light violet
    "\x1b[38;5;183m",  // lavender
];

/// Wrap `inner` with `prefix` and RESET. Convenience for short colored text.
#[allow(dead_code)]
pub fn paint(prefix: &str, inner: &str) -> String {
    if prefix.is_empty() { return inner.to_string(); }
    format!("{}{}{}", prefix, inner, RESET)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests touch a process-global override; serialize so they don't race.
    static TEST_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

    #[test]
    fn parse_accepts_aliases() {
        assert_eq!(Theme::parse("iris"), Some(Theme::Iris));
        assert_eq!(Theme::parse("dark"), Some(Theme::Iris));    // dark aliases to iris
        assert_eq!(Theme::parse("violet"), Some(Theme::Iris));
        assert_eq!(Theme::parse("purple"), Some(Theme::Iris));
        assert_eq!(Theme::parse("Light"), Some(Theme::Light));
        assert_eq!(Theme::parse("MONO"), Some(Theme::Mono));
        assert_eq!(Theme::parse("none"), Some(Theme::Mono));
        assert_eq!(Theme::parse("off"), Some(Theme::Mono));
        assert_eq!(Theme::parse("rainbow"), None);
    }

    #[test]
    fn runtime_override_beats_env() {
        let _g = TEST_LOCK.lock().unwrap();
        std::env::set_var("MLX_CODE_THEME", "dark");
        set_runtime(Theme::Light);
        assert_eq!(current(), Theme::Light);
        // Reset for other tests.
        *RUNTIME_OVERRIDE.lock().unwrap() = None;
        std::env::remove_var("MLX_CODE_THEME");
    }

    #[test]
    fn mono_emits_no_color_codes_for_good_warn() {
        let _g = TEST_LOCK.lock().unwrap();
        set_runtime(Theme::Mono);
        assert_eq!(good(), "");
        assert_eq!(warn(), "");
        assert_eq!(highlight(), "");
        // accent in mono uses bold but not color
        assert!(accent().contains("\x1b[1m"));
        *RUNTIME_OVERRIDE.lock().unwrap() = None;
    }

    #[test]
    fn iris_emits_256_color_codes() {
        let _g = TEST_LOCK.lock().unwrap();
        set_runtime(Theme::Iris);
        // Iris theme uses 256-color codes (\x1b[38;5;Nm), not 16-color (\x1b[0;Nm).
        assert!(accent().contains("\x1b[38;5;141m"), "expected soft-violet accent, got {:?}", accent());
        assert!(highlight().contains("\x1b[38;5;183m"), "expected lavender highlight, got {:?}", highlight());
        assert!(warn().contains("\x1b[38;5;220m"), "expected gold warn, got {:?}", warn());
        *RUNTIME_OVERRIDE.lock().unwrap() = None;
    }

    #[test]
    fn paint_wraps_with_reset() {
        assert_eq!(paint("\x1b[1m", "hi"), "\x1b[1mhi\x1b[0m");
        assert_eq!(paint("", "plain"), "plain");
    }
}
