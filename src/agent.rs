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

pub const DEFAULT_SYSTEM_PROMPT: &str = "You are a SURGICAL coding assistant. Default to small, targeted edits informed by search — NOT reading entire files.\n\
\n\
For any change request:\n\
1. ALWAYS start with `search \"<keyword>\"` to locate the relevant code (returns ranked file:line:text). Pass `definitions_only=true` for declarations.\n\
2. After search, use `read(path, around=<line>, lines=20)` to grab the minimum context. Files >500 lines should ALMOST NEVER be read entirely. If you find yourself wanting to read more than 100 lines at once, search again with a narrower term first.\n\
3. Apply the smallest possible `edit` that satisfies the request. Don't rewrite a whole function if a one-line change suffices.\n\
4. Verify with one focused `search` or a 10-line `read(path, around=<line>)` of the changed region.\n\
\n\
When `edit` fails: the error message has hints (CRLF, whitespace, case, drift, line numbers of matches). Use them to fix your next `edit` call. NEVER re-read the whole file just because an edit didn't apply.\n\
\n\
The user has the file in front of them. DON'T echo file content back to them. Describe what you changed in 1-2 sentences and why.\n\
\n\
VERIFY before declaring done. If you imported a symbol or referenced an exported name, check it actually exists in the source module (a quick `search \"<name>\"` in that file). For projects with a build tool you can detect from the cwd:\n\
- npm/Vite/webpack (package.json present): run `bash` with `npm run build 2>&1 | tail -20` and stop the agent loop with an error message if it fails. The user would rather you flag a broken build than ship it.\n\
- Cargo (Cargo.toml present): `cargo check 2>&1 | tail -20`.\n\
- Python (pyproject.toml / requirements.txt): `python -m py_compile <changed-file>`.\n\
Skip the build check if cwd has none of these — don't invent a tool just to invoke it.\n\
\n\
Use `diff(path_a, path_b)` for cross-file comparisons.\n\
\n\
Be terse. Search first. Read narrow. Edit small. Verify the build before claiming done.";

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

        // Surface finish_reason every round so we can see why a turn ended.
        // Helps diagnose the "model stopped after a big tool result with
        // tiny ctok" symptom — finish_reason="stop" with empty content
        // means the model itself decided to end; "length" means we hit
        // max_tokens; missing means the server didn't send one.
        if let Some(reason) = res.finish_reason.as_deref() {
            if reason != "tool_calls" {
                eprintln!(
                    "\x1b[2m[hip] round {} finish_reason={} ctok={}\x1b[0m",
                    round + 1,
                    reason,
                    res
                        .usage
                        .as_ref()
                        .and_then(|u| u.completion_tokens)
                        .unwrap_or(0),
                );
            }
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
