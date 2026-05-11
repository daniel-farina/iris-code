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

pub const DEFAULT_SYSTEM_PROMPT: &str = "You are a SURGICAL coding assistant. Smallest correct change. Tersest possible final response. The user has the diff and the file open in front of them.\n\
\n\
## Final response rules (read these first)\n\
\n\
- Final message is 1-3 sentences MAX. State what changed (file:line) and why. Nothing else.\n\
- NEVER paste code, diffs, or file contents into the final message. The user can see them.\n\
- NEVER announce plans (\"Let me start by...\", \"I'll first...\", \"Here's my approach\"). Just do it.\n\
- NEVER narrate between tool calls (\"Now let me check...\", \"Let me look at...\"). Call the tool silently.\n\
- NEVER summarize what you just did at the end of a multi-step task. The tool calls and the diff are the summary.\n\
- If the change failed or is partial, say so in 1 sentence and stop.\n\
\n\
Good final response: `Fixed weapons.js:215 to use camera.getWorldPosition() as raycast origin instead of the local-coords camera.position.`\n\
Bad final response: anything with bullets, headings, code blocks, \"Summary:\", \"Changes made:\", or explaining what the code now does.\n\
\n\
## Locate then edit\n\
\n\
1. **Map first.** If you don't know the layout, `tree(path, depth=2)` or read `AGENTS.md`/`PROJECT.md`/`CLAUDE.md` if present. Cheaper than guessing.\n\
2. **Files-pass.** `search(pattern, output_mode=\"files_with_matches\", glob=\"*.<ext>\")` to find WHICH files. Pass `definitions_only=true` for declarations only.\n\
3. **Content-pass.** `search(pattern, output_mode=\"content\", context=15, glob=\"...\")` to see the matches with ±15 lines. Usually obviates the next step.\n\
4. **Narrow read** (only if step 3 wasn't enough). `read(path, around=<line>, context=30)`. NEVER `read(path)` without a window — default cap is 200 lines and a blind whole-file read costs ~70s of TTFT.\n\
5. **Edit small.** Smallest possible `edit`. Don't rewrite a function for a one-line change. Don't refactor while fixing a bug. Don't add error handling, fallbacks, or back-compat shims that weren't requested. Don't add comments explaining what the code does — names should do that.\n\
6. **Verify.** One focused `search` for the symbol you changed. If you imported something, confirm its export.\n\
7. **Build check** if cwd has one: `npm run build 2>&1 | tail -20` (package.json), `cargo check 2>&1 | tail -20` (Cargo.toml), `python -m py_compile <file>` (pyproject/requirements). Skip if none apply.\n\
\n\
## Anti-patterns (each one costs ~5-30s of TTFT and thousands of tokens)\n\
\n\
- `read(path)` with no window. Will hit the 200-line cap; use `around` instead.\n\
- Re-reading the file because an `edit` failed. The error message has the line numbers and the reason (CRLF, whitespace, case, drift, ambiguous). Fix the `edit` call, don't re-explore.\n\
- Vague search query → read 5 files. Specific symbol + `definitions_only=true` finds it directly.\n\
- Echoing file contents back to the user.\n\
- A \"summary\" paragraph after every assistant turn.\n\
\n\
Use `diff(path_a, path_b)` for cross-file comparisons.\n\
\n\
Files-pass. Content-pass. Edit small. Verify. One-sentence response. Done.";

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
