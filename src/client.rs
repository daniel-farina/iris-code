//! Thin HTTP/SSE client for the MTPLX OpenAI-compatible endpoint.
//!
//! Streaming protocol: server sends `data: {json}\n\n` lines, terminated by
//! `data: [DONE]`. We assemble per-choice `delta.content` into a running string
//! and per-index `delta.tool_calls` deltas into completed `ToolCall`s.

use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::Value;
use std::time::{Duration, Instant};
use tokio::io::{AsyncWrite, AsyncWriteExt};

use crate::pretty::PrettyOut;
use crate::schema::{
    ChatMessage, ChatRequest, FunctionCall, StreamChunk, StreamError, ToolCall, Usage,
};

pub struct MtplxClient {
    http: Client,
    base_url: String,
    session_id: String,
    model: String,
}

#[derive(Debug, Clone, Copy)]
pub struct SamplingOpts {
    pub temperature: f32,
    pub top_p: f32,
    pub top_k: u32,
    pub max_tokens: u32,
    pub enable_thinking: bool,
}

impl Default for SamplingOpts {
    fn default() -> Self {
        // Match the values opencode uses for this model so we get apples-to-apples
        // generation behavior.
        Self {
            temperature: 0.6,
            top_p: 0.95,
            top_k: 20,
            // Set effectively-unlimited so the model decides when to stop,
            // not the sampler cap. The MTPLX server clamps internally to
            // (context_length - prompt_tokens), so picking a number larger
            // than the context window is safe — it just means "no cap from
            // our side". Was 16384, which truncated mid-explanation on
            // long file reads and made the model look like it had stopped
            // when it was actually capped. --max-tokens still overrides.
            max_tokens: 128_000,
            enable_thinking: false,
        }
    }
}

#[derive(Debug, Default)]
pub struct StreamResult {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
    pub ttft: Option<Duration>,
    pub total: Duration,
    /// Set when MTPLX emitted a `finish_reason="error"` chunk with a
    /// structured `error` field. Lets the agent loop surface the actual
    /// server-side exception message instead of swallowing it.
    pub error: Option<StreamError>,
}

/// Live throughput / metrics bar.
///
/// Renders a single \r-terminated colorized status line on stderr every
/// refresh_ms while a request is in flight. Pinned to the same line so
/// streamed content on stdout flows above it cleanly.
///
/// Phases shown:
/// - "prefill"  : request sent, waiting for first token
/// - "decoding" : first token arrived; rate computed from time-since-first-token
/// - "done"     : final summary line (with newline so it stays in scrollback)
///
/// Token count uses the count of SSE delta chunks that carried content,
/// which matches the server's per-token streaming with stream-interval=1.
/// The server's true count from `usage.completion_tokens` is used in the
/// final summary when available.
///
/// Disabled if stderr isn't a TTY or `MLX_CODE_NO_LIVE_TPS=1` is set.
struct LiveTps {
    enabled: bool,
    request_started: Instant,
    streaming_started: Option<Instant>,
    last_render: Instant,
    refresh: Duration,
    token_chunks: u32,
    content_lines: u32,
    last_line_len: usize,
    label: String,
}

impl LiveTps {
    fn new(request_started: Instant, label: impl Into<String>) -> Self {
        let enabled = std::env::var("MLX_CODE_NO_LIVE_TPS").ok().as_deref() != Some("1")
            && std::io::IsTerminal::is_terminal(&std::io::stderr());
        let mut tps = Self {
            enabled,
            request_started,
            streaming_started: None,
            last_render: request_started - Duration::from_secs(1),
            refresh: Duration::from_millis(100),
            token_chunks: 0,
            content_lines: 0,
            last_line_len: 0,
            label: label.into(),
        };
        // Render the initial prefill line right away.
        tps.render();
        tps
    }

    /// Count one SSE chunk that carried content/tool-call delta.
    fn tick(&mut self) {
        self.tick_text("");
    }

    /// Like tick(), but also counts newlines in the delta text so the bar can
    /// display lines-created.
    fn tick_text(&mut self, text: &str) {
        if !self.enabled {
            return;
        }
        if self.streaming_started.is_none() {
            self.streaming_started = Some(Instant::now());
        }
        self.token_chunks += 1;
        self.content_lines += text.bytes().filter(|&b| b == b'\n').count() as u32;
        if self.last_render.elapsed() < self.refresh {
            return;
        }
        self.last_render = Instant::now();
        self.render();
    }

    /// Periodic re-render even when no chunk arrived (so prefill phase ticks).
    fn heartbeat(&mut self) {
        if !self.enabled {
            return;
        }
        if self.last_render.elapsed() < self.refresh {
            return;
        }
        self.last_render = Instant::now();
        self.render();
    }

    /// Erase the current live line and reset bookkeeping so the next render
    /// re-paints from scratch. Used right before printing tool-call output so
    /// the bar doesn't get mixed into the tool stream.
    fn clear_line(&mut self) {
        if !self.enabled {
            return;
        }
        use std::io::Write;
        // In sticky-bar mode the metrics live at the bottom row; just blank it.
        if crate::sticky_bar::supported() && self.last_line_len > 0 {
            crate::sticky_bar::paint_bottom("", 0);
        } else {
            let mut stderr = std::io::stderr();
            let _ = write!(stderr, "\r{:width$}\r", "", width = self.last_line_len);
            let _ = stderr.flush();
        }
        self.last_line_len = 0;
        self.last_render -= Duration::from_secs(60);
    }

    fn render(&mut self) {
        if !self.enabled {
            return;
        }
        use crate::theme::{accent, dim, good, highlight, warn, RESET};
        use std::io::Write;
        let total = self.request_started.elapsed().as_secs_f64().max(0.001);
        let line = if let Some(start) = self.streaming_started {
            let stream_elapsed = start.elapsed().as_secs_f64().max(0.001);
            let tps = (self.token_chunks as f64) / stream_elapsed;
            format!(
                "{d}─[{a}{label}{d}]─ {g}{tok:>4}{d} tok ─ {h}{lines:>4}{d} lines ─ {g}{tps:>5.1}{d} tok/s ─ ttft {w}{ttft:.1}s{d} ─ stream {w}{se:.1}s{d} ─ total {w}{tot:.1}s{r}",
                d = dim(), a = accent(), g = good(), h = highlight(), w = warn(), r = RESET,
                label = self.label,
                tok = self.token_chunks,
                lines = self.content_lines,
                tps = tps,
                ttft = (total - stream_elapsed).max(0.0),
                se = stream_elapsed,
                tot = total,
            )
        } else {
            let dots = (((total * 4.0) as usize) % 4) + 1;
            let dotstr: String = ".".repeat(dots);
            format!(
                "{d}─[{a}{label}{d}]─ {w}prefill{dots:<4}{d} {tot:.1}s{r}",
                d = dim(),
                a = accent(),
                w = warn(),
                r = RESET,
                label = self.label,
                dots = dotstr,
                tot = total,
            )
        };
        let visible_len = strip_ansi_len(&line);

        // Right-aligned secondary metrics: cache hit rate (if active).
        let right = build_right_aligned_metrics();
        let right_visible = strip_ansi_len(&right);

        if crate::sticky_bar::enter() {
            crate::sticky_bar::paint_bottom_with_right(&line, visible_len, &right, right_visible);
            self.last_line_len = visible_len;
            return;
        }
        let pad = self.last_line_len.saturating_sub(visible_len);
        let mut stderr = std::io::stderr();
        let _ = write!(stderr, "\r{line}{:pad$}", "", pad = pad);
        let _ = stderr.flush();
        self.last_line_len = visible_len;
    }

    fn finish(&mut self, usage_completion_tokens: Option<u32>, prompt_tokens: Option<u32>) {
        if !self.enabled {
            return;
        }
        use std::io::Write;
        let mut stderr = std::io::stderr();
        // Release sticky region if we were using it; otherwise blank the inline line.
        if crate::sticky_bar::supported() {
            crate::sticky_bar::leave();
        } else {
            let _ = write!(stderr, "\r{:width$}\r", "", width = self.last_line_len);
        }
        let total = self.request_started.elapsed().as_secs_f64().max(0.001);
        let stream_elapsed = self
            .streaming_started
            .map(|s| s.elapsed().as_secs_f64().max(0.001));
        let count = usage_completion_tokens.unwrap_or(self.token_chunks);
        let source_tag = if usage_completion_tokens.is_some() {
            ""
        } else {
            " (est)"
        };
        let stream_rate = stream_elapsed.map(|s| (count as f64) / s);
        let ttft = stream_elapsed.map(|s| total - s).unwrap_or(total);
        let prompt_str = prompt_tokens
            .map(|n| n.to_string())
            .unwrap_or_else(|| "?".to_string());
        let line = match (stream_elapsed, stream_rate) {
            (Some(s), Some(r)) => format!(
                "\x1b[2m─[\x1b[0;36m{}\x1b[2m]─ \x1b[0;32m{}\x1b[2m tok{} ─ \x1b[0;35m{}\x1b[2m lines ─ \x1b[0;32m{:.1}\x1b[2m tok/s ─ ttft \x1b[0;33m{:.1}s\x1b[2m ─ stream \x1b[0;33m{:.1}s\x1b[2m ─ total \x1b[0;33m{:.1}s\x1b[2m ─ prompt \x1b[0;35m{}\x1b[0m\n",
                self.label, count, source_tag, self.content_lines, r, ttft, s, total, prompt_str,
            ),
            _ => format!(
                "\x1b[2m─[\x1b[0;36m{}\x1b[2m]─ \x1b[0;32m{}\x1b[2m tok{} ─ \x1b[0;35m{}\x1b[2m lines ─ total \x1b[0;33m{:.1}s\x1b[2m ─ prompt \x1b[0;35m{}\x1b[0m\n",
                self.label, count, source_tag, self.content_lines, total, prompt_str,
            ),
        };
        let _ = stderr.write_all(line.as_bytes());
        let _ = stderr.flush();
    }
}

/// Build the right-aligned secondary metrics chunk for the live bar.
/// Currently shows read-cache hit rate when there's been activity.
/// Returns empty when there's nothing useful to show, so the caller can
/// skip alignment work.
fn build_right_aligned_metrics() -> String {
    use crate::theme::{accent, dim, good, RESET};
    let s = crate::read_cache::stats();
    let total = s.hits + s.misses;
    if total == 0 {
        return String::new();
    }
    let pct = (100.0 * s.hits as f64 / total as f64) as u32;
    format!(
        "{d}cache {a}{h}/{t}{d} ({g}{p}%{d}){r}",
        d = dim(),
        a = accent(),
        g = good(),
        r = RESET,
        h = s.hits,
        t = total,
        p = pct,
    )
}

fn strip_ansi_len(s: &str) -> usize {
    // Approximate: strip ESC[...m sequences, count the rest as bytes (close
    // enough for monospace ASCII).
    let mut count = 0usize;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // skip CSI
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            count += 1;
        }
    }
    count
}

impl MtplxClient {
    pub fn new(
        base_url: impl Into<String>,
        session_id: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self> {
        let http = Client::builder()
            // streaming responses can take a while; don't time out on idle SSE
            .timeout(Duration::from_secs(0).max(Duration::from_secs(900)))
            .build()
            .context("building reqwest client")?;
        Ok(Self {
            http,
            base_url: base_url.into(),
            session_id: session_id.into(),
            model: model.into(),
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Replace the active session id. Used by `/new` to start a fresh
    /// conversation that doesn't share MTPLX's prefix cache with the prior
    /// session — otherwise the new conversation would inherit cached
    /// prefixes from the old one and leak partial state.
    pub fn set_session_id(&mut self, sid: impl Into<String>) {
        self.session_id = sid.into();
    }

    /// Stream a chat-completion. `out` receives content tokens as they arrive
    /// (typically stdout). Tool-call deltas are accumulated silently.
    pub async fn stream<W: AsyncWrite + Unpin>(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[Value]>,
        opts: SamplingOpts,
        mut out: W,
    ) -> Result<StreamResult> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let body = ChatRequest {
            model: &self.model,
            messages,
            tools,
            stream: true,
            temperature: Some(opts.temperature),
            top_p: Some(opts.top_p),
            top_k: Some(opts.top_k),
            max_tokens: Some(opts.max_tokens),
            chat_template_kwargs: Some(
                serde_json::json!({ "enable_thinking": opts.enable_thinking }),
            ),
        };

        // Optional request-body dump for cache diagnostics. Set MLX_CODE_DUMP_REQ
        // to a directory path to write each /v1/chat/completions body as JSON.
        // Off unless the env var is set, so zero overhead in normal use.
        if let Ok(dir) = std::env::var("MLX_CODE_DUMP_REQ") {
            let _ = std::fs::create_dir_all(&dir);
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::path::PathBuf::from(&dir)
                .join(format!("req-{}-{}.json", stamp, std::process::id()));
            if let Ok(json) = serde_json::to_string_pretty(&body) {
                let _ = std::fs::write(&path, json);
            }
        }

        let started = Instant::now();
        let resp = self
            .http
            .post(&url)
            .header("x-mtplx-session-id", &self.session_id)
            .header("accept", "text/event-stream")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {}", url))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("MTPLX returned HTTP {}: {}", status, text));
        }

        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
        let mut result = StreamResult::default();
        let mut ttft_set = false;
        let label = std::env::var("MLX_CODE_TPS_LABEL").unwrap_or_else(|_| "hip".into());
        let mut live = LiveTps::new(started, label);
        let mut pretty = PrettyOut::new();
        // tool_calls accumulator: index -> (id, name, args_buf)
        let mut acc: Vec<(String, String, String)> = Vec::new();

        while let Some(chunk) = stream.next().await {
            let chunk: Bytes = chunk.context("reading stream chunk")?;
            // Even if this chunk is just SSE framing with no token, refresh
            // the live bar so the prefill phase animates.
            live.heartbeat();
            buf.extend_from_slice(&chunk);
            // Split on \n\n event boundaries
            while let Some(pos) = find_double_newline(&buf) {
                let event = buf.drain(..pos + 2).collect::<Vec<u8>>();
                let event_text = match std::str::from_utf8(&event) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                for line in event_text.lines() {
                    let line = line.trim_end_matches('\r');
                    if let Some(payload) = line.strip_prefix("data:") {
                        let payload = payload.trim();
                        if payload.is_empty() {
                            continue;
                        }
                        if payload == "[DONE]" {
                            // drain pending bytes after [DONE] (should be none)
                            buf.clear();
                            break;
                        }
                        let parsed: StreamChunk = match serde_json::from_str(payload) {
                            Ok(v) => v,
                            Err(e) => {
                                // Don't bail on a single malformed line; log to stderr.
                                eprintln!(
                                    "\n[hip] failed to parse SSE chunk: {} ({})",
                                    e,
                                    truncate(payload, 200)
                                );
                                continue;
                            }
                        };
                        if let Some(usage) = parsed.usage {
                            result.usage = Some(usage);
                        }
                        if let Some(err) = parsed.error {
                            result.error = Some(err);
                        }
                        for choice in parsed.choices {
                            if let Some(reason) = choice.finish_reason {
                                result.finish_reason = Some(reason);
                            }
                            if let Some(text) = choice.delta.content {
                                if !text.is_empty() {
                                    if !ttft_set {
                                        result.ttft = Some(started.elapsed());
                                        ttft_set = true;
                                    }
                                    if pretty.enabled {
                                        // Pretty mode: clear status bar so
                                        // the section header doesn't collide
                                        // with the bar's `\r` line, then
                                        // route content through PrettyOut
                                        // (which handles <think> detection).
                                        live.clear_line();
                                        pretty.push_content(&text);
                                    } else {
                                        out.write_all(text.as_bytes()).await.ok();
                                        out.flush().await.ok();
                                    }
                                    result.content.push_str(&text);
                                    live.tick_text(&text);
                                }
                            }
                            if let Some(tcs) = choice.delta.tool_calls {
                                if !ttft_set {
                                    result.ttft = Some(started.elapsed());
                                    ttft_set = true;
                                }
                                let any_tc_content = tcs.iter().any(|d| {
                                    d.function.as_ref().is_some_and(|f| {
                                        f.arguments.as_ref().is_some_and(|a| !a.is_empty())
                                            || f.name.as_ref().is_some_and(|n| !n.is_empty())
                                    })
                                });
                                if any_tc_content {
                                    live.tick();
                                }
                                for d in tcs {
                                    let idx = d.index as usize;
                                    while acc.len() <= idx {
                                        acc.push((String::new(), String::new(), String::new()));
                                    }
                                    let slot = &mut acc[idx];
                                    if let Some(id) = d.id {
                                        if !id.is_empty() {
                                            slot.0 = id;
                                        }
                                    }
                                    if let Some(f) = d.function {
                                        if let Some(name) = f.name {
                                            if !name.is_empty() {
                                                slot.1 = name;
                                            }
                                        }
                                        if let Some(args) = f.arguments {
                                            slot.2.push_str(&args);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        result.total = started.elapsed();
        // Close out any open pretty section before the bar's final summary so
        // the borders end cleanly above it.
        pretty.flush_end();
        live.finish(
            result.usage.as_ref().and_then(|u| u.completion_tokens),
            result.usage.as_ref().and_then(|u| u.prompt_tokens),
        );
        result.tool_calls = acc
            .into_iter()
            .enumerate()
            .filter(|(_, (_, name, _))| !name.is_empty())
            .map(|(i, (id, name, args))| ToolCall {
                id: if id.is_empty() {
                    format!("call_{i}")
                } else {
                    id
                },
                kind: "function".to_string(),
                function: FunctionCall {
                    name,
                    arguments: args,
                },
            })
            .collect();

        Ok(result)
    }
}

fn find_double_newline(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\n\n")
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}...", &s[..n])
    }
}
