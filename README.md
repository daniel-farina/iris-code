<p align="center">
  <img src="docs/hippo-logo.png" alt="hippo-code" width="420" />
</p>

# hip

Lean coding agent for the [MTPLX](https://github.com/daniel-farina/MTPLX) local model server.
Sleek Ink-based TUI inspired by claude-code; tool-call agent loop with parity to the [Rust hip](./_old_version_backup) it replaces.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/daniel-farina/hippo-code/main/install.sh | sh
```

Or from source:

```sh
git clone https://github.com/daniel-farina/hippo-code.git
cd hippo-code && bun install
./bin/hip --version
```

Requires [bun](https://bun.sh) on PATH, Apple Silicon, and an MTPLX server running locally (default `http://127.0.0.1:8088/v1`). MLX is Apple-Silicon-only, so the release tarball is mac-arm64 only.

Updating an existing install: `hip --update` downloads the latest release and atomically swaps the files (previous version backed up alongside as `*.prev`).

## Use

```sh
hip                                 # interactive TUI
hip --print "explain this repo"     # headless one-shot
echo "what is 7*11?" | hip --print  # stdin pipe
hip --continue-last                 # resume last session
hip --resume <session-id>           # resume a specific session
hip --update                        # download + install latest release
hip --help                          # all flags
```

In the TUI: `/help` lists slash commands. Up arrow walks prompt history. Ctrl-C cancels a stream; press twice to exit. Esc clears input. Type while busy to queue messages.

### Pinning a stable session id

MTPLX keeps a small (8-slot) LRU "session bank" of warm prefix caches. If a long-prefix session goes idle while shorter sessions land, the bank can evict it (POLICY_MISMATCH) and the next request from hip pays the full prefill cost again. To keep the slot warm across hip invocations, pin a stable id:

```sh
hip --session my-project                          # same id every run
HIP_SESSION=my-project hip                        # env var fallback
hip --clear-stale-sessions=30 --session my-project  # also evict slots idle >30min at startup
```

The `--session` flag wins over `HIP_SESSION` which wins over the auto-generated `auto-YYYYMMDD-...` id. Combine with `--clear-stale-sessions` (default 30min cutoff if no value is passed) to proactively free slots held by older sessions.

## Highlights

- **10 tools** with 1:1 parity to Rust hip: `read`, `search` (ripgrep), `grep`, `glob`, `edit`, `bash`, `list`, `tree`, `diff`, `peek_log`
- **Live streaming output** with throttled redraws (no flicker)
- **Input queue** while the model is busy — Up arrow edits queued messages
- **Cross-process session resume** via `--continue-last` / `--resume`, with measured 5× speedup from MTPLX prefix cache
- **Recovery from MTPLX 422 malformed-JSON errors** (qwen3 tool-call quirk)
- **Hippo-purple theme** + ANSI logo from the Rust source

## Build / test

```sh
bun run check    # typecheck + biome lint + vitest
bun test         # bun's runner (+ test-game playground)
bun run lint:fix # auto-fix style
```

## Layout

```
src/
  cli.ts             argv parsing, --print mode, --update dispatch
  agent.ts           tool-use agent loop (with malformed-JSON recovery)
  client.ts          MTPLX SSE streaming client + AbortSignal
  updater.ts         hip --update: GitHub releases download + atomic swap
  schema.ts          OpenAI-compatible chat message types
  system_prompt.ts   coding-assistant system prompt
  config.ts          defaults (URL, model, sampling)
  session_store.ts   JSONL persistence at ~/.hip/sessions.jsonl
  tools/             10 tool implementations
  ui/                Ink TUI + theme + Live-streaming logic
bin/hip              bun launcher
assets/hippo-logo.txt  ANSI logo printed on TUI launch
.github/workflows/   CI + release pipelines
```

## License

Same as the original Rust hip.
