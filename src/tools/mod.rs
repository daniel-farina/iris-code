//! Tool registry. Each tool is a JSON-schema definition (sent to the model)
//! plus a local executor (called when the model emits a tool_call).
//!
//! Phase 1 stub: registry is empty until tools are added in Phase 2.

use anyhow::Result;
use serde_json::Value;

pub mod bash;
pub mod diff;
pub mod edit;
pub mod glob;
pub mod grep;
pub mod list;
pub mod peek_log;
pub mod read;
pub mod search;
pub mod tree;

pub struct Tool {
    pub name: &'static str,
    /// JSON-schema definition the model sees in the tools array.
    pub schema: Value,
    /// Async executor; receives the parsed-arguments JSON and returns a string body.
    pub exec: ToolFn,
}

pub type ToolFn = fn(args: Value) -> futures_util::future::BoxFuture<'static, Result<String>>;

pub fn registry() -> Vec<Tool> {
    vec![
        read::tool(),
        grep::tool(),
        edit::tool(),
        bash::tool(),
        list::tool(),
        glob::tool(),
        search::tool(),
        diff::tool(),
        tree::tool(),
        peek_log::tool(),
    ]
}

/// Build the OpenAI-compatible `tools` array from the registry.
pub fn tool_specs(tools: &[Tool]) -> Vec<Value> {
    tools.iter().map(|t| t.schema.clone()).collect()
}

pub fn lookup<'a>(tools: &'a [Tool], name: &str) -> Option<&'a Tool> {
    tools.iter().find(|t| t.name == name)
}
