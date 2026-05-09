# iris

A lean Rust CLI coding agent for a local MTPLX server (Qwen3.6-27B with native MTP).
Replicates the useful subset of opencode's agent loop without the heavy parent-agent
prompt overhead, so cold prefills are tiny and the model's prompt format is exactly
what it expects.

> **Status:** ~5K LoC across 24 modules · 10 tools · ~1.35K token prompt overhead
> (9.6× lighter than opencode) · 66 tests passing · 943 cumulative smoke passes / 0 failures
> · iris-flower theme · sticky bottom status bar · runs entirely against your local
> MTPLX server.

## Install

One-line installer (Linux x86_64, macOS x86_64, macOS arm64):

```
curl -sSL https://raw.githubusercontent.com/daniel-farina/iris-code/main/install.sh | sh
```

Override the install location:

```
IRIS_INSTALL_DIR=/usr/local/bin curl -sSL https://raw.githubusercontent.com/daniel-farina/iris-code/main/install.sh | sh
```

Or build from source:

```
git clone https://github.com/daniel-farina/iris-code
cd iris-code  # repo name unchanged
cargo build --release
./target/release/iris --help
```

On first run, iris probes your local MTPLX server, lists available
models, and prints any setup hints needed. Re-run the wizard with
`iris --setup`.

## Usage

```
# One-shot ("print mode") — stream a single response, no tools.
iris --one-shot --save-html ~/airplane.html "make a 3D airplane game; output just <html>...</html>."
iris --print "..."   # alias for --one-shot

# Agent loop (default) — full tools.
iris "in /tmp/foo, create hello.py and run it"

# Interactive chat — REPL with arrow-key history (rustyline).
iris            # empty prompt enters chat
iris --chat     # explicit
# REPL commands: :help :reset :quit :stats :cwd <path> :show-thinking on|off :full-output on|off
#                :history :smoke [path] :tools :overhead :diff <a> <b> :peek [N] [failed]
#                :dry-run [on|off] :cache [clear] :tps [N]

# Recent-runs summary table.
iris --summary --summary-n 10

# Verbose / quiet shortcuts.
iris -v "explain recursion"   # show thinking + full tool output + stats
iris -q "..." > out.txt       # suppress live bar / panels for piping

# Persistent session id keeps the prefix-cache slot warm across calls.
iris --session my-project "find where SamplingOpts defaults are defined"
```

## Live status bar (sticky bottom)

While streaming you'll see a status line pinned to the bottom row of the
terminal via ANSI scroll regions (DECSTBM). Streamed output flows in the
region above; the bar stays put even when you scroll the scrollback:

```
─[iris]─ 247 tok ─ 18 lines ─ 102.9 tok/s ─ ttft 0.6s ─ stream 2.4s ─ total 3.0s
```

After the run finishes the scroll region is released and a final summary
line is left in scrollback with the server-reported `completion_tokens`,
`prompt_tokens`, and overall throughput.

Falls back to a plain `\r`-pinned inline line if the terminal doesn't
support scroll regions or `MLX_CODE_NO_STICKY=1` is set. Panic hook +
SIGINT handler restore the scroll region cleanly on unexpected exit.

## Settings

Toggle at launch (CLI flags) or runtime (REPL `:cmds`) or via env vars:

| flag                | env                     | default | effect                                |
|---------------------|-------------------------|---------|----------------------------------------|
| `--hide-thinking`   | `MLX_CODE_SHOW_THINKING=0` | off (default: thinking SHOWN) | Suppress `<think>` blocks. Default is to display them in a dim panel. |
| `--full-output`     | `MLX_CODE_FULL_OUTPUT`  | off     | Don't truncate tool results           |
| `--log-runs`        | `MLX_CODE_LOG_RUNS`     | on      | Append JSONL row per run              |
| `--quiet / -q`      | `MLX_CODE_NO_PRETTY`+`MLX_CODE_NO_LIVE_TPS` | off | Suppress UI |
| `--verbose / -v`    | —                       | off     | show_thinking + full_output + stats   |
| `--session ID`      | `MLX_CODE_SESSION`      | mlx-code-default | Cache key                  |
| `--url URL`         | `MLX_CODE_URL`          | localhost:8088/v1 | Server                    |
| `--model NAME`      | `MLX_CODE_MODEL`        | mtplx-qwen36-27b-optimized-speed | Model id |
| `--continue / -c`   | —                       | off     | Resume the most recent session_id     |
| `--system-file PATH`| —                       | —       | Load system prompt from file          |
| `--temperature N`   | —                       | 0.6     | Sampler temperature                   |
| `--top-p N`         | —                       | 0.95    | Sampler top-p                         |
| `--top-k N`         | —                       | 20      | Sampler top-k                         |
| `--max-tokens N`    | —                       | 16384   | Max output tokens per turn            |
| `--diff`            | -                       | off     | Snapshot cwd before/after; show A/M/D files plus +/- line diff (5 files x 20 lines cap) |
| `--auto-smoke`      | `MLX_CODE_AUTO_SMOKE`   | off     | Auto-run smoke harness against cwd post-run |
| `--smoke [PATH,…]`  | —                       | —       | Run smoke harness only, exit code 0/1 |
| `--summary [-n N]`  | —                       | —       | Print last N runs from runs.jsonl     |
| `--watch PATH`      | -                       | -       | Re-run prompt on file change in PATH (1s mtime poll) |
| `--watch-pattern G` | -                       | -       | Glob filter for `--watch` (e.g. `*.rs`, `src/**/*.ts`); only matching files trigger reruns |
| `--inspect-prompt`  | -                       | -       | Print system+tool-spec sizes (chars/tokens, per-tool breakdown) and exit; no model call |
| `--show-prompt`     | -                       | -       | Dump the full prompt body as JSON (system+user+tools); no model call |
| `--dry-run`         | `MLX_CODE_DRY_RUN`      | off     | Agent-loop dry run: `edit` and `bash` return `(dry_run) would...` previews; no writes/exec |

## Smoke harness

The bundled `tools/smoke/run_all.sh` validates HTML/JS artifacts:
- HTML: balance check on `<html>`, balanced braces/parens, Three.js refs on game files
- JS/MJS: `node --check` syntax validation
- Append-only JSONL log at `~/.mlx-code/logs/smoke.jsonl`

Invoke via `iris --smoke` or pair with `--auto-smoke` for post-run verify.

## Test-suite stability

`tools/check-flake.sh [N]` runs `cargo test --release` N times (default 10) and reports per-run pass/fail with timing. Useful for catching parallel-test races on global state (env vars, static caches) before they bite. Exits 1 if any run failed.

```
$ tools/check-flake.sh 5
─ check-flake.sh ─ running cargo test --release 5 time(s) in /...
  run  1/5  PASS  (2285ms)  test result: ok. 46 passed; 0 failed
  run  2/5  PASS  (2304ms)  test result: ok. 46 passed; 0 failed
  ...
─ summary ─ 5/5 passed, 0 failed, slowest run 2304ms
```

## Performance targets

Measured on Apple Silicon M5 Max 128GB against local MTPLX serving Qwen3.6-27B:

| metric | typical |
|---|---|
| Cold one-shot TTFT (~250 input tokens) | 15-30s |
| Warm `--continue` TTFT | 1-3s (3-4× speedup) |
| Sustained decode rate | 35-50 tok/s |
| Agent-loop tool calls per refactor | 7-21 (depends on file count) |
| Prefill rate at 80K context | 565 tok/s |
| Prompt overhead (system + 10 tool specs) | ~1.35K tokens (vs opencode's ~13K) - run `iris --inspect-prompt` for live breakdown |

## Tested project shapes

The same lean agent loop has been validated end-to-end on:
- 7 single-HTML 3D games (Three.js)
- 2 modular ES-module game projects (5- and 7-file)
- 1 Python CLI (stdlib + pytest)
- 1 Rust CLI (Cargo + std)

Refactor patterns proven: feature-add, instrumentation, rename-symbol, single-file-edit. All artifacts pass the bundled smoke harness.

## Tools

Default registry (Phase 2 + 3):

- `read(path, offset?, limit?, around?, context=20)` — text-file slice; default first 200 lines; `around=N` returns symmetric `context` lines around line N (target marked with `>`); refuses >1MB
- `grep(pattern, path?, glob?)` — regex/literal search via the `ignore` crate (gitignore-aware), 50 hit cap
- `edit(path, old_string, new_string, replace_all=false, dry_run=false)` — exact-match replace; `old_string=""` creates/overwrites; `dry_run=true` previews the change as a `@L` diff without writing
- `bash(command, timeout_s?)` — runs in `zsh -lc`, 30s default timeout, output truncated at 16 KB
- `list(path)` — directory listing
- `glob(pattern, path?)` — file globbing, gitignore-aware, 200 path cap
- `search(query, max_results=10, path?, definitions_only=false, files_only=false)` — search with query expansion; `definitions_only=true` keeps only declaration lines; `files_only=true` returns ranked file paths only (no per-line hits, very compact)
- `diff(path_a, path_b, context=3)` — unified-diff between two text files; LCS-based; emits `@@ hunks @@` and `-/+` lines, useful for verifying edits or comparing related files
- `tree(path?, depth=2, show_hidden=false)` — recursive directory view, gitignore-aware; up to 500 entries; skips target/node_modules/dist/.next/.cache/build/.git
- `peek_log(n=10, failed_only=false)` — return last N entries from `~/.mlx-code/logs/runs.jsonl` with mode/success/timing/error; max 50

## Design notes

- **Streaming**: SSE parser in `src/client.rs`. Server emits `delta.tool_calls` deltas which we
  accumulate per `index` until the stream ends.
- **Agent loop** (`src/agent.rs`): up to 30 rounds. Each round sends the conversation + tool
  specs, streams the response, and either appends an assistant text and exits, or appends
  the assistant tool_calls + one `tool` role message per result and recurses.
- **System prompt**: 1 line (~30 tokens). Combined with all 7 tool specs the prompt is
  ~1.05K tokens of fixed overhead, vs opencode's ~13K parent-agent prompt.
- **Session header**: every request sends `x-mtplx-session-id: <session>` so MTPLX can keep
  a per-session prefix-cache slot warm.
- **No daemon, no TUI, no MCP, no auth.**

See `REPORT.md` for measurements vs opencode.
