//! Sticky bottom status bar via ANSI DECSTBM (scroll region).
//!
//! Sets up the terminal so that:
//! - The bottom row is reserved for the live metrics line
//! - Streamed output above scrolls normally inside the region
//! - User scroll-up navigates the scrollback above the region; the bar
//!   stays fixed at the bottom of the viewport
//!
//! On drop / panic / Ctrl-C the scroll region is restored and the cursor
//! moved past the bar so subsequent shell prompts don't overwrite it.

use once_cell::sync::Lazy;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static GUARD_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Cached terminal height when the region was set up. Used by `paint_bottom`
/// so we don't query the terminal size on every render (cheap but not free).
static HEIGHT: Lazy<Mutex<u16>> = Lazy::new(|| Mutex::new(0));

/// True if stderr is a TTY and the user hasn't opted out via env var.
pub fn supported() -> bool {
    use std::io::IsTerminal;
    std::io::stderr().is_terminal()
        && std::env::var("MLX_CODE_NO_STICKY")
            .map(|v| v != "1")
            .unwrap_or(true)
        && std::env::var("MLX_CODE_NO_LIVE_TPS")
            .map(|v| v != "1")
            .unwrap_or(true)
}

/// Install the scroll region. Idempotent; safe to call multiple times.
/// Returns true if the region was activated (i.e. supported and not already on).
pub fn enter() -> bool {
    if !supported() {
        return false;
    }
    if ACTIVE.swap(true, Ordering::SeqCst) {
        return true;
    }
    install_safety_guard();
    let (cols, rows) = match terminal_size::terminal_size() {
        Some((terminal_size::Width(w), terminal_size::Height(h))) => (w, h),
        None => return false,
    };
    if rows < 5 {
        ACTIVE.store(false, Ordering::SeqCst);
        return false;
    }
    *HEIGHT.lock().unwrap() = rows;
    let _ = cols; // not used, but kept for future right-aligned text
    let mut err = std::io::stderr();
    // Set scroll region to lines 1..=(rows-1), reserving line `rows` for the bar.
    let _ = write!(err, "\x1b[1;{};r", rows - 1);
    // Move cursor inside region so subsequent output starts there.
    let _ = write!(err, "\x1b[{};1H", rows - 1);
    let _ = err.flush();
    true
}

/// Paint a single line at the reserved bottom row. Cursor is restored to
/// wherever it was before the call. `raw` is a string that may contain ANSI
/// escapes; padding is computed from its visible (escape-stripped) length.
pub fn paint_bottom(raw: &str, visible_len: usize) {
    paint_bottom_with_right(raw, visible_len, "", 0)
}

/// Paint primary text on the left and right-aligned secondary text
/// (e.g. cache hit rate) at the same row. If they would collide given the
/// current terminal width, the right chunk is silently dropped.
pub fn paint_bottom_with_right(left: &str, left_visible: usize, right: &str, right_visible: usize) {
    if !ACTIVE.load(Ordering::SeqCst) {
        return;
    }
    let h = *HEIGHT.lock().unwrap();
    if h == 0 {
        return;
    }
    let cols = terminal_size::terminal_size()
        .map(|(w, _)| w.0 as usize)
        .unwrap_or(80);
    let mut err = std::io::stderr();
    // Save cursor, move to row, clear it, paint left, then optionally right.
    let _ = write!(err, "\x1b7\x1b[{};1H\x1b[2K{}", h, left);
    if !right.is_empty() && left_visible + right_visible + 2 < cols {
        // Position cursor so the right chunk ends exactly at the last column.
        // Cursor cols are 1-based.
        let right_start_col = cols.saturating_sub(right_visible) + 1;
        let _ = write!(err, "\x1b[{};{}H{}", h, right_start_col, right);
    }
    let _ = write!(err, "\x1b8");
    let _ = err.flush();
}

/// Restore the scrolling region to the full screen and move cursor below
/// the (now-released) reserved row so a subsequent shell prompt lands cleanly.
pub fn leave() {
    if !ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }
    let h = *HEIGHT.lock().unwrap();
    let mut err = std::io::stderr();
    // Reset scroll region to full screen.
    let _ = write!(err, "\x1b[r");
    if h > 0 {
        // Move cursor to the bar's row and clear it, then drop a newline.
        let _ = writeln!(err, "\x1b[{};1H\x1b[2K", h);
    }
    let _ = err.flush();
}

/// Install Drop-style safety: panic hook + Ctrl-C handler that resets
/// the scroll region so the user's terminal isn't left in a weird state.
fn install_safety_guard() {
    if GUARD_INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    // Panic hook: chain after the existing one, but reset region first.
    let prior = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        leave();
        prior(info);
    }));
    // SIGINT/SIGTERM via a tokio task; falls back to no-op if reactor isn't up.
    if let Ok(mut sigint) =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
    {
        tokio::spawn(async move {
            sigint.recv().await;
            leave();
            // Re-raise default behavior by exiting cleanly.
            std::process::exit(130);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_obeys_env_opt_out() {
        std::env::set_var("MLX_CODE_NO_STICKY", "1");
        let s = supported();
        std::env::remove_var("MLX_CODE_NO_STICKY");
        // Whether it's true otherwise depends on stderr-is-tty in the test
        // runner; just assert env opt-out forces false.
        assert!(!s, "MLX_CODE_NO_STICKY=1 should suppress sticky mode");
    }
}
