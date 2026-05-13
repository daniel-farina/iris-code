#!/usr/bin/env node
// hip CLI entrypoint. Two modes:
//   * --print "<prompt>"   -> headless: one prompt, stream to stdout, exit
//   * (no args / chat)     -> interactive Ink TUI
//
// Flag parity with hip's Rust binary (hippo-code) where it makes sense:
// --max-tokens, --max-rounds, --temperature, --top-p, --top-k, --url,
// --model, --session, --print/--one-shot, --version.

import { parseArgs } from 'node:util';
import { runLoop } from './agent.js';
import { MtplxClient } from './client.js';
import {
  DEFAULT_MAX_ROUNDS,
  DEFAULT_MAX_TOKENS,
  DEFAULT_MODEL,
  DEFAULT_MTPLX_URL,
  DEFAULT_POSTCOMMIT_DELAY_MS,
  DEFAULT_TEMPERATURE,
  DEFAULT_TOP_K,
  DEFAULT_TOP_P,
  generateAutoSessionId,
} from './config.js';
import { type ChatMessage, systemMessage, userMessage } from './schema.js';
import { DEFAULT_SYSTEM_PROMPT } from './system_prompt.js';

interface Flags {
  print?: string;
  url: string;
  model: string;
  session?: string;
  maxTokens: number;
  maxRounds: number;
  temperature: number;
  topP: number;
  topK: number;
  continueLast?: boolean;
  resume?: string;
  quiet?: boolean;
  system?: string;
  listTools?: boolean;
  /** Hard wall-clock cap on the print-mode agent loop (seconds). 0 = no cap. */
  maxTime?: number;
  /** Yield this many ms between agent rounds to let MTPLX postcommit land. */
  postcommitDelayMs?: number;
  update?: boolean;
  installInfo?: boolean;
  version?: boolean;
  help?: boolean;
  /** TUI only: when false, suppresses the conv>9K-token auto-/compact. */
  autoCompact?: boolean;
  /** When set, before normal work clear MTPLX sessions idle longer
   *  than this many minutes (frees session-bank slots). 0/undefined = off. */
  clearStaleSessionsMinutes?: number;
}

const VERSION = '0.4.1-dev';

/** Normalize a bare `--clear-stale-sessions` (with no following value or
 *  followed by another flag) to `--clear-stale-sessions=30`. node:parseArgs
 *  treats string options as required-value, so a bare flag yields a
 *  cryptic "argument missing" error otherwise. */
function normalizeBareClearStale(argv: string[]): string[] {
  const KEY = '--clear-stale-sessions';
  const out: string[] = [];
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === KEY) {
      const next = argv[i + 1];
      // Bare if at end OR next token looks like a flag/option.
      if (next === undefined || next.startsWith('-')) {
        out.push(`${KEY}=30`);
        continue;
      }
    }
    if (a !== undefined) out.push(a);
  }
  return out;
}

export function parseFlags(argv: string[]): Flags {
  // Normalize bare `--clear-stale-sessions` (no arg) to the
  // documented default of 30 minutes. node:parseArgs treats string
  // flags as eagerly consuming a value and errors on a bare presence.
  const argvNorm = normalizeBareClearStale(argv);
  const { values, positionals } = parseArgs({
    args: argvNorm,
    allowPositionals: true,
    options: {
      // --print / --one-shot are boolean toggles; the prompt is always
      // read from positionals (or stdin in the future). This lets
      // `--print --quiet "foo bar"` parse correctly under node:parseArgs,
      // which otherwise treats `--print --quiet` as print=`--quiet`.
      print: { type: 'boolean' },
      'one-shot': { type: 'boolean' },
      url: { type: 'string' },
      model: { type: 'string' },
      session: { type: 'string' },
      'max-tokens': { type: 'string' },
      'max-rounds': { type: 'string' },
      temperature: { type: 'string' },
      'top-p': { type: 'string' },
      'top-k': { type: 'string' },
      'continue-last': { type: 'boolean', short: 'c' },
      resume: { type: 'string' },
      quiet: { type: 'boolean', short: 'q' },
      system: { type: 'string' },
      'list-tools': { type: 'boolean' },
      'max-time': { type: 'string' },
      'postcommit-delay': { type: 'string' },
      update: { type: 'boolean' },
      'install-info': { type: 'boolean' },
      'no-auto-compact': { type: 'boolean' },
      'clear-stale-sessions': { type: 'string' },
      version: { type: 'boolean', short: 'V' },
      help: { type: 'boolean', short: 'h' },
    },
  });
  const printFlag =
    (values.print as boolean | undefined) ?? (values['one-shot'] as boolean | undefined) ?? false;
  // Use empty string to mean "flag was passed but no prompt" so the
  // print-mode handler can emit a clear error instead of silently
  // dropping into the TUI.
  const printPrompt: string | undefined = printFlag ? positionals.join(' ') : undefined;
  const num = (name: string, raw: string | undefined, fallback: number): number => {
    if (raw === undefined) return fallback;
    const n = Number(raw);
    if (!Number.isFinite(n)) {
      throw new Error(`--${name}: expected a number, got "${raw}"`);
    }
    return n;
  };
  return {
    print: printPrompt,
    url: (values.url as string | undefined) ?? DEFAULT_MTPLX_URL,
    model: (values.model as string | undefined) ?? DEFAULT_MODEL,
    // --session > $HIP_SESSION env var > undefined (auto-id at resolve).
    // The env var lets shell wrappers / cron jobs pin a stable session id
    // across hip invocations so MTPLX's session-bank LRU keeps the slot warm.
    session:
      (values.session as string | undefined) ?? (process.env['HIP_SESSION']?.trim() || undefined),
    maxTokens: num('max-tokens', values['max-tokens'] as string | undefined, DEFAULT_MAX_TOKENS),
    maxRounds: num('max-rounds', values['max-rounds'] as string | undefined, DEFAULT_MAX_ROUNDS),
    temperature: num('temperature', values.temperature as string | undefined, DEFAULT_TEMPERATURE),
    topP: num('top-p', values['top-p'] as string | undefined, DEFAULT_TOP_P),
    topK: num('top-k', values['top-k'] as string | undefined, DEFAULT_TOP_K),
    continueLast: (values['continue-last'] as boolean | undefined) ?? false,
    resume: (values.resume as string | undefined) ?? undefined,
    quiet: (values.quiet as boolean | undefined) ?? false,
    system: (values.system as string | undefined) ?? undefined,
    listTools: (values['list-tools'] as boolean | undefined) ?? false,
    maxTime: num('max-time', values['max-time'] as string | undefined, 0),
    postcommitDelayMs: num(
      'postcommit-delay',
      values['postcommit-delay'] as string | undefined,
      DEFAULT_POSTCOMMIT_DELAY_MS,
    ),
    update: (values.update as boolean | undefined) ?? false,
    installInfo: (values['install-info'] as boolean | undefined) ?? false,
    autoCompact: !((values['no-auto-compact'] as boolean | undefined) ?? false),
    clearStaleSessionsMinutes: values['clear-stale-sessions']
      ? num('clear-stale-sessions', values['clear-stale-sessions'] as string, 30)
      : undefined,
    version: (values.version as boolean | undefined) ?? false,
    help: (values.help as boolean | undefined) ?? false,
  };
}

function printHelp() {
  process.stdout.write(`hip ${VERSION} -- TypeScript port of hip (mlx-code)

Usage:
  hip [OPTIONS] [PROMPT...]
  hip --print "prompt"            # headless: one prompt, stream to stdout
  echo "prompt" | hip --print     # headless via stdin pipe
  hip                             # interactive chat REPL

Options:
  --print, --one-shot <prompt>       headless mode (also reads positional args)
  --url <url>                        MTPLX base URL (default ${DEFAULT_MTPLX_URL})
  --model <id>                       model id (default ${DEFAULT_MODEL})
  --session <id>                     stable session id; reuse across hip runs so MTPLX's 8-slot session bank keeps the prefix cache warm (default auto-YYYYMMDD-HHMMSS-<hex>)
  -c, --continue-last                resume the most recent session from ~/.hip/sessions.jsonl
  --resume <id>                      resume a specific session id
  -q, --quiet                        --print mode: suppress tool-call logs on stderr (final answer only)
  --system <prompt>                  override the default coding-assistant system prompt
  --list-tools                       print the tool spec (json) and exit
  --max-time <seconds>               --print mode: hard wall-clock cap; aborts the agent loop
  --postcommit-delay <ms>            max time to wait for MTPLX postcommit between rounds when prompt >8K tok (polls; default ${DEFAULT_POSTCOMMIT_DELAY_MS}, 0 disables)
  --no-auto-compact                  TUI: disable auto-/compact when conv approaches ~9K tokens
  --clear-stale-sessions [N]         at startup, clear MTPLX sessions idle >N min (default 30) to free session-bank slots
  --update                           download + install the latest release from GitHub
  --install-info                     print where hip is installed and the target platform
  --max-tokens <n>                   max output tokens per turn (default ${DEFAULT_MAX_TOKENS})
  --max-rounds <n>                   max agent loop rounds (default ${DEFAULT_MAX_ROUNDS})
  --temperature <f>                  sampler temperature (default ${DEFAULT_TEMPERATURE})
  --top-p <f>                        sampler top-p (default ${DEFAULT_TOP_P})
  --top-k <n>                        sampler top-k (default ${DEFAULT_TOP_K})
  -V, --version                      print version
  -h, --help                         this message

Environment:
  MLX_CODE_URL                       overrides --url
  MLX_CODE_MODEL                     overrides --model
  HIP_SESSION                        fallback for --session (--session flag wins)
  HIP_HOME                           override session-store dir (default ~/.hip)
`);
}

async function main() {
  const flags = parseFlags(process.argv.slice(2));
  if (flags.help) {
    printHelp();
    return;
  }
  if (flags.version) {
    process.stdout.write(`hip ${VERSION}\n`);
    return;
  }
  if (flags.listTools) {
    const { toolSpecs } = await import('./tools/index.js');
    process.stdout.write(JSON.stringify(toolSpecs(), null, 2) + '\n');
    return;
  }
  if (flags.update) {
    const { runUpdate } = await import('./updater.js');
    await runUpdate(VERSION);
    return;
  }
  if (flags.installInfo) {
    const { printInstallInfo } = await import('./updater.js');
    printInstallInfo();
    return;
  }

  // Resolve session + starting conversation:
  //   --resume <id>     -> load conv from store
  //   --continue-last   -> load the most recent session's conv
  //   else              -> new conv with system prompt only
  const { resolvedSessionId, startConv } = await resolveSessionAndConv(flags);

  // Pre-flight stale-session cleanup (frees MTPLX session-bank slots).
  // Runs AFTER session resolution so we know which id to protect.
  if (typeof flags.clearStaleSessionsMinutes === 'number') {
    const { clearStaleSessions } = await import('./clear_stale.js');
    try {
      const r = await clearStaleSessions({
        v1BaseUrl: flags.url,
        cutoffMinutes: flags.clearStaleSessionsMinutes,
        activeSessionId: resolvedSessionId,
      });
      const skip = r.skippedActive.length > 0 ? ` (skipped active: ${r.skippedActive.length})` : '';
      const errs = r.errors.length > 0 ? ` (errors: ${r.errors.length})` : '';
      process.stderr.write(
        `[hip] cleared ${r.cleared.length} stale session(s) (>${flags.clearStaleSessionsMinutes}min idle)${skip}${errs}\n`,
      );
    } catch (e) {
      // Non-fatal: continue with hip's normal work even if admin endpoint
      // is unreachable (older MTPLX, network glitch).
      process.stderr.write(
        `[hip] --clear-stale-sessions failed: ${(e as Error).message} (continuing)\n`,
      );
    }
  }

  if (flags.print !== undefined) {
    // Headless: if no positional prompt was passed but stdin is piped,
    // read the prompt from stdin (UNIX idiom: `echo "..." | hip -p`).
    if (flags.print.trim().length === 0 && !process.stdin.isTTY) {
      const piped = await readAllStdin();
      if (piped.trim().length > 0) flags.print = piped.trim();
    }
    return runPrintMode(flags, resolvedSessionId, startConv);
  }

  // Interactive TUI - lazy-import so --print doesn't pay the React load cost.
  const { runTui } = await import('./ui/tui.js');
  await runTui(flags, resolvedSessionId);
}

async function resolveSessionAndConv(
  flags: Flags,
): Promise<{ resolvedSessionId: string; startConv: ChatMessage[] }> {
  const { findSession, lastSession } = await import('./session_store.js');
  if (flags.resume) {
    const rec = await findSession(flags.resume);
    if (!rec) {
      process.stderr.write(`[hip] session ${flags.resume} not found; starting fresh\n`);
    } else {
      process.stderr.write(`[hip] resumed session ${rec.session_id} (${rec.conv.length} msgs)\n`);
      return { resolvedSessionId: rec.session_id, startConv: rec.conv };
    }
  } else if (flags.continueLast) {
    const rec = await lastSession();
    if (!rec) {
      process.stderr.write('[hip] no prior sessions; starting fresh\n');
    } else {
      process.stderr.write(`[hip] continuing ${rec.session_id} (${rec.conv.length} msgs)\n`);
      return { resolvedSessionId: rec.session_id, startConv: rec.conv };
    }
  }
  return {
    resolvedSessionId: flags.session ?? generateAutoSessionId(),
    startConv: [systemMessage(flags.system ?? DEFAULT_SYSTEM_PROMPT)],
  };
}

async function readAllStdin(): Promise<string> {
  const chunks: Buffer[] = [];
  for await (const chunk of process.stdin) {
    chunks.push(chunk as Buffer);
  }
  return Buffer.concat(chunks).toString('utf8');
}

async function runPrintMode(
  flags: Flags,
  sessionId: string,
  startConv: ChatMessage[],
): Promise<void> {
  if (!flags.print || flags.print.trim().length === 0) {
    process.stderr.write(
      'hip --print: empty prompt (pass prompt as positional, or pipe via stdin)\n',
    );
    process.exit(2);
  }
  const client = new MtplxClient({
    baseUrl: flags.url,
    model: flags.model,
    sessionId,
    onContent: (t) => process.stdout.write(t),
  });
  const conv: ChatMessage[] = [...startConv, userMessage(flags.print)];
  let completionTokens = 0;
  const toolStartMs: Record<string, number> = {};
  // SIGINT in headless mode cancels the in-flight stream + agent loop.
  // Without this, Ctrl-C just kills the process mid-stream and loses any
  // tool results that were already collected for this turn.
  const abort = new AbortController();
  const onSigint = () => {
    if (!abort.signal.aborted) {
      process.stderr.write(
        `\n[hip] received SIGINT, aborting...\n[hip] to resume this session later, run:\n  hip --resume ${sessionId}\n`,
      );
      abort.abort();
    }
  };
  process.on('SIGINT', onSigint);
  // Wall-clock cap so cron-style invocations can't hang forever. 0 = off.
  let maxTimeTimer: NodeJS.Timeout | undefined;
  if (flags.maxTime && flags.maxTime > 0) {
    maxTimeTimer = setTimeout(() => {
      if (!abort.signal.aborted) {
        process.stderr.write(`\n[hip] --max-time ${flags.maxTime}s reached, aborting...\n`);
        abort.abort();
      }
    }, flags.maxTime * 1000);
    // Don't keep the event loop alive just for this timer.
    maxTimeTimer.unref?.();
  }
  const stats = await runLoop({
    client,
    conv,
    signal: abort.signal,
    sampling: {
      temperature: flags.temperature,
      top_p: flags.topP,
      top_k: flags.topK,
      max_tokens: flags.maxTokens,
    },
    maxRounds: flags.maxRounds,
    postcommitDelayMs: flags.postcommitDelayMs,
    events: {
      onRound: (_n, info) => {
        if (typeof info.ctok === 'number') completionTokens += info.ctok;
      },
      onPostcommitWait: flags.quiet
        ? undefined
        : (maxMs) =>
            process.stderr.write(`[hip] waiting up to ${maxMs}ms for MTPLX postcommit...\n`),
      onPostcommitDone: flags.quiet
        ? undefined
        : (landed, elapsedMs) =>
            process.stderr.write(
              `[hip] postcommit ${landed ? 'landed' : 'timed-out'} after ${elapsedMs.toFixed(0)}ms\n`,
            ),
      onToolStart: flags.quiet
        ? undefined
        : (name, args) => {
            (toolStartMs as Record<string, number>)[name] = performance.now();
            process.stderr.write(`\n[tool] ${name}(${jsonOneLine(args)})\n`);
          },
      onToolResult: flags.quiet
        ? undefined
        : (name, ok, body) => {
            const t = (toolStartMs as Record<string, number>)[name];
            const ms = t ? ` (${(performance.now() - t).toFixed(0)}ms)` : '';
            process.stderr.write(
              `[${ok ? 'ok' : 'err'}]${ms} ${name}: ${body.split('\n').slice(0, 3).join('\\n').slice(0, 200)}\n`,
            );
          },
    },
  });

  // Persist the session so --continue-last / --resume can pick it up.
  const { updateSession } = await import('./session_store.js');
  const firstUser = conv.find((m) => m.role === 'user')?.content ?? '';
  await updateSession({
    session_id: sessionId,
    ts_unix: Math.floor(Date.now() / 1000),
    cwd: process.cwd(),
    first_user: typeof firstUser === 'string' ? firstUser.slice(0, 200) : '',
    conv,
  });
  process.off('SIGINT', onSigint);
  if (maxTimeTimer) clearTimeout(maxTimeTimer);
  const tps =
    completionTokens > 0 && stats.totalMs > 0
      ? ` tok/s=${((completionTokens / stats.totalMs) * 1000).toFixed(1)}`
      : '';
  // `undefined` finish means we hit the maxRounds cap; surface that
  // explicitly instead of a useless '?'.
  const finishLabel = stats.finishReason ?? (stats.rounds >= flags.maxRounds ? 'max_rounds' : '?');
  process.stderr.write(
    `\n[done] session=${sessionId} rounds=${stats.rounds} tool_calls=${stats.toolCalls} ms=${stats.totalMs.toFixed(0)} ctok=${completionTokens}${tps} finish=${finishLabel}\n`,
  );
}

function jsonOneLine(o: Record<string, unknown>): string {
  return JSON.stringify(o).slice(0, 160);
}

// Skip the main() invocation when imported from a test runner. Tests
// only need parseFlags() and shouldn't trigger the TUI/print pipeline.
const isUnderTest =
  typeof process.env['VITEST'] !== 'undefined' ||
  typeof process.env['BUN_TEST'] !== 'undefined' ||
  process.argv.some(
    (a) => a.includes('vitest') || a.endsWith('.test.ts') || a.endsWith('.test.js'),
  );
if (!isUnderTest) {
  main().catch((e: unknown) => {
    const msg = (e as Error).message ?? String(e);
    // Network errors usually mean MTPLX isn't running. Surface a
    // specific hint so first-run users don't get stuck on a generic
    // "fetch failed" message.
    const isNetErr =
      /fetch failed|ECONNREFUSED|ENOTFOUND|getaddrinfo|unable to connect|connection refused/i.test(
        msg,
      ) || (e as Error).name === 'TypeError';
    if (isNetErr) {
      process.stderr.write(
        `\n[hip error] could not reach MTPLX (${msg}).\n` +
          `  is the server running on the expected URL?  (override with --url or MLX_CODE_URL)\n`,
      );
    } else {
      process.stderr.write(`\n[hip error] ${msg}\n`);
    }
    process.exit(1);
  });
}
