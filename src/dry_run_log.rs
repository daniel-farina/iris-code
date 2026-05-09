//! Process-global accumulator for paths that `edit`/`bash` *would* have
//! mutated during a `--dry-run` agent loop. Read at the end of the run by
//! `print_dry_run_summary` to give the user a "this is what would change"
//! report, which is the actionable companion to the inline `(dry_run)`
//! preview text.

use once_cell::sync::Lazy;
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct WouldChange {
    pub kind: &'static str, // "create" | "overwrite" | "replace" | "bash"
    pub target: String,
    /// Bytes that would be written (for create/overwrite/replace) or 0 for bash.
    pub bytes: u64,
}

static LOG: Lazy<Mutex<Vec<WouldChange>>> = Lazy::new(|| Mutex::new(Vec::new()));

pub fn record(kind: &'static str, target: impl Into<String>) {
    record_with_bytes(kind, target, 0)
}

pub fn record_with_bytes(kind: &'static str, target: impl Into<String>, bytes: u64) {
    if let Ok(mut g) = LOG.lock() {
        g.push(WouldChange { kind, target: target.into(), bytes });
    }
}

pub fn drain() -> Vec<WouldChange> {
    match LOG.lock() {
        Ok(mut g) => std::mem::take(&mut *g),
        Err(_) => Vec::new(),
    }
}

#[allow(dead_code)]
pub fn is_active() -> bool {
    std::env::var("MLX_CODE_DRY_RUN").map(|v| v == "1").unwrap_or(false)
}

/// Test-only serialization guard for the `MLX_CODE_DRY_RUN` env var. Tests
/// that mutate this env var must hold this lock for their duration so they
/// don't race against each other when cargo runs the suite in parallel.
#[cfg(test)]
pub static ENV_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_then_drain_returns_entries_in_order() {
        // Drain anything left over from earlier tests in the same process.
        let _ = drain();
        record("create", "/tmp/a.txt");
        record("bash", "echo hi");
        record_with_bytes("replace", "/tmp/b.rs", 512);
        let out = drain();
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].kind, "create");
        assert_eq!(out[0].target, "/tmp/a.txt");
        assert_eq!(out[0].bytes, 0);
        assert_eq!(out[1].kind, "bash");
        assert_eq!(out[2].kind, "replace");
        assert_eq!(out[2].bytes, 512);
        let again = drain();
        assert!(again.is_empty());
    }
}
