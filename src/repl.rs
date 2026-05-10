//! rustyline `Helper` for the chat REPL: tab-completes `:`/`/`-prefixed
//! commands and relative paths after spaces, plus glues in syntax-highlight
//! and validation traits with sensible no-op defaults.
//!
//! Slash-prefix (`/`) commands and colon-prefix (`:`) commands are kept in
//! lockstep — both work everywhere. The `/` flavor matches what users coming
//! from other agent CLIs (claude, openai, etc.) expect; `:` is the original.

use rustyline::completion::{Completer, FilenameCompleter, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::{ValidationContext, ValidationResult, Validator};
use rustyline::{Context, Helper};
use std::borrow::Cow;

/// All built-in REPL commands. Kept in one list so completion + help stay in
/// sync. Each entry is the colon form; the `/` form is generated on the fly
/// during completion so users see both prefixes work without us doubling the
/// list. When adding a new command, add it here AND wire it into the match
/// block in `main.rs::run_chat`.
pub const COMMANDS_COLON: &[&str] = &[
    ":help",
    ":new",
    ":reset",
    ":quit",
    ":exit",
    ":q",
    ":stats",
    ":context",
    ":queue",
    ":history",
    ":cwd",
    ":show-thinking",
    ":full-output",
    ":smoke",
    ":tools",
    ":overhead",
    ":diff",
    ":peek",
    ":dry-run",
    ":cache",
    ":tps",
    ":theme",
];

pub struct MlxHelper {
    files: FilenameCompleter,
}

impl MlxHelper {
    pub fn new() -> Self {
        Self {
            files: FilenameCompleter::new(),
        }
    }
}

impl Helper for MlxHelper {}

impl Hinter for MlxHelper {
    type Hint = String;

    /// Show a faint inline hint. Two cases handled:
    ///   1. The buffer starts with `:` or `/` and matches a command prefix
    ///      uniquely → show the rest of the command name as a hint, so the
    ///      user can see what they're about to invoke without tabbing.
    ///   2. The buffer is empty → show a one-line tip listing the most
    ///      useful commands and shortcuts.
    fn hint(&self, line: &str, pos: usize, _ctx: &Context<'_>) -> Option<String> {
        // Empty buffer: surface a discovery hint for new users.
        if line.is_empty() && pos == 0 {
            return Some(
                " (try /help · /new starts fresh context · Alt-Enter for newline · Ctrl-C twice exits)"
                    .to_string(),
            );
        }
        // Cursor must be at end-of-line for command-suffix hints to make sense.
        if pos != line.len() {
            return None;
        }
        let prefix = line.trim_start();
        if prefix.is_empty() {
            return None;
        }
        let head = prefix.chars().next()?;
        if head != ':' && head != '/' {
            return None;
        }
        // Translate `/foo` → `:foo` for matching against the canonical list.
        let canon = if head == '/' {
            format!(":{}", &prefix[1..])
        } else {
            prefix.to_string()
        };
        // Find a unique match.
        let mut matches = COMMANDS_COLON.iter().filter(|c| c.starts_with(&canon));
        let first = matches.next()?;
        if matches.next().is_some() {
            // Ambiguous — let tab-completion handle it.
            return None;
        }
        // Return the missing suffix only, so rustyline appends it past the cursor.
        Some(first[canon.len()..].to_string())
    }
}

impl Highlighter for MlxHelper {
    /// Render hints in dim gray so they're clearly distinguishable from typed
    /// input. Without this hints render in the terminal's default color and
    /// look indistinguishable from real text.
    fn highlight_hint<'h>(&self, hint: &'h str) -> Cow<'h, str> {
        Cow::Owned(format!("\x1b[2m{}\x1b[0m", hint))
    }
}

impl Validator for MlxHelper {
    fn validate(&self, _ctx: &mut ValidationContext) -> Result<ValidationResult, ReadlineError> {
        Ok(ValidationResult::Valid(None))
    }
}

impl Completer for MlxHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        ctx: &Context<'_>,
    ) -> Result<(usize, Vec<Pair>), ReadlineError> {
        let prefix = &line[..pos];
        let on_first_token = !prefix.contains(' ');
        if on_first_token {
            if let Some(head) = prefix.chars().next() {
                if head == ':' || head == '/' {
                    // Translate prefix to colon-form for matching, but echo
                    // back replacements in whichever prefix the user typed
                    // so completion doesn't surprise them.
                    let canon = if head == '/' {
                        format!(":{}", &prefix[1..])
                    } else {
                        prefix.to_string()
                    };
                    let matches: Vec<Pair> = COMMANDS_COLON
                        .iter()
                        .filter(|c| c.starts_with(&canon))
                        .map(|c| {
                            let display = if head == '/' {
                                format!("/{}", &c[1..])
                            } else {
                                c.to_string()
                            };
                            let replacement = display.clone();
                            Pair {
                                display,
                                replacement,
                            }
                        })
                        .collect();
                    return Ok((0, matches));
                }
            }
        }
        // Otherwise delegate to the filename completer for paths after a space.
        self.files.complete(line, pos, ctx)
    }
}
