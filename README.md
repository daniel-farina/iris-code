# hip

Lean coding agent for the [MTPLX](https://github.com/daniel-farina/MTPLX) local model server.
Sleek Ink-based TUI inspired by claude-code; tool-call agent loop with parity to the [Rust hip](./_old_version_backup) it replaces.

```
                           ████         ████▌    █        ▀
                           ████         ████▌    █ ▄▄   ▄▄▄    ▄▄▄▄   ▄▄▄▄    ▄▄▄
                            ▐██████████████     █▀  █    █    █▀ ▀█  █▀ ▀█  █▀ ▀█
                                                █   █    █    █   █  █   █  █   █
                                                █   █  ▄▄█▄▄  ██▄█▀  ██▄█▀  ▀█▄█▀
                                                              █      █
                                                              ▀      ▀

                                                       ▄▄▄    ▄▄▄    ▄▄▄█   ▄▄▄
                                                       █▀  ▀  █▀ ▀█  █▀ ▀█  █▀  █
                                                       █      █   █  █   █  █▀▀▀▀
                                                       ▀█▄▄▀  ▀█▄█▀  ▀█▄██  ▀█▄▄▀
```

## Install

```sh
# from a release tarball:
curl -L https://github.com/daniel-farina/hippo-code/releases/latest/download/hip-<version>-<target>.tar.gz | tar -xzf -
cd hip-<version> && ./install.sh

# from source:
git clone https://github.com/daniel-farina/hippo-code.git
cd hippo-code && bun install
./bin/hip --version
```

Requires [bun](https://bun.sh) on PATH and an MTPLX server running locally (default `http://127.0.0.1:8088/v1`).

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
