//! `list` tool: structured directory listing.

use anyhow::{anyhow, Context, Result};
use futures_util::future::FutureExt;
use serde_json::{json, Value};
use std::path::PathBuf;

use super::Tool;

pub fn tool() -> Tool {
    Tool {
        name: "list",
        schema: json!({
            "type": "function",
            "function": {
                "name": "list",
                "description": "List directory entries (one per line, name + type + size).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "directory path; default cwd" }
                    }
                }
            }
        }),
        exec: |args| async move { run(args).await }.boxed(),
    }
}

async fn run(args: Value) -> Result<String> {
    let path = args.get("path").and_then(|v| v.as_str()).map(|s| PathBuf::from(shellexpand::tilde(s).into_owned()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let read = std::fs::read_dir(&path).with_context(|| format!("readdir {}", path.display()))?;
    let mut entries: Vec<(String, String, u64)> = Vec::new();
    for e in read {
        let Ok(e) = e else { continue };
        let name = e.file_name().to_string_lossy().to_string();
        let ft = e.file_type().ok();
        let kind = if ft.as_ref().map(|f| f.is_dir()).unwrap_or(false) { "dir" }
                   else if ft.as_ref().map(|f| f.is_symlink()).unwrap_or(false) { "link" }
                   else { "file" };
        let size = e.metadata().map(|m| m.len()).unwrap_or(0);
        entries.push((kind.to_string(), name, size));
    }
    if entries.is_empty() {
        return Err(anyhow!("(empty directory: {})", path.display()));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let mut out = format!("{}\n", path.display());
    for (kind, name, size) in entries {
        out.push_str(&format!("  {:<4} {:>10} {}\n", kind, size, name));
    }
    Ok(out)
}
