//! `multi_edit` tool: apply a sequence of edits to the same file atomically.
//!
//! Each edit's `old_string` is matched against the result of the previous
//! edits. If ANY edit in the sequence fails, NONE land - the file is
//! untouched. Uses the same whitespace-tolerant fallback as the single-shot
//! `edit` tool via `apply_edit_in_memory`.

use anyhow::{anyhow, Context, Result};
use futures_util::future::FutureExt;
use serde_json::{json, Value};
use std::path::PathBuf;

use super::edit::apply_edit_in_memory;
use super::Tool;

pub fn tool() -> Tool {
    Tool {
        name: "multi_edit",
        schema: json!({
            "type": "function",
            "function": {
                "name": "multi_edit",
                "description": "Apply a sequence of edits to one file atomically. Each edit is applied to the result of the previous one. If any edit fails, NONE land. Use this when you need 2+ replacements in the same file in one round-trip.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "edits": {
                            "type": "array",
                            "description": "Sequence of edits applied in order to the same file.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "old_string": { "type": "string", "description": "exact text to replace; empty for full overwrite (only valid as first edit on a missing/empty file)" },
                                    "new_string": { "type": "string" },
                                    "replace_all": { "type": "boolean", "default": false }
                                },
                                "required": ["old_string", "new_string"]
                            }
                        },
                        "dry_run": { "type": "boolean", "description": "validate + return summary only; no write. default false" }
                    },
                    "required": ["path", "edits"]
                }
            }
        }),
        exec: |args| async move { run(args).await }.boxed(),
    }
}

async fn run(args: Value) -> Result<String> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("multi_edit: missing path"))?
        .to_string();
    let edits = args
        .get("edits")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("multi_edit: missing or non-array edits"))?
        .clone();
    if edits.is_empty() {
        return Err(anyhow!("multi_edit: edits array is empty"));
    }
    let dry_run = args
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || std::env::var("MLX_CODE_DRY_RUN")
            .map(|v| v == "1")
            .unwrap_or(false);

    let p = PathBuf::from(shellexpand::tilde(&path).into_owned());

    // Read-staleness gate: same as single edit.
    if let Some(stamp) = crate::read_cache::last_read(&p) {
        if stamp.seen_mtime != 0 {
            if let Ok(meta) = std::fs::metadata(&p) {
                let cur_mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                if cur_mtime > stamp.seen_mtime {
                    return Err(anyhow!(
                        "multi_edit: {} changed since last read at {} (now mtime {}); read it again before editing",
                        p.display(),
                        stamp.read_at,
                        cur_mtime
                    ));
                }
            }
        }
    }

    // Load current contents (or treat as empty if first edit's old_string is "" and file missing).
    let original = match std::fs::read_to_string(&p) {
        Ok(s) => s,
        Err(_) => {
            // Missing-file path is only legal if first edit is a create (old_string == "").
            let first = edits
                .first()
                .and_then(|e| e.get("old_string"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !first.is_empty() {
                return Err(anyhow!(
                    "multi_edit: cannot read {} and first edit's old_string is non-empty",
                    p.display()
                ));
            }
            String::new()
        }
    };

    // Apply edits in sequence to an in-memory buffer. If ANY fails, abort.
    let mut buf = original.clone();
    let mut total_replacements: usize = 0;
    for (i, e) in edits.iter().enumerate() {
        let old = e.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
        let new = e.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
        let replace_all = e
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let (next, n) = apply_edit_in_memory(&buf, old, new, replace_all)
            .map_err(|err| anyhow!("multi_edit: edit[{}] failed: {}", i, err))?;
        buf = next;
        total_replacements += n;
    }

    if buf == original {
        return Err(anyhow!(
            "multi_edit: no-op - applied edits produced identical content"
        ));
    }

    if dry_run {
        crate::dry_run_log::record_with_bytes("replace", p.display().to_string(), buf.len() as u64);
        return Ok(format!(
            "(dry_run) would multi_edit {} ({} edit{}, {} total replacement{}, final size {} bytes)\n",
            p.display(),
            edits.len(),
            if edits.len() == 1 { "" } else { "s" },
            total_replacements,
            if total_replacements == 1 { "" } else { "s" },
            buf.len()
        ));
    }

    // Atomic write happens later in feature 8; for now use direct write
    // to keep this commit surgical.
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("mkdir -p {}", parent.display()))?;
        }
    }
    std::fs::write(&p, &buf).with_context(|| format!("write {}", p.display()))?;
    crate::read_cache::invalidate(&p);

    Ok(format!(
        "multi_edited {} ({} edit{}, {} total replacement{})\n",
        p.display(),
        edits.len(),
        if edits.len() == 1 { "" } else { "s" },
        total_replacements,
        if total_replacements == 1 { "" } else { "s" }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt_run(args: serde_json::Value) -> Result<String> {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(run(args))
    }

    #[test]
    fn multi_edit_applies_sequence_atomically() {
        let _guard = crate::dry_run_log::ENV_LOCK.lock().unwrap();
        std::env::remove_var("MLX_CODE_DRY_RUN");
        crate::read_cache::clear_reads();
        let p = std::env::temp_dir().join(format!("mlx-multi-{}.txt", std::process::id()));
        std::fs::write(&p, "alpha\nbeta\ngamma\n").unwrap();

        let out = rt_run(json!({
            "path": p.to_string_lossy(),
            "edits": [
                { "old_string": "alpha", "new_string": "ALPHA" },
                { "old_string": "beta",  "new_string": "BETA" },
                { "old_string": "gamma", "new_string": "GAMMA" }
            ]
        }))
        .unwrap();
        assert!(out.contains("multi_edited"), "expected success: {}", out);
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(body, "ALPHA\nBETA\nGAMMA\n");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn multi_edit_aborts_on_failure_leaving_file_untouched() {
        let _guard = crate::dry_run_log::ENV_LOCK.lock().unwrap();
        std::env::remove_var("MLX_CODE_DRY_RUN");
        crate::read_cache::clear_reads();
        let p = std::env::temp_dir().join(format!("mlx-multi-fail-{}.txt", std::process::id()));
        let original = "alpha\nbeta\ngamma\n";
        std::fs::write(&p, original).unwrap();

        // 2nd edit refers to text that doesn't exist; whole call must fail
        // and leave file untouched.
        let res = rt_run(json!({
            "path": p.to_string_lossy(),
            "edits": [
                { "old_string": "alpha", "new_string": "ALPHA" },
                { "old_string": "DOES_NOT_EXIST", "new_string": "X" }
            ]
        }));
        assert!(res.is_err(), "expected failure, got: {:?}", res);
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(body, original, "file should be untouched on failure");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn multi_edit_second_edit_sees_first_edit_result() {
        let _guard = crate::dry_run_log::ENV_LOCK.lock().unwrap();
        std::env::remove_var("MLX_CODE_DRY_RUN");
        crate::read_cache::clear_reads();
        let p = std::env::temp_dir().join(format!("mlx-multi-chain-{}.txt", std::process::id()));
        std::fs::write(&p, "old_name\n").unwrap();

        // First edit renames old_name -> mid_name, second renames mid_name -> new_name.
        // Only legal if the second edit operates on the result of the first.
        let out = rt_run(json!({
            "path": p.to_string_lossy(),
            "edits": [
                { "old_string": "old_name", "new_string": "mid_name" },
                { "old_string": "mid_name", "new_string": "new_name" }
            ]
        }))
        .unwrap();
        assert!(out.contains("multi_edited"), "expected success: {}", out);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new_name\n");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn multi_edit_dry_run_does_not_write() {
        let _guard = crate::dry_run_log::ENV_LOCK.lock().unwrap();
        std::env::remove_var("MLX_CODE_DRY_RUN");
        crate::read_cache::clear_reads();
        let p = std::env::temp_dir().join(format!("mlx-multi-dry-{}.txt", std::process::id()));
        let body = "alpha\n";
        std::fs::write(&p, body).unwrap();

        let out = rt_run(json!({
            "path": p.to_string_lossy(),
            "edits": [{ "old_string": "alpha", "new_string": "beta" }],
            "dry_run": true
        }))
        .unwrap();
        assert!(
            out.starts_with("(dry_run)"),
            "expected dry_run prefix: {}",
            out
        );
        assert_eq!(std::fs::read_to_string(&p).unwrap(), body);
        let _ = std::fs::remove_file(&p);
    }
}
