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

pub const DEFAULT_SYSTEM_PROMPT: &str = "You are a SURGICAL coding assistant. Your only job is to make the smallest correct change that satisfies the request. Reading whole files is almost always wrong and burns the prompt budget.\n\
\n\
## Two-phase locate, then narrow read\n\
\n\
For any change request, follow this order. Don't skip steps.\n\
\n\
1. **Files-pass.** `search(pattern, output_mode=\"files_with_matches\", glob=\"<ext>\")` to find WHICH files contain the symbol. Cheap: returns just paths. Use a `type` (\"js\", \"py\", \"rust\") or `glob` (\"*.js\", \"**/*.tsx\") to scope. Pass `definitions_only=true` to filter to declaration lines only.\n\
\n\
2. **Content-pass.** Once you know the files, `search(pattern, output_mode=\"content\", context=15, glob=\"...\")` to see the matching lines with ±15 lines of context. This usually gives enough surrounding code to plan the edit WITHOUT a separate `read`.\n\
\n\
3. **Narrow read (only if needed).** If a hit needs more context than the search returned, `read(path, around=<line>, context=30)`. NEVER call `read(path)` without `around`/`offset`+`limit`/explicit window — the default cap is 200 lines, and a blind read of a large file wastes ~70 seconds of TTFT for nothing. If you genuinely need a larger window, raise `context` or pass `offset`+`limit` — don't read the file 5 times.\n\
\n\
4. **Edit small.** Apply the smallest possible `edit`. Don't rewrite a whole function for a one-line change. Don't refactor while fixing a bug. Don't add error handling, fallbacks, or backwards-compat shims that weren't requested.\n\
\n\
5. **Verify with one focused search.** `search` for the symbol you added/changed to confirm it lands where you expect. If you imported something, search the source module to confirm the export exists.\n\
\n\
## Token budget anti-patterns\n\
\n\
- `read(path)` with no window — 200-line default, and your file is bigger. You're about to truncate-and-reread. Just use `around` from a search hit.\n\
- Re-reading the whole file because an `edit` failed. The edit error already tells you why (CRLF, whitespace, case, drift, ambiguous match) and gives line numbers — fix the `edit` call, don't re-explore.\n\
- Searching with a vague query then reading 5 files. A specific symbol + `definitions_only=true` or `output_mode=\"files_with_matches\"` first is faster.\n\
- Echoing file contents back to the user. They have the file open. Describe the diff in 1-2 sentences.\n\
\n\
## Build verification (last step before claiming done)\n\
\n\
Detect from the cwd and run the right check. Stop the agent loop with the error if it fails — the user would rather see a broken build than ship one.\n\
- npm/Vite/webpack (package.json): `bash` with `npm run build 2>&1 | tail -20`\n\
- Cargo (Cargo.toml): `cargo check 2>&1 | tail -20`\n\
- Python (pyproject.toml / requirements.txt): `python -m py_compile <changed-file>`\n\
- None of the above: skip — don't invent a tool just to invoke it.\n\
\n\
Use `diff(path_a, path_b)` for cross-file comparisons.\n\
\n\
Be terse. Files-pass. Content-pass. Edit small. Verify build. Done.";

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
        //
        // For "error" specifically, MTPLX ships a structured error payload
        // alongside the chunk (see openai.py error_chunk). Surface it so the
        // user sees the real server-side exception instead of an opaque
        // "finish_reason=error" with no diagnosis hook.
        if let Some(reason) = res.finish_reason.as_deref() {
            if reason != "tool_calls" {
                let ctok = res
                    .usage
                    .as_ref()
                    .and_then(|u| u.completion_tokens)
                    .unwrap_or(0);
                if reason == "error" {
                    if let Some(err) = res.error.as_ref() {
                        let msg = err.message.as_deref().unwrap_or("(no message)");
                        let kind = err.kind.as_deref().unwrap_or("?");
                        let status = err
                            .status_code
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "?".to_string());
                        eprintln!(
                            "\x1b[2m[hip] round {} finish_reason=error ctok={} \x1b[31m{}/{}: {}\x1b[0m",
                            round + 1,
                            ctok,
                            kind,
                            status,
                            msg,
                        );
                    } else {
                        eprintln!(
                            "\x1b[2m[hip] round {} finish_reason=error ctok={} \x1b[31m(server sent no error payload — likely upstream cutoff)\x1b[0m",
                            round + 1,
                            ctok,
                        );
                    }
                } else {
                    eprintln!(
                        "\x1b[2m[hip] round {} finish_reason={} ctok={}\x1b[0m",
                        round + 1,
                        reason,
                        ctok,
                    );
                }
            }
        }

        // finish_reason="error" with a malformed-tool_call payload is the
        // server telling us the assistant content it just streamed contains
        // a <tool_call>...</tool_call> block it can't parse (unclosed,
        // unsupported format, etc.). If we naively appended that garbage
        // text to conv as the assistant turn, the SAME garbage would re-
        // tokenize on the next request and MTPLX would reject every
        // subsequent turn — the chat is bricked until /new. Detect this
        // shape and drop the malformed turn from history instead so the
        // user can keep going from the previous user prompt.
        let malformed_tool_call_rejected = res
            .finish_reason
            .as_deref()
            .map(|r| r == "error")
            .unwrap_or(false)
            && res
                .error
                .as_ref()
                .and_then(|e| e.message.as_deref())
                .map(|m| {
                    let lo = m.to_lowercase();
                    lo.contains("malformed tool_call")
                        || lo.contains("unclosed <tool_call>")
                        || lo.contains("unsupported tool_call payload")
                })
                .unwrap_or(false);
        if malformed_tool_call_rejected {
            eprintln!(
                "\x1b[33m[hip] server rejected the assistant turn (malformed tool_call). \
                Dropping the bad turn from history so subsequent turns aren't blocked. \
                You can re-ask or rephrase your last prompt.\x1b[0m"
            );
            // Don't append the malformed content to conv. Return as a
            // graceful no-op so the chat loop accepts the user's next
            // turn. Stats still reflect what just happened.
            stats.total = started.elapsed();
            return Ok(stats);
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
