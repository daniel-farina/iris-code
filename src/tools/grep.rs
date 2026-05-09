//! `grep` tool: ripgrep-style search using the `ignore` crate.

use anyhow::{anyhow, Result};
use futures_util::future::FutureExt;
use ignore::WalkBuilder;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use super::Tool;

const MAX_HITS: usize = 50;

pub fn tool() -> Tool {
    Tool {
        name: "grep",
        schema: json!({
            "type": "function",
            "function": {
                "name": "grep",
                "description": "Search files for a regex. Honors .gitignore. Returns up to 50 file:line:text hits.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "regex (Rust regex syntax) or literal substring" },
                        "path": { "type": "string", "description": "directory or file to search; default cwd" },
                        "glob": { "type": "string", "description": "optional file glob filter, e.g. *.rs" }
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
        .ok_or_else(|| anyhow!("grep: missing pattern"))?
        .to_string();
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .map(|s| PathBuf::from(shellexpand::tilde(s).into_owned()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let glob = args
        .get("glob")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let matcher: Matcher = match regex::Regex::new(&pattern) {
        Ok(re) => Matcher::Regex(re),
        Err(_) => Matcher::Literal(pattern.clone()),
    };

    let glob_match = glob
        .as_deref()
        .map(|g| globset::Glob::new(g).map(|gg| gg.compile_matcher()))
        .transpose()?;

    let mut hits = Vec::new();
    let walker = WalkBuilder::new(&path)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build();

    'outer: for entry in walker {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        let p = entry.path();
        if let Some(m) = &glob_match {
            let basename = p.file_name().map(|f| Path::new(f)).unwrap_or(p);
            if !m.is_match(basename) {
                continue;
            }
        }
        if let Ok(meta) = entry.metadata() {
            if meta.len() > 8 * 1024 * 1024 {
                continue;
            }
        }
        let f = match std::fs::File::open(p) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let reader = BufReader::new(f);
        for (i, line) in reader.lines().enumerate() {
            let Ok(line) = line else { break };
            if line.len() > 4000 {
                continue;
            }
            if matcher.is_match(&line) {
                hits.push(format!("{}:{}:{}", p.display(), i + 1, line));
                if hits.len() >= MAX_HITS {
                    break 'outer;
                }
            }
        }
    }

    if hits.is_empty() {
        Ok(format!(
            "(no matches for /{}/ in {})\n",
            pattern,
            path.display()
        ))
    } else {
        Ok(hits.join("\n") + "\n")
    }
}

enum Matcher {
    Regex(regex::Regex),
    Literal(String),
}
impl Matcher {
    fn is_match(&self, s: &str) -> bool {
        match self {
            Matcher::Regex(r) => r.is_match(s),
            Matcher::Literal(l) => s.contains(l),
        }
    }
}
