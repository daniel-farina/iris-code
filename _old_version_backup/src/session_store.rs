//! Persist per-session conversation history to disk so `hip --resume` can
//! actually reload the prior conversation into the in-memory `conv` array
//! instead of only rotating the MTPLX session id (which warms the prefix
//! cache but leaves the model with no view of past turns).
//!
//! Storage: `~/.mlx-code/sessions/<session_id>.json` — a single JSON
//! document containing the full ChatMessage array. Sanitized session ids
//! (no path separators) are used as the file stem.
//!
//! Failures are best-effort: if save fails we log and keep going; if load
//! returns None the caller falls back to a fresh conversation.

use crate::schema::ChatMessage;
use std::path::PathBuf;

fn dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let p = PathBuf::from(home).join(".mlx-code").join("sessions");
    let _ = std::fs::create_dir_all(&p);
    Some(p)
}

/// Strip anything that could escape the sessions directory or be ambiguous
/// on disk (path separators, dots beyond the file extension, control chars).
/// We keep alphanumerics, dashes, underscores; replace everything else with
/// `_` so two distinct ids always map to two distinct files.
fn sanitize(sid: &str) -> String {
    sid.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn path_for(sid: &str) -> Option<PathBuf> {
    let mut p = dir()?;
    p.push(format!("{}.json", sanitize(sid)));
    Some(p)
}

/// Persist `conv` for a given session. Best-effort: any error is swallowed
/// so a flaky disk doesn't break the chat loop. Caller decides when to call
/// (typically after each successful model turn).
pub fn save(sid: &str, conv: &[ChatMessage]) {
    let Some(path) = path_for(sid) else { return };
    let Ok(serialized) = serde_json::to_vec_pretty(conv) else {
        return;
    };
    // Atomic-ish write: write to <path>.tmp then rename. Avoids leaving a
    // half-written file if the process is killed mid-flush.
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, &serialized).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Load the persisted conversation for `sid`. Returns None if the file is
/// absent or unreadable; the caller should then start with a fresh
/// system-prompt-only conv.
pub fn load(sid: &str) -> Option<Vec<ChatMessage>> {
    let path = path_for(sid)?;
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice::<Vec<ChatMessage>>(&bytes).ok()
}

/// Estimate the prior context size in tokens for a loaded session. Uses the
/// classic chars/4 heuristic — close enough for the running indicator to be
/// meaningful before the next API call's real `usage.prompt_tokens` arrives.
pub fn estimate_tokens(conv: &[ChatMessage]) -> u32 {
    let total_chars: usize = conv
        .iter()
        .map(|m| m.content.as_deref().map(|s| s.len()).unwrap_or(0))
        .sum();
    (total_chars / 4) as u32
}
