//! `tree` tool: compact recursive directory view, gitignore-aware.
//!
//! Cheaper than calling `list` repeatedly to map a project. Designed to give
//! the agent a one-call "what does this codebase look like" answer, with
//! sensible defaults that keep the output bounded for large repos.

use anyhow::{anyhow, Context, Result};
use futures_util::future::FutureExt;
use ignore::WalkBuilder;
use serde_json::{json, Value};
use std::path::PathBuf;

use super::Tool;

const DEFAULT_DEPTH: usize = 2;
const MAX_ENTRIES: usize = 500;
const SKIP_SEGMENTS: &[&str] = &[
    "/target/",
    "/node_modules/",
    "/dist/",
    "/.next/",
    "/.cache/",
    "/build/",
    "/.git/",
];

pub fn tool() -> Tool {
    Tool {
        name: "tree",
        schema: json!({
            "type": "function",
            "function": {
                "name": "tree",
                "description": "Recursive directory view, gitignore-aware. Default depth 2, max 500 entries. Skips build/dependency dirs.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "root dir; default cwd" },
                        "depth": { "type": "integer", "description": "max depth (default 2, cap 6)" },
                        "show_hidden": { "type": "boolean", "description": "include dotfiles (default false)" }
                    }
                }
            }
        }),
        exec: |args| async move { run(args).await }.boxed(),
    }
}

async fn run(args: Value) -> Result<String> {
    let root = args
        .get("path")
        .and_then(|v| v.as_str())
        .map(|s| PathBuf::from(shellexpand::tilde(s).into_owned()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let depth = args
        .get("depth")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(DEFAULT_DEPTH)
        .clamp(1, 6);
    let show_hidden = args
        .get("show_hidden")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let meta = std::fs::metadata(&root).with_context(|| format!("stat {}", root.display()))?;
    if !meta.is_dir() {
        return Err(anyhow!("tree: not a directory: {}", root.display()));
    }

    let mut walker = WalkBuilder::new(&root);
    walker
        .hidden(!show_hidden)
        .git_ignore(true)
        .max_depth(Some(depth + 1));
    let mut entries: Vec<(usize, String, &'static str, u64)> = Vec::new();
    let mut total_files = 0usize;
    let mut total_dirs = 0usize;
    let mut total_bytes: u64 = 0;
    let mut truncated = false;

    for e in walker.build() {
        let Ok(e) = e else { continue };
        let path = e.path();
        if let Some(s) = path.to_str() {
            if SKIP_SEGMENTS.iter().any(|seg| s.contains(seg)) {
                continue;
            }
        }
        if path == root {
            continue;
        } // don't print the root itself in the tree body

        // depth = path components past root.
        let depth_here = path
            .components()
            .count()
            .saturating_sub(root.components().count());
        if depth_here == 0 || depth_here > depth {
            continue;
        }

        let ft = e.file_type();
        let (kind, size): (&'static str, u64) = match ft {
            Some(ft) if ft.is_dir() => {
                total_dirs += 1;
                ("dir", 0)
            }
            Some(ft) if ft.is_symlink() => ("link", 0),
            Some(_) => {
                let sz = e.metadata().map(|m| m.len()).unwrap_or(0);
                total_files += 1;
                total_bytes += sz;
                ("file", sz)
            }
            None => continue,
        };
        let name = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        if entries.len() >= MAX_ENTRIES {
            truncated = true;
            break;
        }
        entries.push((depth_here, name, kind, size));
    }

    entries.sort_by(|a, b| a.1.cmp(&b.1));

    let mut out = String::new();
    out.push_str(&format!(
        "{} (depth={}, hidden={})\n",
        root.display(),
        depth,
        show_hidden
    ));
    for (lvl, name, kind, size) in &entries {
        let indent = "  ".repeat(*lvl);
        match *kind {
            "dir" => out.push_str(&format!(
                "{}{}/\n",
                indent,
                name.split('/').next_back().unwrap_or(name)
            )),
            "link" => out.push_str(&format!(
                "{}{} -> link\n",
                indent,
                name.split('/').next_back().unwrap_or(name)
            )),
            _ => out.push_str(&format!(
                "{}{}  ({} b)\n",
                indent,
                name.split('/').next_back().unwrap_or(name),
                size
            )),
        }
    }
    out.push_str(&format!(
        "\n({} files, {} dirs, {} b{})\n",
        total_files,
        total_dirs,
        total_bytes,
        if truncated {
            format!("; truncated at {}", MAX_ENTRIES)
        } else {
            String::new()
        },
    ));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mkdir_p(p: &PathBuf) {
        let _ = std::fs::create_dir_all(p);
    }

    #[test]
    fn tree_lists_files_within_depth() {
        let dir = std::env::temp_dir().join(format!("mlx-tree-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        mkdir_p(&dir);
        std::fs::write(dir.join("a.txt"), "hi").unwrap();
        let sub = dir.join("sub");
        mkdir_p(&sub);
        std::fs::write(sub.join("b.txt"), "hello").unwrap();
        let deep = sub.join("deeper");
        mkdir_p(&deep);
        std::fs::write(deep.join("c.txt"), "world").unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();

        // depth=1 -> a.txt and sub/ visible, b.txt + deeper/ hidden
        let args = json!({"path": dir.to_string_lossy(), "depth": 1});
        let out = rt.block_on(run(args)).unwrap();
        assert!(out.contains("a.txt"), "depth=1 missing a.txt:\n{}", out);
        assert!(out.contains("sub/"), "depth=1 missing sub/:\n{}", out);
        assert!(
            !out.contains("b.txt"),
            "depth=1 should NOT list b.txt:\n{}",
            out
        );

        // depth=2 -> b.txt visible, c.txt still hidden
        let args = json!({"path": dir.to_string_lossy(), "depth": 2});
        let out = rt.block_on(run(args)).unwrap();
        assert!(out.contains("b.txt"), "depth=2 missing b.txt:\n{}", out);
        assert!(
            !out.contains("c.txt"),
            "depth=2 should NOT list c.txt:\n{}",
            out
        );

        // depth=3 -> c.txt visible
        let args = json!({"path": dir.to_string_lossy(), "depth": 3});
        let out = rt.block_on(run(args)).unwrap();
        assert!(out.contains("c.txt"), "depth=3 missing c.txt:\n{}", out);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tree_rejects_non_directory() {
        let dir = std::env::temp_dir().join(format!("mlx-tree-notdir-{}.txt", std::process::id()));
        std::fs::write(&dir, "data").unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let args = json!({"path": dir.to_string_lossy()});
        let res = rt.block_on(run(args));
        assert!(res.is_err());
        let _ = std::fs::remove_file(&dir);
    }
}
