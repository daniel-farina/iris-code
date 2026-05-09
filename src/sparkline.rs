//! Tiny ASCII/Unicode formatting helpers.
//!
//! - `render(values)`: Unicode-block sparkline normalized to min/max,
//!   with safe fallback when min == max.
//! - `format_age(seconds)`: human-readable "7s ago" / "13m ago" / "2h ago"
//!   / "4d ago" cascade, used by `peek_log` and the `:tps` REPL view.
//!
//! Same character set as `tools/bench/trend.py:sparkline()` so a series
//! renders identically in Python (offline analysis) and Rust (REPL).

#[allow(dead_code)]
const BLOCKS: &[char] = &[' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Render `values` as a Unicode-block sparkline using min/max normalization.
/// Returns an empty string for empty input. When all values are equal
/// (within 0.001), returns a row of mid-block characters.
#[allow(dead_code)]
pub fn render(values: &[f64]) -> String {
    if values.is_empty() { return String::new(); }
    let lo = values.iter().cloned().fold(f64::INFINITY, f64::min);
    let hi = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    if (hi - lo).abs() < 0.001 {
        return std::iter::repeat(BLOCKS[4]).take(values.len()).collect();
    }
    values.iter().map(|v| {
        let idx = ((v - lo) / (hi - lo) * 8.0).round() as usize;
        BLOCKS[idx.min(8)]
    }).collect()
}

/// Format a duration in seconds as "Ns ago" / "Nm ago" / "Nh ago" / "Nd ago".
/// `seconds` is the elapsed time since some past event (typically
/// `now - ts_unix`). Always returns a non-negative-looking string; pass 0
/// for placeholder cases.
#[allow(dead_code)]
pub fn format_age(seconds: u64) -> String {
    if seconds < 60 { return format!("{}s ago", seconds); }
    if seconds < 3600 { return format!("{}m ago", seconds / 60); }
    if seconds < 86400 { return format!("{}h ago", seconds / 3600); }
    format!("{}d ago", seconds / 86400)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_empty_returns_empty() {
        assert_eq!(render(&[]), "");
    }

    #[test]
    fn render_constant_returns_mid_block_row() {
        let s = render(&[5.0, 5.0, 5.0]);
        assert_eq!(s.chars().count(), 3);
        assert!(s.chars().all(|c| c == '▄'), "expected all mid-block, got: {:?}", s);
    }

    #[test]
    fn render_ascending_returns_increasing_blocks() {
        let s: Vec<char> = render(&[1.0, 2.0, 3.0, 4.0, 5.0]).chars().collect();
        assert_eq!(s.len(), 5);
        // First should be the lowest block (space), last should be full.
        assert_eq!(s[0], ' ');
        assert_eq!(s[4], '█');
        // Monotonically non-decreasing in BLOCKS index.
        for w in s.windows(2) {
            let a = BLOCKS.iter().position(|&c| c == w[0]).unwrap();
            let b = BLOCKS.iter().position(|&c| c == w[1]).unwrap();
            assert!(a <= b, "non-monotonic: {} -> {}", w[0], w[1]);
        }
    }

    #[test]
    fn render_min_max_endpoints_are_low_and_high() {
        // Even with noise, the extremes should map to lowest/highest blocks.
        let s: Vec<char> = render(&[0.0, 0.5, 1.0, 0.25, 0.75]).chars().collect();
        assert_eq!(s[0], ' ');  // 0.0 -> lowest
        assert_eq!(s[2], '█');  // 1.0 -> highest
    }

    #[test]
    fn format_age_seconds() {
        assert_eq!(format_age(0), "0s ago");
        assert_eq!(format_age(30), "30s ago");
        assert_eq!(format_age(59), "59s ago");
    }

    #[test]
    fn format_age_minutes() {
        assert_eq!(format_age(60), "1m ago");
        assert_eq!(format_age(3599), "59m ago");
    }

    #[test]
    fn format_age_hours() {
        assert_eq!(format_age(3600), "1h ago");
        assert_eq!(format_age(86399), "23h ago");
    }

    #[test]
    fn format_age_days() {
        assert_eq!(format_age(86400), "1d ago");
        assert_eq!(format_age(86400 * 7), "7d ago");
    }
}
