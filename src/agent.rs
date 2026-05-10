//! Tool-using agent loop.
//!
//! On each round we send the conversation + tool specs to MTPLX, stream the
//! response, and:
//! - If the assistant turn ended with `tool_calls`, execute each one locally,
//!   append the assistant message + one `tool` message per result, and recurse.
//! - Otherwise the assistant produced final content and we stop.

use anyhow::Result;
use serde_json::Value;
use std::time::{Duration, Instant};
use tokio::io::{stdout, AsyncWriteExt};

use crate::client::{MtplxClient, SamplingOpts};
use crate::schema::ChatMessage;
use crate::tools::{self, Tool};

pub const DEFAULT_SYSTEM_PROMPT: &str = "You are a coding assistant. Use tools to read, search, and edit files. Make minimal targeted changes. \
When `edit` fails: the error message lists hints (CRLF, whitespace, case, drift) and the line numbers of all matches - use them to fix the next call instead of re-reading. \
When unsure where something lives, use `search` (returns ranked file:line:text); pass `definitions_only=true` to jump straight to the declaration. Then use `read(path, around=<line>)` to grab context around the hit instead of reading the whole file. \
After applying edits, verify with `bash` if the user asked to verify, or use `diff(path_a, path_b)` to compare two files (e.g. compare a freshly written file to a reference). Be concise.";

#[derive(Debug, Default)]
pub struct LoopStats {
    pub rounds: u32,
    pub total_tool_calls: u32,
    pub first_ttft: Option<Duration>,
    pub total: Duration,
    pub first_prompt_tokens: Option<u32>,
    pub total_completion_tokens: u32,
}

pub async fn run_loop(
    client: &MtplxClient,
    conv: &mut Vec<ChatMessage>,
    max_rounds: u32,
    opts: SamplingOpts,
) -> Result<LoopStats> {
    let tools = tools::registry();
    let specs = tools::tool_specs(&tools);
    let mut stats = LoopStats::default();
    let started = Instant::now();
    let mut out = stdout();

    for round in 0..max_rounds {
        stats.rounds = round + 1;
        let res = client.stream(conv, Some(&specs), opts, &mut out).await?;

        if stats.first_ttft.is_none() {
            stats.first_ttft = res.ttft;
        }
        if let Some(usage) = &res.usage {
            if stats.first_prompt_tokens.is_none() {
                stats.first_prompt_tokens = usage.prompt_tokens;
            }
            if let Some(c) = usage.completion_tokens {
                stats.total_completion_tokens += c;
            }
        }

        if !res.content.is_empty() {
            // Tail newline so subsequent tool output starts on its own line.
            out.write_all(b"\n").await.ok();
            out.flush().await.ok();
        }

        // finish_reason="length" means the model was cut off because it hit
        // max_tokens, NOT because it considered the task done. If we let the
        // loop exit here the user has to manually type "continue" — which
        // they've reported is annoying. Auto-continue once by appending the
        // partial assistant text and re-prompting. We cap at one auto-continue
        // per round so a runaway model can't pin the loop forever.
        let truncated_by_length = res
            .finish_reason
            .as_deref()
            .map(|r| r == "length")
            .unwrap_or(false);
        if truncated_by_length && res.tool_calls.is_empty() {
            // Print a visible marker so the user knows what's happening.
            crate::pretty::truncation_notice(opts.max_tokens);
            conv.push(ChatMessage::assistant_text(res.content.clone()));
            // Nudge the model to keep going. A bare "continue" turns into a
            // user message in the conversation; the assistant can then
            // continue from where it stopped without losing context.
            conv.push(ChatMessage::user("continue"));
            // Loop again; if it truncates a second time we DO exit so the
            // user can intervene.
            continue;
        }

        if res.tool_calls.is_empty() {
            // Final answer — append to history and exit.
            conv.push(ChatMessage::assistant_text(res.content.clone()));
            stats.total = started.elapsed();
            return Ok(stats);
        }

        // Append assistant message carrying the tool_calls and then run each one.
        let assistant_text = if res.content.trim().is_empty() {
            None
        } else {
            Some(res.content.clone())
        };
        conv.push(ChatMessage::assistant_tool_calls(
            assistant_text,
            res.tool_calls.clone(),
        ));

        for call in &res.tool_calls {
            stats.total_tool_calls += 1;
            // Pretty header + parsed args.
            crate::pretty::tool_call_header(&call.function.name);
            let pretty_args = crate::pretty::pretty_args(&call.function.arguments);
            crate::pretty::tool_call_args(&pretty_args);

            let (body, ok) = run_tool(&tools, &call.function.name, &call.function.arguments).await;
            crate::pretty::tool_result(&call.function.name, ok, &body, 30);
            conv.push(ChatMessage::tool_result(&call.id, body));
        }
    }

    stats.total = started.elapsed();
    Ok(stats)
}

async fn run_tool(tools: &[Tool], name: &str, raw_args: &str) -> (String, bool) {
    let parsed: Value = match serde_json::from_str(raw_args) {
        Ok(v) => v,
        Err(_) => {
            // Some servers send empty or whitespace arguments for no-arg calls.
            if raw_args.trim().is_empty() {
                Value::Object(Default::default())
            } else {
                return (
                    format!("error: tool arguments are not valid JSON: {}", raw_args),
                    false,
                );
            }
        }
    };
    let Some(tool) = tools::lookup(tools, name) else {
        return (format!("error: unknown tool '{}'", name), false);
    };
    match (tool.exec)(parsed).await {
        Ok(s) => (s, true),
        Err(e) => (format!("error: {}", e), false),
    }
}
