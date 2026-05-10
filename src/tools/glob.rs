//! `glob` tool: glob matching, returns matching paths (no contents).

use anyhow::{anyhow, Result};
use futures_util::future::FutureExt;
use ignore::WalkBuilder;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use super::Tool;

const MAX_RESULTS: usize = 200;

pub fn tool() -> Tool {
    Tool {
        name: "glob",
        schema: json!({
            "type": "function",
            "function": {
                "name": "glob",
                "description": "Find files matching a glob pattern (e.g. **/*.rs). Honors .gitignore. Returns up to 200 paths.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string" },
                        "path": { "type": "string", "description": "root dir; default cwd" }
                    },
                    "required": ["pattern"]
                }
            }
        }),
        exec: |args| async move { run(args).await }.boxed(),
    }
}

async fn run(args: Value) -> Result<String> {
    let pattern = args
        .get("pattern")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("glob: missing pattern"))?
        .to_string();
    let root = args
        .get("path")
        .and_then(|v| v.as_str())
        .map(|s| PathBuf::from(shellexpand::tilde(s).into_owned()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let g = globset::Glob::new(&pattern)
        .map_err(|e| anyhow!("invalid glob: {}", e))?
        .compile_matcher();

    let mut hits: Vec<String> = Vec::new();
    for entry in WalkBuilder::new(&root)
        .hidden(false)
        .git_ignore(true)
        .build()
    {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        let p = entry.path();
        let rel = p.strip_prefix(&root).unwrap_or(p);
        let basename: &Path = p.file_name().map(Path::new).unwrap_or(p);
        if g.is_match(rel) || g.is_match(basename) {
            hits.push(p.display().to_string());
            crate::read_cache::mark_seen_by_search(p);
            if hits.len() >= MAX_RESULTS {
                break;
            }
        }
    }
    if hits.is_empty() {
        return Ok(format!(
            "(no matches for '{}' under {})\n",
            pattern,
            root.display()
        ));
    }
    Ok(hits.join("\n") + "\n")
}
