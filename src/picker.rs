//! Tiny arrow-key picker for terminal menus.
//!
//! Renders a numbered list, lets the user move the cursor with the up/down
//! arrows (or `j`/`k`, or a digit to jump), and returns the chosen index
//! on Enter. Falls back to the supplied default when stdin isn't a TTY
//! (CI, pipes), so callers never have to special-case that themselves.
//!
//! Why this exists: line-buffered `read_line` prompts (Y/n / u/f) broke
//! on terminals where Enter sends raw CR instead of NL -- the read just
//! waits forever. Raw-mode reads of single keystrokes sidestep that
//! whole class of bug because we never wait for a newline.

use std::io::{IsTerminal, Read, Write};

/// One row in the picker. `subtitle` is rendered dim under the main label.
pub struct Option_<'a> {
    pub label: &'a str,
    pub subtitle: Option<&'a str>,
}

/// Display `options` with arrow-key navigation. Returns the selected index.
/// When stdin isn't a TTY (or any setup step fails), returns `default_idx`
/// after echoing the default to stderr so script invocations stay
/// deterministic.
pub fn select_one(prompt: &str, options: &[Option_<'_>], default_idx: usize) -> usize {
    use crate::theme::{accent, dim, good, RESET};
    let d = dim();
    let a = accent();
    let g = good();
    let r = RESET;

    if options.is_empty() {
        return 0;
    }
    let default_idx = default_idx.min(options.len() - 1);

    // Always print the prompt header so the user knows what's being asked,
    // even in non-TTY mode where we won't render the interactive list.
    eprintln!();
    eprintln!("  {a}?{r} {}", prompt);

    if !std::io::stdin().is_terminal() {
        // Non-interactive: list the choices once and announce the default.
        for (i, opt) in options.iter().enumerate() {
            let mark = if i == default_idx {
                format!("{g}>{r}")
            } else {
                " ".to_string()
            };
            eprintln!("    {} [{}] {}", mark, i + 1, opt.label);
            if let Some(sub) = opt.subtitle {
                eprintln!("        {d}{}{r}", sub);
            }
        }
        eprintln!("  {d}(non-tty: picking default [{}]){r}", default_idx + 1);
        return default_idx;
    }

    // Stash and switch to raw mode. The guard restores termios on drop so
    // a Ctrl+C or panic can't leave the user's terminal mangled.
    let _guard = match RawMode::enter() {
        Some(g) => g,
        None => {
            eprintln!(
                "  {d}(could not enter raw mode; picking default [{}]){r}",
                default_idx + 1
            );
            return default_idx;
        }
    };

    let mut current = default_idx;
    let mut first_render = true;

    loop {
        // After the first render, move the cursor back up to the top of
        // the list so we can repaint in-place.
        if !first_render {
            // Lines printed: option_count (rendered_height_per_option)
            let height: usize = options
                .iter()
                .map(|o| 1 + if o.subtitle.is_some() { 1 } else { 0 })
                .sum();
            eprint!("\x1b[{}A", height);
        }
        first_render = false;

        for (i, opt) in options.iter().enumerate() {
            let cursor = if i == current {
                format!("{a}▶{r}")
            } else {
                " ".to_string()
            };
            let num = if i == current {
                format!("{a}[{}]{r}", i + 1)
            } else {
                format!("{d}[{}]{r}", i + 1)
            };
            let label = if i == current {
                format!("{a}{}{r}", opt.label)
            } else {
                opt.label.to_string()
            };
            // Clear-to-end-of-line so a shorter row doesn't leave trailing
            // junk from a previous frame.
            eprint!("\r\x1b[2K    {} {} {}\n", cursor, num, label);
            if let Some(sub) = opt.subtitle {
                eprint!("\r\x1b[2K        {d}{}{r}\n", sub);
            }
        }
        let _ = std::io::stderr().flush();

        let mut buf = [0u8; 8];
        let n = std::io::stdin().lock().read(&mut buf).unwrap_or(0);
        if n == 0 {
            return current;
        }
        match &buf[..n] {
            // Enter (LF or CR)
            [b'\n'] | [b'\r'] => return current,
            // Ctrl+C / q / Esc on its own = cancel, keep current
            [3] | [b'q'] | [b'Q'] | [0x1b] => return current,
            // Arrow up: ESC [ A    -- also accept vim-style 'k'
            [0x1b, b'[', b'A'] | [b'k'] if current > 0 => {
                current -= 1;
            }
            // Arrow down: ESC [ B   -- also accept 'j'
            [0x1b, b'[', b'B'] | [b'j'] if current + 1 < options.len() => {
                current += 1;
            }
            // Digit jump (1-9)
            [c] if c.is_ascii_digit() && *c != b'0' => {
                let idx = (*c - b'1') as usize;
                if idx < options.len() {
                    current = idx;
                }
            }
            _ => {}
        }
    }
}

/// Raw-mode guard: sets ICANON+ECHO off on construction, restores the
/// previous termios on drop. Lifetime-scoped so a panic or early return
/// can't strand the user's TTY in raw mode.
struct RawMode {
    fd: i32,
    saved: libc::termios,
}

impl RawMode {
    fn enter() -> Option<Self> {
        let fd = libc::STDIN_FILENO;
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::tcgetattr(fd, &mut saved) };
        if rc != 0 {
            return None;
        }
        let mut raw = saved;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO);
        // VMIN=1 / VTIME=0: each read returns as soon as a byte arrives,
        // no inter-character timer.
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        let rc = unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) };
        if rc != 0 {
            return None;
        }
        Some(RawMode { fd, saved })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.saved);
        }
    }
}
