//! mlx-code: a tiny Rust CLI coding agent for a local MTPLX server.
//!
//! Phase 1: one-shot streaming client (no tools).
//! Phase 2: tool-using agent loop (read/grep/edit/bash/list/glob).
//! Phase 3: ripgrep-backed search tool.

mod agent;
mod client;
mod dry_run_log;
mod logo;
mod pretty;
mod read_cache;
mod repl;
mod runlog;
mod schema;
mod setup;
mod sparkline;
mod sticky_bar;
mod theme;
mod tools;
mod updater;

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{stdout, AsyncWriteExt};

use crate::client::{MtplxClient, SamplingOpts};
use crate::schema::ChatMessage;

#[derive(Parser, Debug)]
#[command(
    name = "hip",
    version,
    about = "hippo-code · lean coding agent for the local MTPLX model"
)]
struct Cli {
    /// User prompt. Positional, joined with spaces.
    #[arg(trailing_var_arg = true)]
    prompt: Vec<String>,

    /// Disable tool-use; just stream a single response (Phase 1 mode).
    /// Aliased as `--print` for parity with other coding agents.
    #[arg(long, alias = "print")]
    one_shot: bool,

    /// Save the response to this path. If omitted, no file is written.
    #[arg(long)]
    save: Option<PathBuf>,

    /// Extract the `<html>...</html>` block from the response and save to this path.
    /// Implies behaviour for the airplane.html acceptance test.
    #[arg(long)]
    save_html: Option<PathBuf>,

    /// Open the saved file in the default browser when done.
    #[arg(long)]
    open: bool,

    /// Override the system prompt (Phase 2/3 default is the built-in coding-agent prompt).
    #[arg(long)]
    system: Option<String>,

    /// Load the system prompt from a file (overrides --system if both set).
    /// Useful for project-specific agent instructions.
    #[arg(long)]
    system_file: Option<PathBuf>,

    /// Cap the agent loop at N tool-call rounds.
    #[arg(long, default_value_t = 30)]
    max_rounds: u32,

    /// Session id sent as `x-mtplx-session-id`. Reusing it warms the prefix cache.
    #[arg(long, env = "MLX_CODE_SESSION", default_value = "mlx-code-default")]
    session: String,

    /// MTPLX base URL.
    #[arg(long, env = "MLX_CODE_URL", default_value = "http://127.0.0.1:8088/v1")]
    url: String,

    /// Model id.
    #[arg(
        long,
        env = "MLX_CODE_MODEL",
        default_value = "mtplx-qwen36-27b-optimized-speed"
    )]
    model: String,

    /// Print timing/usage info on stderr after generation.
    #[arg(long)]
    stats: bool,

    /// Force interactive chat mode even if a prompt is given (uses prompt as
    /// the first turn). Without this flag, an empty prompt also enters chat.
    #[arg(long)]
    chat: bool,

    /// Show the model's <think>...</think> reasoning blocks (default ON).
    /// Renders them in a dim panel above the response. Pass `--hide-thinking`
    /// or set `MLX_CODE_SHOW_THINKING=0` to suppress.
    #[arg(long, hide = true)]
    show_thinking: bool,

    /// Hide the model's <think>...</think> reasoning blocks. Inverse of
    /// the (now default-on) thinking display.
    #[arg(long)]
    hide_thinking: bool,

    /// Show full output without truncation in tool result panels and
    /// other places that normally cap line counts.
    #[arg(long, env = "MLX_CODE_FULL_OUTPUT")]
    full_output: bool,

    /// Append a JSONL line per run to ~/.mlx-code/logs/runs.jsonl with
    /// metrics (tokens, TTFT, decode-rate, tool calls, success). Useful for
    /// tracking performance across many runs.
    #[arg(long, env = "MLX_CODE_LOG_RUNS", default_value_t = true)]
    log_runs: bool,

    /// Shortcut: enable --show-thinking, --full-output and --stats together.
    #[arg(short = 'v', long)]
    verbose: bool,

    /// Suppress the live status bar AND section panels. Useful when piping
    /// output to a file/another process.
    #[arg(short = 'q', long)]
    quiet: bool,

    /// Print a summary of the last N runs from ~/.mlx-code/logs/runs.jsonl
    /// and exit. Use with --summary-n to change N (default 10).
    #[arg(long)]
    summary: bool,

    /// Number of recent runs to show with --summary.
    #[arg(long, default_value_t = 10)]
    summary_n: usize,

    /// Run the smoke-test harness (HTML balance + node --check on .js files)
    /// against ~/mlx-code-out/ or a custom path. Exits 0 on all pass, 1 on any fail.
    #[arg(long)]
    smoke: bool,

    /// Path(s) to scan when using --smoke. Comma-separated list. Default
    /// scans ~/mlx-code-out/.
    #[arg(long, default_value = "~/mlx-code-out/")]
    smoke_path: String,

    /// After an agent or one-shot run completes, automatically run the smoke
    /// harness against the current working directory and report PASS/FAIL.
    /// Useful as a quick "did the agent break anything?" check.
    #[arg(long, env = "MLX_CODE_AUTO_SMOKE")]
    auto_smoke: bool,

    /// Snapshot the cwd file list before the agent runs and report a unified
    /// diff summary at the end (added / modified / removed). Limits to .py
    /// .js .mjs .ts .tsx .rs .html .css .json .md to keep noise down.
    #[arg(long)]
    diff: bool,

    /// Continue the most recent session (reuse the last session_id from
    /// ~/.mlx-code/logs/runs.jsonl). Cache stays warm; conversation history
    /// resets unless you also pass --chat.
    #[arg(short = 'c', long)]
    continue_last: bool,

    /// Sampler temperature (default 0.6).
    #[arg(long)]
    temperature: Option<f32>,

    /// Sampler top-p (default 0.95).
    #[arg(long)]
    top_p: Option<f32>,

    /// Sampler top-k (default 20).
    #[arg(long)]
    top_k: Option<u32>,

    /// Max output tokens per turn (default 16384).
    #[arg(long)]
    max_tokens: Option<u32>,

    /// Watch a path; re-run the prompt whenever any file under it changes
    /// (mtime poll, 1s interval). Press Ctrl-C to exit. Best with --print.
    #[arg(long)]
    watch: Option<PathBuf>,

    /// Glob pattern that filters which files trigger a --watch rerun.
    /// Defaults to all files. Examples: "*.rs", "src/**/*.ts".
    /// Pattern is matched against the relative path under --watch root.
    #[arg(long, value_name = "GLOB")]
    watch_pattern: Option<String>,

    /// Print the system prompt + tool spec JSON sizes (chars, lines, approx
    /// tokens) without sending anything. Useful for tracking prompt overhead
    /// as tools are added. Exits after printing.
    #[arg(long)]
    inspect_prompt: bool,

    /// Print the full prompt body as it would be sent to the server (system
    /// message + tools array, optional user message if a prompt was passed).
    /// Useful for verifying exact bytes are what you expect. Exits without
    /// sending anything.
    #[arg(long)]
    show_prompt: bool,

    /// Agent-loop dry run: every `edit` and `bash` call returns a "(dry_run)
    /// would..." preview instead of mutating the filesystem or running a
    /// command. Useful to ask "what would the agent do?" without committing.
    #[arg(long)]
    dry_run: bool,

    /// Run the first-run setup wizard explicitly (probe MTPLX, list models,
    /// print install hints). Auto-runs on first invocation; this flag forces
    /// it to run again.
    #[arg(long)]
    setup: bool,

    /// Download and install the latest GitHub release in place. Exits after
    /// the swap; restart iris to use the new version.
    #[arg(long)]
    update: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut cli = Cli::parse();

    // Default thinking display is ON. Resolve in priority order:
    // 1. --hide-thinking flag wins (explicit opt-out)
    // 2. MLX_CODE_SHOW_THINKING=0/false explicit env opt-out
    // 3. otherwise default to true (overrides clap's bool default of false)
    cli.show_thinking = if cli.hide_thinking {
        false
    } else {
        !matches!(
            std::env::var("MLX_CODE_SHOW_THINKING").as_deref(),
            Ok("0") | Ok("false") | Ok("off")
        )
    };

    if cli.verbose {
        cli.show_thinking = true;
        cli.full_output = true;
        cli.stats = true;
    }
    if cli.quiet {
        std::env::set_var("MLX_CODE_NO_LIVE_TPS", "1");
        std::env::set_var("MLX_CODE_NO_PRETTY", "1");
    }
    apply_pretty_env(cli.show_thinking, cli.full_output);

    // --dry-run: cascade into every mutation tool via env var so the model
    // doesn't need to know it's in dry-run mode.
    if cli.dry_run {
        std::env::set_var("MLX_CODE_DRY_RUN", "1");
        eprintln!("\x1b[2m─ dry-run ─ edit/bash will preview only; no writes\x1b[0m");
    }

    // --update: download + install latest release, exit.
    if cli.update {
        updater::do_update().await?;
        return Ok(());
    }

    // Background-cached update notice. The fetch is short-circuited via a
    // 24h cache so we only hit the GitHub API once a day; HTTP timeout is
    // 2s so we never noticeably block startup. Skip in --quiet contexts.
    if !cli.quiet && std::io::IsTerminal::is_terminal(&std::io::stderr()) {
        if let Some(notice) = updater::update_notice_if_any().await {
            eprintln!("{}", notice);
        }
    }

    // --setup OR first run: probe MTPLX and walk the user through any
    // missing pieces. The wizard returns false if it printed setup hints
    // and the user should restart; in that case we exit cleanly.
    if cli.setup || (setup::is_first_run() && std::io::IsTerminal::is_terminal(&std::io::stderr()))
    {
        let proceed = setup::run_wizard(&cli.url).await?;
        if cli.setup || !proceed {
            return Ok(());
        }
    }

    // --inspect-prompt: print prompt-overhead stats and exit.
    if cli.inspect_prompt {
        print_inspect_prompt(&cli);
        return Ok(());
    }
    // --show-prompt: dump the full prompt body and exit.
    if cli.show_prompt {
        print_show_prompt(&cli);
        return Ok(());
    }

    // --continue / -c: pick the most recent session_id from runs.jsonl.
    if cli.continue_last {
        match find_last_session_id() {
            Some(sid) => {
                eprintln!("[hip] resuming session: {}", sid);
                cli.session = sid;
            }
            None => {
                eprintln!("[hip] --continue: no prior runs found in ~/.mlx-code/logs/runs.jsonl");
            }
        }
    }

    // Load --system-file (overrides --system).
    if let Some(p) = &cli.system_file {
        match std::fs::read_to_string(expand(p)?) {
            Ok(s) => cli.system = Some(s.trim().to_string()),
            Err(e) => {
                eprintln!("[hip] failed to read --system-file {}: {}", p.display(), e);
                std::process::exit(2);
            }
        }
    }

    if cli.summary {
        return print_run_summary(cli.summary_n);
    }

    if cli.smoke {
        return run_smoke(&cli.smoke_path);
    }

    // --watch: re-run the prompt whenever any file under PATH changes (mtime
    // poll). Hits the same code path as a normal invocation per change.
    if let Some(watch_path) = cli.watch.clone() {
        let prompt = cli.prompt.join(" ");
        if prompt.trim().is_empty() {
            eprintln!("[hip] --watch requires a prompt");
            std::process::exit(2);
        }
        let client = MtplxClient::new(&cli.url, &cli.session, &cli.model)?;
        run_watch_loop(&cli, &client, &prompt, &watch_path).await?;
        return Ok(());
    }

    let prompt = cli.prompt.join(" ");
    let client = MtplxClient::new(&cli.url, &cli.session, &cli.model)?;

    // Interactive chat: explicit --chat flag, or no prompt and stdin is a TTY.
    if cli.chat || (prompt.trim().is_empty() && std::io::IsTerminal::is_terminal(&std::io::stdin()))
    {
        return run_chat(
            &cli,
            &client,
            if prompt.trim().is_empty() {
                None
            } else {
                Some(prompt)
            },
        )
        .await;
    }

    if prompt.trim().is_empty() {
        eprintln!("usage: hip [--one-shot] [--chat] [--save PATH] [--save-html PATH] [--open] <prompt...>");
        std::process::exit(2);
    }

    let result = if cli.one_shot {
        run_one_shot(&cli, &client, &prompt).await
    } else {
        run_agent(&cli, &client, &prompt).await
    };
    // Post-run dry-run summary: shown regardless of run mode (one-shot/agent).
    if cli.dry_run {
        print_dry_run_summary();
    }
    result
}

async fn run_chat(cli: &Cli, client: &MtplxClient, first: Option<String>) -> Result<()> {
    use rustyline::error::ReadlineError;
    use rustyline::history::DefaultHistory;
    use rustyline::{Cmd, Editor, Event, KeyEvent, Modifiers, Result as RLResult};

    let system = cli
        .system
        .clone()
        .unwrap_or_else(|| agent::DEFAULT_SYSTEM_PROMPT.to_string());
    let mut conv: Vec<ChatMessage> = vec![ChatMessage::system(system)];

    print_chat_banner(client, cli);
    if cli.one_shot {
        eprintln!(
            "{}─ one-shot mode • tools disabled • each turn independent{}",
            theme::dim(),
            theme::RESET
        );
    }

    // Settings that the user can toggle at runtime via :commands.
    let mut show_thinking = cli.show_thinking;
    let mut full_output = cli.full_output;
    apply_pretty_env(show_thinking, full_output);

    // rustyline editor with custom Helper (tab completion + filename paths)
    // plus Alt+Enter bound to insert-newline for multiline input.
    let mut rl: Editor<repl::MlxHelper, DefaultHistory> = match Editor::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "[hip] rustyline init failed: {} - falling back to raw stdin",
                e
            );
            return run_chat_fallback(cli, client, first, conv, show_thinking, full_output).await;
        }
    };
    rl.set_helper(Some(repl::MlxHelper::new()));
    // Alt+Enter -> insert newline in the buffer (multiline editing).
    rl.bind_sequence(
        Event::KeySeq(vec![KeyEvent::new('\r', Modifiers::ALT)]),
        rustyline::EventHandler::Simple(Cmd::Newline),
    );
    let history_path = expand(&PathBuf::from("~/.mlx-code/history.txt")).ok();
    if let Some(h) = &history_path {
        if let Some(parent) = h.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = rl.load_history(h);
    }

    let mut pending = first;
    loop {
        let user_msg = if let Some(p) = pending.take() {
            eprintln!("[hip] (using initial prompt as first turn)");
            p
        } else {
            let prompt = "\x1b[1;36m> \x1b[0m";
            let line: RLResult<String> = rl.readline(prompt);
            match line {
                Ok(l) => {
                    let l_trim = l.trim().to_string();
                    if !l_trim.is_empty() {
                        let _ = rl.add_history_entry(&l_trim);
                    }
                    l_trim
                }
                Err(ReadlineError::Interrupted) => continue, // Ctrl-C: drop line, keep going
                Err(ReadlineError::Eof) => {
                    // Ctrl-D: exit
                    if let Some(h) = &history_path {
                        let _ = rl.save_history(h);
                    }
                    eprintln!("\n[hip] bye");
                    return Ok(());
                }
                Err(e) => {
                    eprintln!("[hip] readline error: {}", e);
                    return Ok(());
                }
            }
        };

        let trimmed = user_msg.trim();
        if trimmed.is_empty() {
            continue;
        }

        // meta-commands
        match trimmed {
            ":quit" | ":exit" | ":q" => {
                if let Some(h) = &history_path {
                    let _ = rl.save_history(h);
                }
                eprintln!("[hip] bye");
                return Ok(());
            }
            ":reset" => {
                let sys = conv.first().cloned().unwrap_or_else(|| {
                    ChatMessage::system(agent::DEFAULT_SYSTEM_PROMPT.to_string())
                });
                conv = vec![sys];
                eprintln!("[hip] conversation cleared (session_id unchanged so cache stays warm)");
                continue;
            }
            ":stats" => {
                eprintln!(
                    "[hip] turns_in_history={} session={} cwd={} show_thinking={} full_output={}",
                    conv.iter()
                        .filter(|m| m.role == "user" || m.role == "assistant")
                        .count(),
                    client.session_id(),
                    std::env::current_dir().unwrap_or_default().display(),
                    show_thinking,
                    full_output,
                );
                continue;
            }
            ":history" => {
                eprintln!(
                    "[hip] history file: {}",
                    history_path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "(unset)".into())
                );
                continue;
            }
            cmd if cmd.starts_with(":cwd ") => {
                let path = &cmd[5..].trim();
                if let Err(e) = std::env::set_current_dir(path) {
                    eprintln!("[hip] cd failed: {}", e);
                } else {
                    eprintln!(
                        "[hip] cwd={}",
                        std::env::current_dir().unwrap_or_default().display()
                    );
                }
                continue;
            }
            cmd if cmd.starts_with(":show-thinking ") => {
                show_thinking = matches!(cmd[15..].trim(), "on" | "1" | "true");
                apply_pretty_env(show_thinking, full_output);
                eprintln!("[hip] show_thinking = {}", show_thinking);
                continue;
            }
            cmd if cmd.starts_with(":full-output ") => {
                full_output = matches!(cmd[13..].trim(), "on" | "1" | "true");
                apply_pretty_env(show_thinking, full_output);
                eprintln!("[hip] full_output = {}", full_output);
                continue;
            }
            ":smoke" => {
                let _ = run_smoke_inplace(".");
                continue;
            }
            cmd if cmd.starts_with(":smoke ") => {
                let p = cmd[7..].trim();
                let _ = run_smoke_inplace(p);
                continue;
            }
            ":tools" => {
                let registry = tools::registry();
                eprintln!("─ tools ({}) ─", registry.len());
                for t in &registry {
                    let desc = t
                        .schema
                        .get("function")
                        .and_then(|f| f.get("description"))
                        .and_then(|d| d.as_str())
                        .unwrap_or("");
                    eprintln!("  {:<10} {}", t.name, desc);
                }
                continue;
            }
            ":overhead" => {
                let system = cli
                    .system
                    .clone()
                    .unwrap_or_else(|| agent::DEFAULT_SYSTEM_PROMPT.to_string());
                let registry = tools::registry();
                let specs = tools::tool_specs(&registry);
                let tools_json = serde_json::to_string(&specs).unwrap_or_default();
                let total_chars = system.chars().count() + tools_json.chars().count();
                let approx = (total_chars as f64 / 4.0).round() as usize;
                eprintln!(
                    "─ overhead ─ system+tools = {} chars / ~{} tokens / {} tools",
                    total_chars,
                    approx,
                    registry.len()
                );
                let cs = read_cache::stats();
                let lookups = cs.hits + cs.misses;
                if lookups > 0 {
                    let rate = 100.0 * cs.hits as f64 / lookups as f64;
                    eprintln!(
                        "─ read cache ─ {} entries / hits={}/{} ({:.0}% hit-rate)",
                        read_cache::len(),
                        cs.hits,
                        lookups,
                        rate
                    );
                }
                continue;
            }
            cmd if cmd.starts_with(":diff ") => {
                let rest = cmd[6..].trim();
                let parts: Vec<&str> = rest.splitn(2, char::is_whitespace).collect();
                if parts.len() != 2 {
                    eprintln!("[hip] usage: :diff <path_a> <path_b>");
                    continue;
                }
                let args = serde_json::json!({"path_a": parts[0], "path_b": parts[1]});
                let t = tools::diff::tool();
                match (t.exec)(args).await {
                    Ok(out) => eprint!("{}", out),
                    Err(e) => eprintln!("[hip] diff failed: {}", e),
                }
                continue;
            }
            cmd if cmd == ":tps" || cmd.starts_with(":tps ") => {
                let n: usize = cmd
                    .trim_start_matches(":tps")
                    .trim()
                    .parse()
                    .unwrap_or(20usize)
                    .clamp(1, 50);
                let path = shellexpand::tilde("~/.mlx-code/logs/runs.jsonl").into_owned();
                let body = std::fs::read_to_string(&path).unwrap_or_default();
                let rates: Vec<f64> = body
                    .lines()
                    .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                    .filter_map(|r| r.get("decode_tok_per_s").and_then(|v| v.as_f64()))
                    .filter(|v| *v > 0.0 && *v < 200.0)
                    .collect();
                if rates.is_empty() {
                    eprintln!("─ tps ─ no valid decode_tok_per_s entries in {}", path);
                } else {
                    let tail: Vec<f64> = rates.iter().rev().take(n).rev().cloned().collect();
                    let lo = tail.iter().cloned().fold(f64::INFINITY, f64::min);
                    let hi = tail.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                    let line = sparkline::render(&tail);
                    eprintln!(
                        "─ tps last {} ─ {}  range={:.0}-{:.0} t/s",
                        tail.len(),
                        line,
                        lo,
                        hi
                    );
                }
                continue;
            }
            cmd if cmd.starts_with(":theme") => {
                let arg = cmd.trim_start_matches(":theme").trim();
                if arg.is_empty() {
                    eprintln!(
                        "─ theme = {} (use: :theme dark|light|mono)",
                        theme::current().name()
                    );
                } else if let Some(t) = theme::Theme::parse(arg) {
                    theme::set_runtime(t);
                    eprintln!(
                        "{}─ theme set to {}{}",
                        theme::dim(),
                        t.name(),
                        theme::RESET
                    );
                } else {
                    eprintln!("[hip] unknown theme: {} (try dark/light/mono)", arg);
                }
                continue;
            }
            ":cache" => {
                let s = read_cache::stats();
                let entries = read_cache::len();
                let bytes = read_cache::bytes_held();
                let total_lookups = s.hits + s.misses;
                let hit_rate = if total_lookups > 0 {
                    format!("{:.0}%", 100.0 * s.hits as f64 / total_lookups as f64)
                } else {
                    "n/a".into()
                };
                eprintln!("─ read cache ─ {} entries, {} held; hits={} misses={} ({} hit-rate); invalidations={}",
                    entries,
                    format_bytes(bytes),
                    s.hits, s.misses, hit_rate, s.invalidations);
                continue;
            }
            ":cache clear" => {
                read_cache::clear();
                eprintln!("─ read cache cleared ─");
                continue;
            }
            cmd if cmd.starts_with(":dry-run") => {
                let arg = cmd.trim_start_matches(":dry-run").trim();
                let now_active = std::env::var("MLX_CODE_DRY_RUN")
                    .map(|v| v == "1")
                    .unwrap_or(false);
                let next_active = match arg {
                    "" => !now_active, // bare `:dry-run` toggles
                    "on" | "1" | "true" => true,
                    "off" | "0" | "false" => false,
                    _ => {
                        eprintln!("[hip] usage: :dry-run [on|off]");
                        continue;
                    }
                };
                if next_active {
                    std::env::set_var("MLX_CODE_DRY_RUN", "1");
                    eprintln!("\x1b[2m─ dry-run ─ ON ─ edit/bash will preview only\x1b[0m");
                } else {
                    std::env::remove_var("MLX_CODE_DRY_RUN");
                    eprintln!("\x1b[2m─ dry-run ─ off ─ writes/exec will land normally\x1b[0m");
                }
                continue;
            }
            cmd if cmd == ":peek" || cmd.starts_with(":peek ") => {
                // Parse optional "n" and/or "failed".
                let rest = cmd.trim_start_matches(":peek").trim();
                let mut args = serde_json::Map::new();
                for tok in rest.split_whitespace() {
                    if tok == "failed" {
                        args.insert("failed_only".into(), serde_json::Value::Bool(true));
                    } else if let Ok(n) = tok.parse::<u64>() {
                        args.insert("n".into(), serde_json::Value::from(n));
                    }
                }
                let t = tools::peek_log::tool();
                match (t.exec)(serde_json::Value::Object(args)).await {
                    Ok(out) => eprint!("{}", out),
                    Err(e) => eprintln!("[hip] peek_log failed: {}", e),
                }
                continue;
            }
            ":help" => {
                eprintln!("─ chat commands ─");
                eprintln!("  :reset             clear conversation history");
                eprintln!("  :quit / :exit /:q  exit");
                eprintln!("  :stats             show recent run stats");
                eprintln!("  :history           re-print conversation");
                eprintln!("  :cwd <path>        change working directory");
                eprintln!("  :show-thinking on|off");
                eprintln!("  :full-output on|off");
                eprintln!("  :smoke [path]      run smoke harness in cwd or path");
                eprintln!("  :tools             list registered agent tools");
                eprintln!("  :overhead          show current prompt overhead size");
                eprintln!("  :diff <a> <b>      run the diff tool inline on two files");
                eprintln!("  :peek [N] [failed] show last N runs.jsonl entries (default 10)");
                eprintln!("  :dry-run [on|off]  toggle agent-loop dry-run mode (no writes/exec)");
                eprintln!("  :cache             show read-cache stats");
                eprintln!("  :cache clear       drop all cached file contents");
                eprintln!("  :tps [N]           Unicode sparkline of last N runs' decode rate (default 20)");
                eprintln!("  :theme [dark|light|mono]  switch color theme at runtime");
                continue;
            }
            cmd if cmd.starts_with(':') => {
                eprintln!("[hip] unknown command: {} (try :help)", cmd);
                continue;
            }
            _ => {}
        }

        conv.push(ChatMessage::user(trimmed));

        let mut log = runlog::RunLog::new(
            if cli.one_shot {
                "chat-one-shot"
            } else {
                "chat-agent"
            },
            client.session_id(),
            client.model(),
            trimmed,
        );

        if cli.one_shot {
            let opts = sampler_opts(cli);
            let mut out = stdout();
            match client.stream(&conv, None, opts, &mut out).await {
                Ok(res) => {
                    out.write_all(b"\n").await.ok();
                    out.flush().await.ok();
                    conv.push(ChatMessage::assistant_text(res.content.clone()));
                    log.prompt_tokens = res.usage.as_ref().and_then(|u| u.prompt_tokens);
                    log.completion_tokens = res.usage.as_ref().and_then(|u| u.completion_tokens);
                    log.ttft_ms = res.ttft.map(|d| d.as_millis());
                    log.total_ms = Some(res.total.as_millis());
                    if let Some(c) = log.completion_tokens {
                        log.decode_tok_per_s = Some(c as f64 / res.total.as_secs_f64().max(0.001));
                    }
                    if cli.stats {
                        print_stats(&res, "chat-one-shot");
                    }
                }
                Err(e) => {
                    log.success = false;
                    log.error = Some(format!("{}", e));
                    eprintln!("[hip] error: {}", e);
                }
            }
        } else {
            match agent::run_loop(client, &mut conv, cli.max_rounds).await {
                Ok(stats) => {
                    log.rounds = Some(stats.rounds);
                    log.tool_calls = Some(stats.total_tool_calls);
                    log.prompt_tokens = stats.first_prompt_tokens;
                    log.completion_tokens = Some(stats.total_completion_tokens);
                    log.ttft_ms = stats.first_ttft.map(|d| d.as_millis());
                    log.total_ms = Some(stats.total.as_millis());
                    log.decode_tok_per_s = Some(
                        stats.total_completion_tokens as f64 / stats.total.as_secs_f64().max(0.001),
                    );
                    if cli.stats {
                        eprintln!(
                            "[hip] rounds={} ttft={:?} total={:?} tool_calls={} completion_tok={}",
                            stats.rounds,
                            stats.first_ttft,
                            stats.total,
                            stats.total_tool_calls,
                            stats.total_completion_tokens,
                        );
                    }
                }
                Err(e) => {
                    log.success = false;
                    log.error = Some(format!("{}", e));
                    eprintln!("[hip] error: {}", e);
                }
            }
        }
        if cli.log_runs {
            log.write();
        }
        if let Some(h) = &history_path {
            let _ = rl.save_history(h);
        }
    }
}

/// Update env vars that downstream code reads at construction time.
fn apply_pretty_env(show_thinking: bool, full_output: bool) {
    if show_thinking {
        std::env::set_var("MLX_CODE_SHOW_THINKING", "1");
    } else {
        std::env::set_var("MLX_CODE_SHOW_THINKING", "0");
    }
    if full_output {
        std::env::set_var("MLX_CODE_FULL_OUTPUT", "1");
    } else {
        std::env::set_var("MLX_CODE_FULL_OUTPUT", "0");
    }
}

/// Fallback for systems where rustyline can't init (no PTY etc.). Same loop
/// shape but with raw stdin readline. Kept simple intentionally.
async fn run_chat_fallback(
    cli: &Cli,
    client: &MtplxClient,
    first: Option<String>,
    mut conv: Vec<ChatMessage>,
    _show_thinking: bool,
    _full_output: bool,
) -> Result<()> {
    use std::io::{BufRead, Write};
    let mut pending = first;
    loop {
        let user_msg = if let Some(p) = pending.take() {
            p
        } else {
            eprint!("\n> ");
            let _ = std::io::stderr().flush();
            let mut line = String::new();
            let n = std::io::stdin().lock().read_line(&mut line).unwrap_or(0);
            if n == 0 {
                eprintln!("\n[hip] bye");
                return Ok(());
            }
            line.trim_end_matches(['\n', '\r']).to_string()
        };
        let trimmed = user_msg.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == ":quit" || trimmed == ":exit" || trimmed == ":q" {
            eprintln!("[hip] bye");
            return Ok(());
        }
        conv.push(ChatMessage::user(trimmed));
        if cli.one_shot {
            let opts = SamplingOpts::default();
            let mut out = stdout();
            let res = client.stream(&conv, None, opts, &mut out).await?;
            out.write_all(b"\n").await.ok();
            conv.push(ChatMessage::assistant_text(res.content.clone()));
        } else {
            let _ = agent::run_loop(client, &mut conv, cli.max_rounds).await?;
        }
    }
}

async fn run_one_shot(cli: &Cli, client: &MtplxClient, prompt: &str) -> Result<()> {
    let messages = if let Some(sys) = &cli.system {
        vec![ChatMessage::system(sys.clone()), ChatMessage::user(prompt)]
    } else {
        vec![ChatMessage::user(prompt)]
    };

    let mut log = runlog::RunLog::new("one-shot", client.session_id(), client.model(), prompt);
    let pre_snap = if cli.diff { Some(snap_cwd()) } else { None };

    let opts = sampler_opts(cli);
    let mut out = stdout();
    let res = match client.stream(&messages, None, opts, &mut out).await {
        Ok(r) => r,
        Err(e) => {
            log.success = false;
            log.error = Some(format!("{}", e));
            if cli.log_runs {
                log.write();
            }
            return Err(e);
        }
    };
    out.write_all(b"\n").await.ok();
    out.flush().await.ok();
    log.prompt_tokens = res.usage.as_ref().and_then(|u| u.prompt_tokens);
    log.completion_tokens = res.usage.as_ref().and_then(|u| u.completion_tokens);
    log.ttft_ms = res.ttft.map(|d| d.as_millis());
    log.total_ms = Some(res.total.as_millis());
    // Overall throughput across the request. (Per-decode rate via TTFT can
    // be misleading when the model emits a long <think> block, since the
    // first token arrives late and remaining time is near-zero.)
    if let Some(c) = log.completion_tokens {
        let total_s = res.total.as_secs_f64().max(0.001);
        log.decode_tok_per_s = Some(c as f64 / total_s);
    }

    if let Some(path) = &cli.save_html {
        let html = extract_html(&res.content);
        save_file(path, &html)?;
        eprintln!("[hip] saved html to {}", path.display());
        if cli.open {
            open_in_browser(path)?;
        }
    } else if let Some(path) = &cli.save {
        save_file(path, &res.content)?;
        eprintln!("[hip] saved to {}", path.display());
        if cli.open {
            open_in_browser(path)?;
        }
    }

    if cli.stats {
        print_stats(&res, "one-shot");
    }
    if cli.log_runs {
        log.write();
    }
    if cli.auto_smoke {
        let _ = run_smoke_inplace(".");
    }
    if let Some(pre) = pre_snap {
        let post = snap_cwd();
        print_diff(&pre, &post);
    }
    Ok(())
}

async fn run_agent(cli: &Cli, client: &MtplxClient, prompt: &str) -> Result<()> {
    let system = cli
        .system
        .clone()
        .unwrap_or_else(|| agent::DEFAULT_SYSTEM_PROMPT.to_string());
    let mut conv = vec![ChatMessage::system(system), ChatMessage::user(prompt)];
    let mut log = runlog::RunLog::new("agent", client.session_id(), client.model(), prompt);
    let pre_snap = if cli.diff { Some(snap_cwd()) } else { None };
    let stats = match agent::run_loop(client, &mut conv, cli.max_rounds).await {
        Ok(s) => s,
        Err(e) => {
            log.success = false;
            log.error = Some(format!("{}", e));
            if cli.log_runs {
                log.write();
            }
            return Err(e);
        }
    };
    log.rounds = Some(stats.rounds);
    log.tool_calls = Some(stats.total_tool_calls);
    log.prompt_tokens = stats.first_prompt_tokens;
    log.completion_tokens = Some(stats.total_completion_tokens);
    log.ttft_ms = stats.first_ttft.map(|d| d.as_millis());
    log.total_ms = Some(stats.total.as_millis());
    // Overall throughput: completion tokens across the whole agent loop.
    let _ = stats.first_ttft; // (kept for log; not used for rate to avoid bias)
    {
        let total_s = stats.total.as_secs_f64().max(0.001);
        log.decode_tok_per_s = Some(stats.total_completion_tokens as f64 / total_s);
    }
    if cli.stats {
        eprintln!(
            "[hip] rounds={} ttft={:?} total={:?} tool_calls={} first_prompt_tok={} completion_tok={}",
            stats.rounds,
            stats.first_ttft,
            stats.total,
            stats.total_tool_calls,
            stats.first_prompt_tokens.map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
            stats.total_completion_tokens,
        );
    }
    if cli.log_runs {
        log.write();
    }
    if cli.auto_smoke {
        let _ = run_smoke_inplace(".");
    }
    if let Some(pre) = pre_snap {
        let post = snap_cwd();
        print_diff(&pre, &post);
    }
    Ok(())
}

/// Snapshot of file → (size, mtime, sha-of-first-2KB) for the diff helper.
/// Snapshot of one file. `content` is `Some(...)` for text files within the
/// per-file size cap, so `print_diff` can compute a real line-level diff.
/// For binaries / oversize files we keep size+mtime+head only and fall back
/// to a coarse "size delta" line.
#[derive(Clone, PartialEq, Eq)]
struct FileSnap {
    size: u64,
    mtime: u64,
    head: [u8; 8],
    content: Option<String>,
}

type FileSnaps = std::collections::HashMap<std::path::PathBuf, FileSnap>;

const DIFF_CONTENT_CAP: u64 = 256 * 1024; // read full content up to 256KB

fn diff_extensions() -> &'static [&'static str] {
    &[
        "py", "js", "mjs", "ts", "tsx", "rs", "html", "css", "json", "md", "txt", "go", "java",
    ]
}

fn snap_cwd() -> FileSnaps {
    use std::io::Read;
    let mut out = FileSnaps::new();
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(_) => return out,
    };
    if let Ok(walker) = walkdir::WalkDir::new(&cwd)
        .max_depth(4)
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()
    {
        for entry in walker {
            let p = entry.path();
            if !entry.file_type().is_file() {
                continue;
            }
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !diff_extensions().contains(&ext) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let mut head = [0u8; 8];
            let mut content: Option<String> = None;
            if let Ok(mut f) = std::fs::File::open(p) {
                let _ = f.read(&mut head);
                if meta.len() <= DIFF_CONTENT_CAP {
                    if let Ok(s) = std::fs::read_to_string(p) {
                        content = Some(s);
                    }
                }
            }
            out.insert(
                p.to_path_buf(),
                FileSnap {
                    size: meta.len(),
                    mtime,
                    head,
                    content,
                },
            );
        }
    }
    out
}

/// Compute the prefix/suffix-trimmed line diff between two file contents.
/// Returns `(removed_lines, added_lines, common_prefix_line_count)` so the
/// caller can render with line numbers anchored at the change. Lines that
/// match line-for-line at the head and tail are stripped.
fn line_diff(before: &str, after: &str) -> (Vec<String>, Vec<String>, usize) {
    let b: Vec<&str> = before.lines().collect();
    let a: Vec<&str> = after.lines().collect();
    let mut prefix = 0usize;
    while prefix < b.len() && prefix < a.len() && b[prefix] == a[prefix] {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < b.len().saturating_sub(prefix)
        && suffix < a.len().saturating_sub(prefix)
        && b[b.len() - 1 - suffix] == a[a.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let removed: Vec<String> = b[prefix..b.len() - suffix]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let added: Vec<String> = a[prefix..a.len() - suffix]
        .iter()
        .map(|s| s.to_string())
        .collect();
    (removed, added, prefix)
}

fn print_diff(before: &FileSnaps, after: &FileSnaps) {
    let mut added_paths: Vec<&std::path::PathBuf> =
        after.keys().filter(|k| !before.contains_key(*k)).collect();
    let mut removed_paths: Vec<&std::path::PathBuf> =
        before.keys().filter(|k| !after.contains_key(*k)).collect();
    let mut modified_paths: Vec<&std::path::PathBuf> = before
        .iter()
        .filter_map(|(k, v)| after.get(k).filter(|nv| *nv != v).map(|_| k))
        .collect();
    added_paths.sort();
    removed_paths.sort();
    modified_paths.sort();
    if added_paths.is_empty() && removed_paths.is_empty() && modified_paths.is_empty() {
        eprintln!("\n\x1b[2m─ diff ─ no file changes\x1b[0m");
        return;
    }
    eprintln!(
        "\n\x1b[2m─ diff ─ {} added, {} modified, {} removed\x1b[0m",
        added_paths.len(),
        modified_paths.len(),
        removed_paths.len()
    );
    let cwd = std::env::current_dir().unwrap_or_default();
    let rel =
        |p: &std::path::Path| -> String { p.strip_prefix(&cwd).unwrap_or(p).display().to_string() };

    // Cap: at most this many files get expanded line-diffs; the rest get the
    // one-line size-delta summary so the output stays readable.
    const EXPAND_FILE_CAP: usize = 5;
    // Cap: at most this many lines printed per file (split across +/-).
    const LINES_PER_FILE: usize = 20;

    for p in &added_paths {
        let snap = after.get(*p);
        let sz = snap.map(|v| v.size).unwrap_or(0);
        eprintln!("  \x1b[0;32mA\x1b[0m  {:>7} b  {}", sz, rel(p));
    }
    for (idx, p) in modified_paths.iter().enumerate() {
        let bsnap = before.get(*p);
        let asnap = after.get(*p);
        let bsz = bsnap.map(|v| v.size).unwrap_or(0);
        let asz = asnap.map(|v| v.size).unwrap_or(0);
        let delta = asz as i64 - bsz as i64;
        let delta_disp = if delta >= 0 {
            format!("+{}", delta)
        } else {
            delta.to_string()
        };

        // Decide whether to show line-level diff for this file.
        let want_expand = idx < EXPAND_FILE_CAP;
        let both_have_content = bsnap.and_then(|s| s.content.as_ref()).is_some()
            && asnap.and_then(|s| s.content.as_ref()).is_some();

        if want_expand && both_have_content {
            let bc = bsnap.unwrap().content.as_ref().unwrap();
            let ac = asnap.unwrap().content.as_ref().unwrap();
            let (removed, added, anchor) = line_diff(bc, ac);
            let net_delta_lines: i64 = added.len() as i64 - removed.len() as i64;
            let nd_disp = if net_delta_lines >= 0 {
                format!("+{}", net_delta_lines)
            } else {
                net_delta_lines.to_string()
            };
            eprintln!(
                "  \x1b[0;33mM\x1b[0m  {:>7} b  {:>+5} b  net {} lines  @L{}  {}",
                asz,
                delta_disp,
                nd_disp,
                anchor + 1,
                rel(p)
            );

            let total_changes = removed.len() + added.len();
            let shown_cap = LINES_PER_FILE.min(total_changes);
            if total_changes == 0 {
                continue; // identical content; only metadata changed (mtime/head)
            }
            // Allocate the cap proportionally so a 200-line removal doesn't
            // starve the additions. `total_changes > 0` here - the
            // `continue` above already handled the zero case.
            let rem_share = (shown_cap * removed.len() + total_changes / 2) / total_changes;
            let add_share = shown_cap.saturating_sub(rem_share);

            for (i, line) in removed.iter().take(rem_share).enumerate() {
                let ln = anchor + 1 + i;
                eprintln!(
                    "    \x1b[0;31m- {:>4}\x1b[0m  {}",
                    ln,
                    truncate_line(line, 110)
                );
            }
            if removed.len() > rem_share {
                eprintln!(
                    "    \x1b[2m       ... +{} more removed\x1b[0m",
                    removed.len() - rem_share
                );
            }
            for (i, line) in added.iter().take(add_share).enumerate() {
                let ln = anchor + 1 + i;
                eprintln!(
                    "    \x1b[0;32m+ {:>4}\x1b[0m  {}",
                    ln,
                    truncate_line(line, 110)
                );
            }
            if added.len() > add_share {
                eprintln!(
                    "    \x1b[2m       ... +{} more added\x1b[0m",
                    added.len() - add_share
                );
            }
        } else {
            let suffix = if idx >= EXPAND_FILE_CAP {
                "  (collapsed)"
            } else {
                "  (no content snapshot)"
            };
            eprintln!(
                "  \x1b[0;33mM\x1b[0m  {:>7} b  {:>+5} b  {}{}",
                asz,
                delta_disp,
                rel(p),
                suffix
            );
        }
    }
    for p in &removed_paths {
        eprintln!("  \x1b[0;31mD\x1b[0m            {}", rel(p));
    }
}

fn truncate_line(s: &str, max: usize) -> String {
    let trimmed = s.trim_end();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(max).collect();
    out.push_str("\x1b[2m…\x1b[0m");
    out
}

/// In-place smoke runner — prints PASS/FAIL summary to stderr but does NOT
/// exit the process (unlike the dedicated --smoke subcommand). Used as the
/// post-run verifier when `--auto-smoke` is set.
async fn run_watch_loop(
    cli: &Cli,
    client: &MtplxClient,
    prompt: &str,
    path: &std::path::Path,
) -> Result<()> {
    let expanded = PathBuf::from(shellexpand::tilde(&path.to_string_lossy()).into_owned());
    // Build the optional glob matcher up front; bad patterns surface immediately.
    let matcher = match cli.watch_pattern.as_deref() {
        None => None,
        Some(pat) => match globset::Glob::new(pat) {
            Ok(g) => Some(g.compile_matcher()),
            Err(e) => {
                eprintln!("[hip] --watch-pattern '{}' invalid: {}", pat, e);
                return Ok(());
            }
        },
    };
    let pattern_note = match &cli.watch_pattern {
        Some(p) => format!(" pattern={}", p),
        None => String::new(),
    };
    eprintln!(
        "\x1b[2m─ watch ─ {}{} ─ Ctrl-C to exit\x1b[0m",
        expanded.display(),
        pattern_note
    );
    let mut last_state = scan_mtimes(&expanded, matcher.as_ref());
    // Run once immediately.
    let _ = run_once(cli, client, prompt).await;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let current = scan_mtimes(&expanded, matcher.as_ref());
        if current != last_state {
            eprintln!("\n\x1b[2m─ watch ─ change detected ─ rerunning\x1b[0m");
            last_state = current;
            let _ = run_once(cli, client, prompt).await;
        }
    }
}

fn scan_mtimes(root: &std::path::Path, matcher: Option<&globset::GlobMatcher>) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    if !root.exists() {
        return 0;
    }
    if let Ok(walker) = walkdir::WalkDir::new(root)
        .max_depth(4)
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()
    {
        for entry in walker {
            if !entry.file_type().is_file() {
                continue;
            }
            // If a pattern is set, check the path relative to root against it.
            // Match both the relative form ("src/foo.rs") and the bare file
            // name ("foo.rs") so users can pass either "*.rs" or "src/**/*.rs".
            if let Some(m) = matcher {
                let rel = entry.path().strip_prefix(root).unwrap_or(entry.path());
                let name = rel.file_name().map(std::path::Path::new).unwrap_or(rel);
                if !m.is_match(rel) && !m.is_match(name) {
                    continue;
                }
            }
            if let Ok(meta) = entry.metadata() {
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                entry.path().to_string_lossy().hash(&mut h);
                mtime.hash(&mut h);
                meta.len().hash(&mut h);
            }
        }
    }
    h.finish()
}

async fn run_once(cli: &Cli, client: &MtplxClient, prompt: &str) -> Result<()> {
    if cli.one_shot {
        run_one_shot(cli, client, prompt).await
    } else {
        run_agent(cli, client, prompt).await
    }
}

fn sampler_opts(cli: &Cli) -> SamplingOpts {
    let mut o = SamplingOpts::default();
    if let Some(t) = cli.temperature {
        o.temperature = t;
    }
    if let Some(p) = cli.top_p {
        o.top_p = p;
    }
    if let Some(k) = cli.top_k {
        o.top_k = k;
    }
    if let Some(m) = cli.max_tokens {
        o.max_tokens = m;
    }
    o
}

fn find_last_session_id() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let path = format!("{}/.mlx-code/logs/runs.jsonl", home);
    let content = std::fs::read_to_string(&path).ok()?;
    let last = content.lines().rev().find(|l| !l.trim().is_empty())?;
    let v: serde_json::Value = serde_json::from_str(last).ok()?;
    v.get("session_id")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
}

fn run_smoke_inplace(path: &str) -> Result<()> {
    let script = "/Users/dan/code-2/mlx-code/tools/smoke/run_all.sh";
    if !std::path::Path::new(script).exists() {
        return Ok(());
    }
    let expanded = shellexpand::tilde(path).into_owned();
    eprintln!("\n\x1b[2m─ auto-smoke ─ {}\x1b[0m", expanded);
    let _ = std::process::Command::new("bash")
        .arg(script)
        .arg(&expanded)
        .status();
    Ok(())
}

fn extract_html(text: &str) -> String {
    let lower = text.to_lowercase();
    let start = lower.find("<!doctype html").or_else(|| lower.find("<html"));
    let end = lower.rfind("</html>");
    let extracted = match (start, end) {
        (Some(s), Some(e)) => text[s..e + "</html>".len()].to_string(),
        _ => text.to_string(),
    };
    // The model occasionally emits a stray bare `<html>` before the real
    // `<html ...>` opener, or a duplicated `</html>` at the end. Trim those.
    dedupe_html_shell(&extracted)
}

fn dedupe_html_shell(s: &str) -> String {
    let mut s = s.to_string();
    // Drop a leading bare `<html>` line if there's another `<html` after it.
    let lower = s.to_lowercase();
    if let Some(after) = lower.strip_prefix("<html>") {
        if let Some(pos) = after.find("<html") {
            // skip over the leading "<html>" + any whitespace, keep the second one
            let cut = 6 + pos;
            s = s[cut..].to_string();
        }
    }
    // Drop a duplicated trailing `</html>`.
    let lower2 = s.to_lowercase();
    let pat = "</html>";
    if let Some(last) = lower2.rfind(pat) {
        let before = &lower2[..last];
        if let Some(prev) = before.rfind(pat) {
            // only collapse if the gap is just whitespace
            let gap = &s[prev + pat.len()..last];
            if gap.trim().is_empty() {
                s = format!("{}{}", &s[..prev + pat.len()], &s[last + pat.len()..]);
            }
        }
    }
    s
}

fn save_file(path: &std::path::Path, content: &str) -> Result<()> {
    let path = expand(path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("mkdir -p {}", parent.display()))?;
    }
    std::fs::write(&path, content).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn expand(path: &std::path::Path) -> Result<PathBuf> {
    let s = path.to_string_lossy();
    let expanded = shellexpand::tilde(&s);
    Ok(PathBuf::from(expanded.into_owned()))
}

fn open_in_browser(path: &std::path::Path) -> Result<()> {
    let path = expand(path)?;
    let _ = std::process::Command::new("open").arg(&path).status();
    Ok(())
}

fn run_smoke(paths: &str) -> Result<()> {
    let script = "/Users/dan/code-2/mlx-code/tools/smoke/run_all.sh";
    if !std::path::Path::new(script).exists() {
        eprintln!("[hip] smoke: missing {}", script);
        std::process::exit(2);
    }
    let mut cmd = std::process::Command::new("bash");
    cmd.arg(script);
    // Comma-separated path list: pass each as a separate arg to the script.
    for p in paths.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let expanded = shellexpand::tilde(p).into_owned();
        cmd.arg(&expanded);
    }
    let status = cmd.status()?;
    std::process::exit(status.code().unwrap_or(1));
}

fn print_run_summary(n: usize) -> Result<()> {
    let Ok(home) = std::env::var("HOME") else {
        eprintln!("[hip] HOME unset");
        return Ok(());
    };
    let path = format!("{}/.mlx-code/logs/runs.jsonl", home);
    let file = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("[hip] no runs yet at {}", path);
            return Ok(());
        }
    };
    let lines: Vec<&str> = file.lines().filter(|l| !l.trim().is_empty()).collect();
    let take = lines.len().saturating_sub(n.min(lines.len()));
    eprintln!("\x1b[1mLast {} runs ({}):\x1b[0m", lines.len() - take, path);
    eprintln!(
        "\x1b[2m  {:>16}  {:>14}  {:>5}  {:>6}  {:>5}  {:>6}  {:>5}  prompt\x1b[0m",
        "ts", "mode", "round", "ttft", "tot", "compl", "tok/s"
    );
    let mut total_ttft = 0.0f64;
    let mut total_decode = 0.0f64;
    let mut count = 0u32;
    for line in &lines[take..] {
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let ts = v.get("ts_unix").and_then(|x| x.as_u64()).unwrap_or(0);
        let mode = v.get("mode").and_then(|x| x.as_str()).unwrap_or("?");
        let rounds = v
            .get("rounds")
            .and_then(|x| x.as_u64())
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".into());
        let ttft = v
            .get("ttft_ms")
            .and_then(|x| x.as_u64())
            .map(|n| n as f64 / 1000.0);
        let total = v
            .get("total_ms")
            .and_then(|x| x.as_u64())
            .map(|n| n as f64 / 1000.0);
        let compl = v
            .get("completion_tokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        let tps_raw = v
            .get("decode_tok_per_s")
            .and_then(|x| x.as_f64())
            .unwrap_or(0.0);
        // Outlier filter: cap implausible decode rates (>1000 t/s likely from
        // an early run with broken total/ttft math). Display as N/A; exclude
        // from the avg.
        let tps_plausible = tps_raw.is_finite() && tps_raw > 0.0 && tps_raw < 200.0;
        let tps_disp: String = if tps_plausible {
            format!("{:>5.1}", tps_raw)
        } else {
            "  N/A".into()
        };
        let prompt: String = v
            .get("prompt_first_120_chars")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .chars()
            .take(50)
            .collect();
        let ts_str = format_ts(ts);
        eprintln!(
            "  {:>16}  {:>14}  {:>5}  {:>5.1}s  {:>4.1}s  {:>6}  {}  {}",
            ts_str,
            mode,
            rounds,
            ttft.unwrap_or(0.0),
            total.unwrap_or(0.0),
            compl,
            tps_disp,
            prompt
        );
        if let Some(t) = ttft {
            total_ttft += t;
        }
        if tps_plausible {
            total_decode += tps_raw;
            count += 1;
        }
    }
    if count > 0 {
        eprintln!(
            "\x1b[2m  ─ avg over {} run(s): ttft={:.1}s  tok/s={:.1}\x1b[0m",
            count,
            total_ttft / count as f64,
            total_decode / count as f64
        );
    }
    Ok(())
}

fn format_ts(ts: u64) -> String {
    use std::time::{Duration, UNIX_EPOCH};
    let t = UNIX_EPOCH + Duration::from_secs(ts);
    // Crude local time format: HH:MM:SS based on system_time
    let now = std::time::SystemTime::now();
    let dur = now.duration_since(t).unwrap_or(Duration::ZERO);
    let secs = dur.as_secs();
    if secs < 60 {
        format!("{}s ago", secs)
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

/// Print the system prompt + tool spec sizes without sending anything.
/// Approximate-token estimate uses 4 chars/token (a common heuristic for
/// English-heavy text). The MTPLX server returns exact prompt_tokens for
/// any actual request; this is a fast offline check.
fn print_inspect_prompt(cli: &Cli) {
    let system = cli
        .system
        .clone()
        .unwrap_or_else(|| agent::DEFAULT_SYSTEM_PROMPT.to_string());
    let registry = tools::registry();
    let specs = tools::tool_specs(&registry);
    let tools_json = serde_json::to_string(&specs).unwrap_or_default();
    let tools_pretty = serde_json::to_string_pretty(&specs).unwrap_or_default();

    let sys_chars = system.chars().count();
    let sys_lines = system.lines().count();
    let tools_chars = tools_json.chars().count();
    let tools_pretty_lines = tools_pretty.lines().count();
    let total_chars = sys_chars + tools_chars;

    let approx = |chars: usize| (chars as f64 / 4.0).round() as usize;

    println!("─ mlx-code prompt inspection ─");
    println!(
        "  system prompt:       {:>6} chars   {:>4} lines   ~{} tokens",
        sys_chars,
        sys_lines,
        approx(sys_chars)
    );
    println!(
        "  tool specs (compact):{:>6} chars   {:>4} tools   ~{} tokens",
        tools_chars,
        registry.len(),
        approx(tools_chars)
    );
    println!("  ----------------------------------------------------------");
    println!(
        "  TOTAL fixed overhead:{:>6} chars                ~{} tokens",
        total_chars,
        approx(total_chars)
    );
    println!();
    println!("  tools registered ({}):", registry.len());
    for t in &registry {
        let schema_str = serde_json::to_string(&t.schema).unwrap_or_default();
        println!(
            "    - {:<10} {:>5} chars  ~{} tokens",
            t.name,
            schema_str.chars().count(),
            approx(schema_str.chars().count())
        );
    }
    println!();
    println!(
        "  (pretty-printed tool specs would be {} lines)",
        tools_pretty_lines
    );
    println!();
    println!("  reference: opencode parent-agent prompt is ~13K tokens");
}

/// Print the chat-mode startup banner. First the IRIS-CODE ASCII logo (when
/// stderr is a TTY and not opted-out), then a two-line metadata bar with
/// model / session / cwd / hint. Uses the active theme.
fn print_chat_banner(client: &MtplxClient, cli: &Cli) {
    logo::print();
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "?".into());
    let d = theme::dim();
    let a = theme::accent();
    let r = theme::RESET;
    let dry = if cli.dry_run {
        format!(" {}[DRY-RUN]{}", theme::warn(), theme::RESET)
    } else {
        String::new()
    };
    eprintln!(
        "{d}╭─ {a}hippo-code{d} ─ {a}{model}{d} ─ session {a}{sess}{d}{dry}{r}",
        d = d,
        a = a,
        r = r,
        model = client.model(),
        sess = client.session_id(),
        dry = dry,
    );
    eprintln!("{d}╰─ cwd {a}{cwd}{d} ─ type {a}:help{d} for commands · Alt-Enter for newline · {a}:quit{d} to exit{r}",
        d = d, a = a, r = r, cwd = cwd,
    );
}

/// Drain the dry-run accumulator and print a per-kind grouped summary.
/// Called once at end of run when `--dry-run` is active. Silent when nothing
/// would have changed.
fn print_dry_run_summary() {
    let entries = dry_run_log::drain();
    if entries.is_empty() {
        eprintln!("\n\x1b[2m─ dry-run summary ─ no mutations would have happened\x1b[0m");
        return;
    }
    let mut create = Vec::new();
    let mut overwrite = Vec::new();
    let mut replace = Vec::new();
    let mut bash_cmds = Vec::new();
    let mut total_bytes: u64 = 0;
    for e in entries {
        total_bytes += e.bytes;
        match e.kind {
            "create" => create.push((e.target, e.bytes)),
            "overwrite" => overwrite.push((e.target, e.bytes)),
            "replace" => replace.push((e.target, e.bytes)),
            "bash" => bash_cmds.push(e.target),
            _ => {}
        }
    }
    let mut_count = create.len() + overwrite.len() + replace.len() + bash_cmds.len();
    eprintln!(
        "\n\x1b[2m─ dry-run summary ─ {} mutation(s); {} would touch ~{}\x1b[0m",
        mut_count,
        create.len() + overwrite.len() + replace.len(),
        format_bytes(total_bytes)
    );
    for (p, b) in &create {
        eprintln!(
            "  \x1b[0;32mCREATE\x1b[0m    {:>9}  {}",
            format_bytes(*b),
            p
        );
    }
    for (p, b) in &overwrite {
        eprintln!(
            "  \x1b[0;33mOVERWRITE\x1b[0m {:>9}  {}",
            format_bytes(*b),
            p
        );
    }
    for (p, b) in &replace {
        eprintln!(
            "  \x1b[0;33mREPLACE\x1b[0m   {:>9}  {}",
            format_bytes(*b),
            p
        );
    }
    for c in &bash_cmds {
        let preview: String = c.chars().take(80).collect();
        eprintln!("  \x1b[0;36mBASH\x1b[0m                {}", preview);
    }
}

fn format_bytes(n: u64) -> String {
    if n < 1024 {
        return format!("{}B", n);
    }
    let kb = (n as f64) / 1024.0;
    if kb < 1024.0 {
        return format!("{:.1}KB", kb);
    }
    let mb = kb / 1024.0;
    format!("{:.2}MB", mb)
}

/// Print the FULL prompt body (system message + tools array + optional user
/// message if one was passed positionally). This is exactly the messages /
/// tools structure that would be POSTed to the chat-completions endpoint.
/// Output is JSON for easy diffing and grepping.
fn print_show_prompt(cli: &Cli) {
    let system = cli
        .system
        .clone()
        .unwrap_or_else(|| agent::DEFAULT_SYSTEM_PROMPT.to_string());
    let registry = tools::registry();
    let specs = tools::tool_specs(&registry);

    let mut messages: Vec<serde_json::Value> = Vec::new();
    messages.push(serde_json::json!({"role": "system", "content": system}));
    if !cli.prompt.is_empty() {
        let user = cli.prompt.join(" ");
        messages.push(serde_json::json!({"role": "user", "content": user}));
    }

    let body = serde_json::json!({
        "model": cli.model,
        "stream": true,
        "messages": messages,
        "tools": specs,
    });
    let pretty =
        serde_json::to_string_pretty(&body).unwrap_or_else(|_| "<serialization failed>".into());
    println!("{}", pretty);
}

fn print_stats(res: &client::StreamResult, label: &str) {
    let ttft = res.ttft.unwrap_or(Duration::ZERO);
    let total = res.total;
    let usage = res.usage.as_ref();
    eprintln!(
        "[mlx-code/{}] ttft={}ms total={}ms prompt_tok={} completion_tok={} finish={}",
        label,
        ttft.as_millis(),
        total.as_millis(),
        usage
            .and_then(|u| u.prompt_tokens)
            .map(|n| n.to_string())
            .unwrap_or_else(|| "?".into()),
        usage
            .and_then(|u| u.completion_tokens)
            .map(|n| n.to_string())
            .unwrap_or_else(|| "?".into()),
        res.finish_reason.as_deref().unwrap_or("?"),
    );
}

#[cfg(test)]
mod tests {
    use super::{line_diff, scan_mtimes};
    use std::path::PathBuf;

    #[test]
    fn line_diff_pure_addition_at_end() {
        let (rem, add, anchor) = line_diff("a\nb\n", "a\nb\nc\n");
        assert!(rem.is_empty());
        assert_eq!(add, vec!["c"]);
        assert_eq!(anchor, 2);
    }

    #[test]
    fn line_diff_pure_deletion_in_middle() {
        let (rem, add, anchor) = line_diff("a\nb\nc\n", "a\nc\n");
        assert_eq!(rem, vec!["b"]);
        assert!(add.is_empty());
        assert_eq!(anchor, 1);
    }

    #[test]
    fn line_diff_replacement_in_middle() {
        let (rem, add, anchor) = line_diff("a\nb\nc\n", "a\nB\nc\n");
        assert_eq!(rem, vec!["b"]);
        assert_eq!(add, vec!["B"]);
        assert_eq!(anchor, 1);
    }

    #[test]
    fn line_diff_identical() {
        let (rem, add, _anchor) = line_diff("a\nb\nc\n", "a\nb\nc\n");
        assert!(rem.is_empty());
        assert!(add.is_empty());
    }

    #[test]
    fn line_diff_full_rewrite() {
        let (rem, add, anchor) = line_diff("x\ny\n", "a\nb\n");
        assert_eq!(rem, vec!["x", "y"]);
        assert_eq!(add, vec!["a", "b"]);
        assert_eq!(anchor, 0);
    }

    fn mk_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mlx-watch-test-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn scan_mtimes_no_pattern_hashes_all_files() {
        let dir = mk_dir("nopat");
        std::fs::write(dir.join("a.rs"), "x").unwrap();
        std::fs::write(dir.join("b.txt"), "y").unwrap();
        let h1 = scan_mtimes(&dir, None);
        // Touch the .txt file - hash should change because no pattern means all files counted.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(dir.join("b.txt"), "y2").unwrap();
        let h2 = scan_mtimes(&dir, None);
        assert_ne!(h1, h2, "expected hash to change without pattern");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn show_prompt_body_is_valid_json_with_expected_structure() {
        // Re-derive the same body shape print_show_prompt builds and verify
        // that it round-trips through serde_json.
        let registry = crate::tools::registry();
        let specs = crate::tools::tool_specs(&registry);
        let body = serde_json::json!({
            "model": "test-model",
            "stream": true,
            "messages": [
                {"role": "system", "content": crate::agent::DEFAULT_SYSTEM_PROMPT},
                {"role": "user", "content": "hello"}
            ],
            "tools": specs,
        });
        let serialized = serde_json::to_string(&body).unwrap();
        assert!(!serialized.is_empty());
        // Re-parse, then assert the structure.
        let parsed: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed["model"].as_str().unwrap(), "test-model");
        assert_eq!(parsed["stream"].as_bool().unwrap(), true);
        let msgs = parsed["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"].as_str().unwrap(), "system");
        assert_eq!(msgs[1]["role"].as_str().unwrap(), "user");
        let tools = parsed["tools"].as_array().unwrap();
        // Lower bound only: we expect at least the core tools, but new ones may
        // be added in later iters without immediately bumping this number.
        assert!(tools.len() >= 9, "expected >=9 tools, got {}", tools.len());
    }

    #[test]
    fn inspect_prompt_produces_nonzero_overhead_and_lists_all_tools() {
        // Don't actually call print_inspect_prompt (it prints to stdout).
        // Re-derive the same numbers and assert invariants.
        let registry = crate::tools::registry();
        let specs = crate::tools::tool_specs(&registry);
        let tools_json = serde_json::to_string(&specs).unwrap();
        assert!(tools_json.len() > 100, "tool specs JSON suspiciously small");
        assert!(
            registry.len() >= 7,
            "expected at least 7 registered tools, got {}",
            registry.len()
        );
        let names: std::collections::HashSet<&str> = registry.iter().map(|t| t.name).collect();
        for required in &[
            "read", "grep", "edit", "bash", "list", "glob", "search", "diff", "tree",
        ] {
            assert!(
                names.contains(required),
                "missing required tool: {}",
                required
            );
        }
        // System prompt is non-empty.
        assert!(!crate::agent::DEFAULT_SYSTEM_PROMPT.is_empty());
    }

    #[test]
    fn scan_mtimes_with_pattern_ignores_unrelated_changes() {
        let dir = mk_dir("withpat");
        std::fs::write(dir.join("a.rs"), "x").unwrap();
        std::fs::write(dir.join("b.txt"), "y").unwrap();
        let m = globset::Glob::new("*.rs").unwrap().compile_matcher();
        let h1 = scan_mtimes(&dir, Some(&m));
        // Touch the .txt - hash should NOT change because pattern excludes it.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(dir.join("b.txt"), "y2").unwrap();
        let h2 = scan_mtimes(&dir, Some(&m));
        assert_eq!(h1, h2, "non-matching change should not affect hash");
        // Touch the .rs - hash SHOULD change.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(dir.join("a.rs"), "x2").unwrap();
        let h3 = scan_mtimes(&dir, Some(&m));
        assert_ne!(h1, h3, "matching change should affect hash");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
