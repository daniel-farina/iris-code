//! Pretty-printing for the streamed agent output: think blocks, response
//! content, tool calls, tool results. Renders to stderr (so saved file output
//! on stdout stays clean) using ANSI color and lightweight box drawing.

use std::io::{IsTerminal, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Idle,
    Thinking,
    Response,
}

pub struct PrettyOut {
    pub enabled: bool,
    section: Section,
    /// Buffer to accumulate content while we look for `<think>`/`</think>`
    /// markers that may straddle SSE chunk boundaries.
    pending: String,
    line_in_section: bool,
    /// When false (default), `<think>...</think>` blocks are silently dropped
    /// from output. When true, they render in their own dim section.
    show_thinking: bool,
}

const THINK_OPEN: &str = "<think>";
const THINK_CLOSE: &str = "</think>";

impl PrettyOut {
    pub fn new() -> Self {
        let enabled = std::env::var("MLX_CODE_NO_PRETTY").ok().as_deref() != Some("1")
            && std::io::IsTerminal::is_terminal(&std::io::stdout());
        let show_thinking = std::env::var("MLX_CODE_SHOW_THINKING")
            .ok()
            .as_deref()
            .map(|v| matches!(v, "1" | "true" | "yes" | "on"))
            .unwrap_or(false);
        Self {
            enabled,
            section: Section::Idle,
            pending: String::new(),
            line_in_section: false,
            show_thinking,
        }
    }

    /// Process a streamed text delta. Splits on `<think>`/`</think>` and
    /// writes each segment in its appropriate section style. Output goes to
    /// stdout (so the user can pipe content elsewhere); section borders go to
    /// stderr.
    pub fn push_content(&mut self, delta: &str) {
        if !self.enabled {
            // Plain mode: write raw to stdout.
            let _ = std::io::stdout().write_all(delta.as_bytes());
            let _ = std::io::stdout().flush();
            return;
        }
        self.pending.push_str(delta);

        // Drain segments separated by think markers.
        loop {
            // Look for the next marker (whichever comes first).
            let open_pos = self.pending.find(THINK_OPEN);
            let close_pos = self.pending.find(THINK_CLOSE);
            let next = match (open_pos, close_pos) {
                (Some(o), Some(c)) if o < c => Some((o, true)),
                (Some(_), Some(c)) => Some((c, false)),
                (Some(o), None) => Some((o, true)),
                (None, Some(c)) => Some((c, false)),
                (None, None) => None,
            };
            let Some((pos, is_open)) = next else { break };
            // Emit pre-marker text in current section.
            let pre: String = self.pending.drain(..pos).collect();
            if !pre.is_empty() {
                self.emit_in_section(&pre);
            }
            // Consume the marker itself.
            let marker_len = if is_open {
                THINK_OPEN.len()
            } else {
                THINK_CLOSE.len()
            };
            self.pending.drain(..marker_len);
            // Switch section.
            if is_open {
                self.enter(Section::Thinking);
            } else {
                self.enter(Section::Response);
            }
        }

        // Whatever's left in `pending` might still be a partial marker. Emit
        // anything we know is safe (i.e. up to but not including a possible
        // marker prefix at the tail).
        let safe_len = safe_emit_len(&self.pending);
        if safe_len > 0 {
            let safe: String = self.pending.drain(..safe_len).collect();
            self.emit_in_section(&safe);
        }
    }

    /// Force flush any buffered partial-marker tail and end the current
    /// section. Call when the stream is fully done.
    pub fn flush_end(&mut self) {
        if !self.enabled {
            return;
        }
        // Treat any leftover bytes as content (no marker arrived).
        if !self.pending.is_empty() {
            let drained = std::mem::take(&mut self.pending);
            self.emit_in_section(&drained);
        }
        self.close_section();
    }

    fn enter(&mut self, new_section: Section) {
        if self.section == new_section {
            return;
        }
        self.close_section();
        self.section = new_section;
        self.line_in_section = false;
        match new_section {
            Section::Thinking => {
                if self.show_thinking {
                    header(stderr_handle(), "🤔 thinking", "\x1b[2;3m");
                }
            }
            Section::Response => header(stderr_handle(), "💬 response", "\x1b[0;37m"),
            Section::Idle => {}
        }
    }

    fn close_section(&mut self) {
        if matches!(self.section, Section::Idle) {
            return;
        }
        // Suppress closing border for hidden thinking sections.
        let suppress = matches!(self.section, Section::Thinking) && !self.show_thinking;
        let mut so = std::io::stdout();
        let _ = so.write_all(b"\x1b[0m");
        let _ = so.flush();
        if !suppress {
            footer(stderr_handle());
        }
        self.section = Section::Idle;
    }

    fn emit_in_section(&mut self, text: &str) {
        // If thinking is hidden, drop content while in the thinking section.
        if matches!(self.section, Section::Thinking) && !self.show_thinking {
            return;
        }
        // Default to Response if we're somehow Idle and content arrives.
        if matches!(self.section, Section::Idle) {
            self.enter(Section::Response);
        }
        let mut so = std::io::stdout();
        // Apply ANSI style at start of each line so the section style sticks
        // even if a previous line ended with a reset.
        let style = match self.section {
            Section::Thinking => "\x1b[2;3m",
            Section::Response => "\x1b[0m",
            Section::Idle => "",
        };
        // Write style once at the start, then raw bytes.
        if !self.line_in_section {
            let _ = so.write_all(style.as_bytes());
            self.line_in_section = true;
        }
        let _ = so.write_all(text.as_bytes());
        let _ = so.flush();
    }
}

/// Returns the number of bytes from the start of `s` that we can safely emit
/// without splitting a partial `<think>` or `</think>` marker. The tail bytes
/// that COULD be the start of a marker are held back.
///
/// Only ASCII byte indices are valid markers (`<think>` is all ASCII), so we
/// can use byte indexing — but we must only check positions that are also
/// UTF-8 char boundaries to avoid panicking on multibyte chars in the text
/// (e.g. em-dash `—` is 3 bytes).
fn safe_emit_len(s: &str) -> usize {
    let max = s.len();
    if max == 0 {
        return 0;
    }
    let look = max.saturating_sub(8);
    for i in look..max {
        if !s.is_char_boundary(i) {
            continue;
        }
        let tail = &s[i..];
        for marker in &[THINK_OPEN, THINK_CLOSE] {
            if marker.starts_with(tail) {
                return i;
            }
        }
    }
    max
}

/// Emit a one-line "the model hit max_tokens; auto-continuing" notice.
/// Called by the agent loop when it sees finish_reason="length" without
/// tool calls so the user understands the response is being resumed
/// implicitly instead of looking like a stuck conversation.
pub fn truncation_notice(cap: u32) {
    eprintln!(
        "\x1b[2m─ response hit max_tokens cap ({}); auto-continuing… raise with --max-tokens N if this happens often ─\x1b[0m",
        cap
    );
}

pub fn tool_call_header(name: &str) {
    if !is_pretty() {
        return;
    }
    eprintln!();
    eprintln!(
        "\x1b[2m╭─ \x1b[0;36m🔧 tool: \x1b[1;36m{}\x1b[0;2m ─────────────────────\x1b[0m",
        name
    );
}

pub fn tool_call_args(args_pretty: &str) {
    if !is_pretty() {
        eprintln!("{}", args_pretty);
        return;
    }
    for line in args_pretty.lines() {
        eprintln!("\x1b[2m│\x1b[0m {}", line);
    }
    eprintln!("\x1b[2m╰─────────────────────────────────────\x1b[0m");
}

pub fn tool_result(name: &str, ok: bool, body: &str, default_max_lines: usize) {
    if !is_pretty() {
        eprintln!("[{}] {}", if ok { "ok" } else { "err" }, body);
        return;
    }
    let full = std::env::var("MLX_CODE_FULL_OUTPUT")
        .ok()
        .as_deref()
        .map(|v| matches!(v, "1" | "true" | "on"))
        .unwrap_or(false);
    let max_lines = if full { usize::MAX } else { default_max_lines };
    let icon = if ok { "✓" } else { "✗" };
    let icon_color = if ok { "\x1b[0;32m" } else { "\x1b[0;31m" };
    eprintln!(
        "\x1b[2m╭─ {}{}\x1b[0;2m result: \x1b[1m{}\x1b[0;2m ─────────────────────\x1b[0m",
        icon_color, icon, name
    );
    let total = body.lines().count();
    for (shown, line) in body.lines().enumerate() {
        if shown == max_lines && total > max_lines + 2 {
            eprintln!(
                "\x1b[2m│ ... ({} more lines, truncated; --full-output to see all)\x1b[0m",
                total - shown
            );
            break;
        }
        eprintln!("\x1b[2m│\x1b[0m {}", line);
    }
    eprintln!("\x1b[2m╰─────────────────────────────────────\x1b[0m");
}

fn header<W: Write>(mut w: W, title: &str, _content_style: &str) {
    let _ = writeln!(w);
    let _ = writeln!(w, "\x1b[2m╭─ {}\x1b[2m ─────────────────────\x1b[0m", title);
    let _ = w.flush();
}

fn footer<W: Write>(mut w: W) {
    let _ = writeln!(w);
    let _ = writeln!(w, "\x1b[2m╰─────────────────────────────────────\x1b[0m");
    let _ = w.flush();
}

fn stderr_handle() -> std::io::Stderr {
    std::io::stderr()
}

fn is_pretty() -> bool {
    std::env::var("MLX_CODE_NO_PRETTY").ok().as_deref() != Some("1")
        && std::io::stderr().is_terminal()
}

/// Pretty-print a tool call's JSON arguments as key:value lines. Multi-line
/// string values (like `new_string`) get indented continuation lines.
pub fn pretty_args(args_json: &str) -> String {
    let v: serde_json::Value = match serde_json::from_str(args_json) {
        Ok(v) => v,
        Err(_) => return args_json.to_string(),
    };
    let obj = match v.as_object() {
        Some(o) => o,
        None => return args_json.to_string(),
    };
    let mut out = String::new();
    let key_w = obj.keys().map(|k| k.len()).max().unwrap_or(0);
    for (k, val) in obj {
        let display = match val {
            serde_json::Value::String(s) => {
                if s.contains('\n') {
                    let mut lines = s.lines();
                    let mut acc = String::new();
                    if let Some(first) = lines.next() {
                        acc.push_str(first);
                    }
                    for line in lines {
                        acc.push('\n');
                        for _ in 0..(key_w + 2) {
                            acc.push(' ');
                        }
                        acc.push_str(line);
                    }
                    acc
                } else {
                    s.clone()
                }
            }
            other => other.to_string(),
        };
        out.push_str(&format!(
            "{:<width$}  {}\n",
            format!("\x1b[0;33m{k}\x1b[0m:"),
            display,
            width = key_w + "\x1b[0;33m\x1b[0m:".len()
        ));
    }
    out.trim_end().to_string()
}
