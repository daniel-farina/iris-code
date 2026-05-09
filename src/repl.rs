//! rustyline `Helper` for the chat REPL: tab-completes `:commands` and
//! relative paths after spaces, plus glues in syntax-highlight and validation
//! traits with sensible no-op defaults.

use rustyline::completion::{Completer, FilenameCompleter, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::{ValidationContext, ValidationResult, Validator};
use rustyline::{Context, Helper};

/// Built-in REPL commands. Update this list when adding a new `:command`.
const COMMANDS: &[&str] = &[
    ":help",
    ":reset",
    ":quit",
    ":exit",
    ":q",
    ":stats",
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
}

impl Highlighter for MlxHelper {}

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
        // If the cursor is on the first token AND it begins with `:`,
        // complete from the COMMANDS list.
        let prefix = &line[..pos];
        let on_first_token = !prefix.contains(' ');
        if on_first_token && prefix.starts_with(':') {
            let matches: Vec<Pair> = COMMANDS
                .iter()
                .filter(|c| c.starts_with(prefix))
                .map(|c| Pair {
                    display: c.to_string(),
                    replacement: c.to_string(),
                })
                .collect();
            return Ok((0, matches));
        }
        // Otherwise delegate to the filename completer for paths after a space.
        self.files.complete(line, pos, ctx)
    }
}
