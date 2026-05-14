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
import { setDelegateContext } from './tools/delegate.js';
import {
  DEFAULT_AUTO_COMPACT_THRESHOLD_TOKENS,
  DEFAULT_MAX_ROUNDS,
  DEFAULT_MAX_TOKENS,
  DEFAULT_MODEL,
  DEFAULT_MTPLX_URL,
  DEFAULT_POSTCOMMIT_DELAY_MS,
  DEFAULT_TEMPERATURE,
  DEFAULT_TOP_K,
  DEFAULT_TOP_P,
  generateAutoSessionId,
  generateCwdStableSessionId,
} from './config.js';
import { type ChatMessage, systemMessage, userMessage } from './schema.js';
import { DEFAULT_SYSTEM_PROMPT, withEnv } from './system_prompt.js';
import { VERSION } from './version.js';

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
  fresh?: boolean;
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
  /** Interactive: detect MTPLX, diff against recommended settings, offer restart. */
  setup?: boolean;
  /** Suppress the one-line drift banner that prints on every launch. */
  noDriftCheck?: boolean;
  version?: boolean;
  help?: boolean;
  /** TUI only: when false, suppresses the conv>9K-token auto-/compact. */
  autoCompact?: boolean;
  /** When set, before normal work clear MTPLX sessions idle longer
   *  than this many minutes (frees session-bank slots). 0/undefined = off. */
  clearStaleSessionsMinutes?: number;
  /** Sidecar model id (e.g. gemma4:e2b). When set, hip fires a parallel
   *  summarize call after each round to maintain a free running summary
   *  used by compact instead of the main-model summarize. */
  sidecarModel?: string;
  /** Sidecar base URL (no trailing /v1). Default http://localhost:11434
   *  (Ollama default). */
  sidecarUrl?: string;
  /** When set, after --print exits hip will git-add+commit any
   *  uncommitted changes in cwd if it's a git repo. Commit message is
   *  derived from runningSummary (sidecar) or the first user prompt.
   *  Safety net for when the model forgets to commit-as-it-goes. */
  autoCommit?: boolean;
  /** Post-loop browser_check on a localhost dev URL. Catches JS load
   *  errors the model never saw because it never loaded the page. */
  browserCheck?: boolean;
  /** URL to check; default http://localhost:5173 (Vite). */
  browserCheckUrl?: string;
}

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
      fresh: { type: 'boolean' },
      resume: { type: 'string' },
      quiet: { type: 'boolean', short: 'q' },
      system: { type: 'string' },
      'list-tools': { type: 'boolean' },
      'max-time': { type: 'string' },
      'postcommit-delay': { type: 'string' },
      update: { type: 'boolean' },
      'install-info': { type: 'boolean' },
      setup: { type: 'boolean' },
      'no-drift-check': { type: 'boolean' },
      'no-auto-compact': { type: 'boolean' },
      sidecar: { type: 'string' },
      'sidecar-url': { type: 'string' },
      'auto-commit': { type: 'boolean' },
      'browser-check': { type: 'boolean' },
      'no-browser-check': { type: 'boolean' },
      'browser-check-url': { type: 'string' },
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
    fresh: (values.fresh as boolean | undefined) ?? false,
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
    setup: (values.setup as boolean | undefined) ?? false,
    noDriftCheck: (values['no-drift-check'] as boolean | undefined) ?? false,
    autoCompact: !((values['no-auto-compact'] as boolean | undefined) ?? false),
    sidecarModel: (values.sidecar as string | undefined) ?? process.env['HIP_SIDECAR_MODEL'],
    sidecarUrl:
      (values['sidecar-url'] as string | undefined) ??
      process.env['HIP_SIDECAR_URL'] ??
      'http://localhost:11434',
    autoCommit:
      (values['auto-commit'] as boolean | undefined) ?? process.env['HIP_AUTO_COMMIT'] === '1',
    // Browser-check defaults ON. Explicit --browser-check or
    // --no-browser-check flag wins. Env HIP_NO_BROWSER_CHECK=1 also
    // disables.
    browserCheck:
      (values['browser-check'] as boolean | undefined) ??
      !(
        (values['no-browser-check'] as boolean | undefined) === true ||
        process.env['HIP_NO_BROWSER_CHECK'] === '1'
      ),
    browserCheckUrl:
      (values['browser-check-url'] as string | undefined) ??
      process.env['HIP_BROWSER_CHECK_URL'] ??
      'http://localhost:5173',
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
  -c, --continue-last                resume the most recent session in this cwd (falls back to global-latest if none)
  --fresh                            force a brand-new session, skip auto-resume of the last cwd session
  --resume <id>                      resume a specific session id
  -q, --quiet                        --print mode: suppress tool-call logs on stderr (final answer only)
  --system <prompt>                  override the default coding-assistant system prompt
  --list-tools                       print the tool spec (json) and exit
  --max-time <seconds>               --print mode: hard wall-clock cap; aborts the agent loop
  --postcommit-delay <ms>            max time to wait for MTPLX postcommit between rounds when prompt >8K tok (polls; default ${DEFAULT_POSTCOMMIT_DELAY_MS}, 0 disables)
  --no-auto-compact                  TUI: disable auto-/compact when conv approaches ~9K tokens
  --sidecar <model>                  Ollama small-model id (e.g. gemma4:e2b) for parallel per-round summaries; free compact (env: HIP_SIDECAR_MODEL). Auto-detected if Ollama is reachable with a known small model.
  --sidecar-url <url>                Ollama base URL for --sidecar (default http://localhost:11434; env: HIP_SIDECAR_URL)
  --auto-commit                      after --print exits, sweep up any uncommitted changes into one commit using a sidecar-derived message (env: HIP_AUTO_COMMIT=1)
  --browser-check / --no-browser-check  after --print exits, headless-Chrome load URL + dump console errors (default ON; env: HIP_NO_BROWSER_CHECK=1 to disable)
  --browser-check-url <url>          URL to check (default http://localhost:5173; env: HIP_BROWSER_CHECK_URL)
  --clear-stale-sessions [N]         at startup, clear MTPLX sessions idle >N min (default 30) to free session-bank slots
  --update                           download the latest hip release, install over the current binary, then verify MTPLX config
  --install-info                     print where hip is installed and the target platform
  --setup                            interactive: detect MTPLX, diff vs recommended, offer restart
  --no-drift-check                   suppress the MTPLX config-drift banner on launch
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
  if (flags.setup) {
    const { runSetup } = await import('./setup.js');
    await runSetup(flags.url);
    return;
  }
  // Passive drift banner on regular launches. Cheap (one HTTP probe +
  // one `ps eww`); non-blocking; suppressed by --no-drift-check / -q.
  if (!flags.noDriftCheck && !flags.quiet) {
    const { quickDriftCheck } = await import('./mtplx_manager.js');
    const warning = await quickDriftCheck(flags.url);
    if (warning) process.stderr.write(`${warning}\n`);
  }

  // Resolve session + starting conversation:
  //   --resume <id>     -> load conv from store
  //   --continue-last   -> load the most recent session's conv
  //   else              -> new conv with system prompt only
  const { resolvedSessionId, startConv, startRunningSummary, startTasks, startTaskNextId } =
    await resolveSessionAndConv(flags);

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
    return runPrintMode(flags, resolvedSessionId, startConv, startRunningSummary);
  }

  // Interactive TUI - lazy-import so --print doesn't pay the React load cost.
  const { runTui } = await import('./ui/tui.js');
  await runTui(flags, resolvedSessionId, {
    startConv,
    startRunningSummary,
    startTasks,
    startTaskNextId,
  });
}

async function resolveSessionAndConv(flags: Flags): Promise<{
  resolvedSessionId: string;
  startConv: ChatMessage[];
  startRunningSummary?: string[];
  startTasks?: import('./task_manager.js').Task[];
  startTaskNextId?: number;
}> {
  const { findSession, lastSession, lastSessionForCwd } = await import('./session_store.js');
  if (flags.resume) {
    const rec = await findSession(flags.resume);
    if (!rec) {
      process.stderr.write(`[hip] session ${flags.resume} not found; starting fresh\n`);
    } else {
      const sumHint = rec.running_summary?.length
        ? `, ${rec.running_summary.length} summary lines`
        : '';
      process.stderr.write(
        `[hip] resumed session ${rec.session_id} (${rec.conv.length} msgs${sumHint})\n`,
      );
      return {
        resolvedSessionId: rec.session_id,
        startConv: rec.conv,
        startRunningSummary: rec.running_summary,
        startTasks: rec.tasks,
        startTaskNextId: rec.task_next_id,
      };
    }
  } else if (flags.continueLast) {
    // Prefer a session from the CURRENT cwd. Falls back to the
    // global-latest if nothing matches (covers fresh installs / new
    // dirs where the user wants the most recent anyway).
    const rec = (await lastSessionForCwd(process.cwd())) ?? (await lastSession());
    if (!rec) {
      process.stderr.write('[hip] no prior sessions; starting fresh\n');
    } else {
      const cwdHint = rec.cwd === process.cwd() ? '' : ` (from ${rec.cwd})`;
      const sumHint = rec.running_summary?.length
        ? `, ${rec.running_summary.length} summary lines`
        : '';
      process.stderr.write(
        `[hip] continuing ${rec.session_id} (${rec.conv.length} msgs${sumHint})${cwdHint}\n`,
      );
      return {
        resolvedSessionId: rec.session_id,
        startConv: rec.conv,
        startRunningSummary: rec.running_summary,
        startTasks: rec.tasks,
        startTaskNextId: rec.task_next_id,
      };
    }
  }
  // No --resume, no --continue-last: by default we still resume the
  // most recent session for this cwd if one exists and is recent
  // enough. This matches the user's mental model of "I closed hip in
  // this folder; reopening picks up where I left off." A --fresh flag
  // (or /new inside the TUI) escapes to a brand-new session.
  // Recency cutoff: 24 hours. Older sessions are stale enough that
  // auto-resuming them would surprise the user; in that case we fall
  // through to a fresh session id.
  if (!flags.fresh) {
    const rec = await lastSessionForCwd(process.cwd());
    if (rec) {
      const ageMs = Date.now() - rec.ts_unix * 1000;
      const ageH = ageMs / (1000 * 60 * 60);
      if (ageH < 24) {
        const sumHint = rec.running_summary?.length
          ? `, ${rec.running_summary.length} summary lines`
          : '';
        process.stderr.write(
          `[hip] auto-resume ${rec.session_id} (${rec.conv.length} msgs${sumHint}, ${Math.round(ageH * 60)}m ago) - pass --fresh for a new session\n`,
        );
        return {
          resolvedSessionId: rec.session_id,
          startConv: rec.conv,
          startRunningSummary: rec.running_summary,
        };
      }
    }
  }
  // Fresh session: a brand-new auto-id (not cwd-stable). The
  // cwd-stable-id experiment caused confusion - MTPLX would keep
  // the old session's cached prefix under that id, but hip's conv
  // would be fresh, so the cache diverged on the very first round.
  // A fresh auto-id cold-prefills the small new prompt cleanly.
  return {
    resolvedSessionId: flags.session ?? generateAutoSessionId(),
    startConv: [systemMessage(withEnv(flags.system ?? DEFAULT_SYSTEM_PROMPT))],
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
  startRunningSummary?: string[],
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
  // If the incoming prompt explicitly says "start over" / "reset" / etc.,
  // and the resumed conv has real history, wipe it and reseed with just
  // the system message. Saves prefix-cache bloat and gives the model a
  // clean head on the new task. Logged so the user sees what happened.
  const { detectResetIntent } = await import('./topic_detect.js');
  let baseConv = startConv;
  if (detectResetIntent(flags.print, startConv.length)) {
    const sysMsg = startConv.find((m) => m.role === 'system');
    baseConv = sysMsg ? [sysMsg] : [systemMessage(withEnv(flags.system ?? DEFAULT_SYSTEM_PROMPT))];
    if (!flags.quiet) {
      process.stderr.write(
        `[hip] detected reset/new-task signal in prompt; cleared ${startConv.length - baseConv.length} prior messages\n`,
      );
    }
  }
  const conv: ChatMessage[] = [...baseConv, userMessage(flags.print)];
  let completionTokens = 0;
  let compactFires = 0;
  let compactSavedTokens = 0;
  let sidecarLines = 0;
  // Seed with the resumed session's running summary (if any) so the
  // FIRST compact in this run is also fed by sidecar lines and stays
  // free instead of paying the main-model summarize cost.
  const runningSummary: string[] = startRunningSummary ? [...startRunningSummary] : [];
  const toolStartMs: Record<string, number> = {};
  // Sidecar: explicit flag wins; otherwise probe Ollama and pick a
  // small model if installed. Probe has a 500ms timeout - never blocks.
  const { autoDetectSidecar } = await import('./sidecar.js');
  const resolvedSidecar = flags.sidecarModel
    ? { url: flags.sidecarUrl ?? 'http://localhost:11434', model: flags.sidecarModel }
    : await autoDetectSidecar(flags.sidecarUrl);
  if (resolvedSidecar && !flags.quiet && !flags.sidecarModel) {
    process.stderr.write(
      `[hip] sidecar auto-detected: ${resolvedSidecar.model} @ ${resolvedSidecar.url}\n`,
    );
  }
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
  // Thread delegate-tool context so sub-agents reuse the same MTPLX
  // server + model + sampling as the main loop.
  setDelegateContext({
    baseUrl: flags.url,
    model: flags.model,
    parentSessionId: sessionId,
    sampling: {
      temperature: flags.temperature,
      top_p: flags.topP,
      top_k: flags.topK,
      max_tokens: flags.maxTokens,
    },
  });
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
    // Mid-loop auto-compact only when explicitly enabled by the user
    // (or by default if they didn't pass --no-auto-compact). The TUI
    // already wires its own auto-compact; in --print mode this is
    // brand new and gates on the same flag.
    autoCompactThresholdTokens:
      flags.autoCompact === false ? 0 : DEFAULT_AUTO_COMPACT_THRESHOLD_TOKENS,
    systemPromptForCompact: flags.system,
    sidecar: resolvedSidecar ?? undefined,
    runningSummary: resolvedSidecar ? runningSummary : undefined,
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
      onToolResultPrune: flags.quiet
        ? undefined
        : (pruned, charsSaved) =>
            process.stderr.write(
              `[hip] pruned ${pruned} old tool result${pruned === 1 ? '' : 's'} (~${(charsSaved / 4).toFixed(0)} tok saved)\n`,
            ),
      onSidecarSummary: (line, total) => {
        sidecarLines = total;
        if (!flags.quiet) {
          process.stderr.write(`[hip:sidecar] ${line}\n`);
        }
      },
      onCompactStart: flags.quiet
        ? undefined
        : (tokensBefore) =>
            process.stderr.write(`\n[hip] auto-compact starting (conv ~${tokensBefore} tok)...\n`),
      onCompactDone: (tokensBefore, tokensAfter, msgsBefore) => {
        compactFires++;
        compactSavedTokens += Math.max(0, tokensBefore - tokensAfter);
        if (!flags.quiet) {
          process.stderr.write(
            `[hip] auto-compacted ${msgsBefore} msgs / ~${tokensBefore} tok → ~${tokensAfter} tok\n`,
          );
        }
      },
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
    // Persist sidecar lines so a future --resume / --continue-last
    // inherits them and the first compact in that run is also ~free.
    running_summary: runningSummary.length > 0 ? runningSummary : undefined,
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
  const compactStats =
    compactFires > 0 ? ` compact=${compactFires}x/-${compactSavedTokens}tok` : '';
  const sidecarStats = sidecarLines > 0 ? ` sidecar=${sidecarLines}lines` : '';
  process.stderr.write(
    `\n[done] session=${sessionId} rounds=${stats.rounds} tool_calls=${stats.toolCalls} ms=${stats.totalMs.toFixed(0)} ctok=${completionTokens}${tps}${compactStats}${sidecarStats} finish=${finishLabel}\n`,
  );
  // Safety-net auto-commit: if the user asked for it and the model
  // didn't commit on its own, sweep up any dirty changes.
  // Post-loop browser check. Runs BEFORE auto-commit so a broken page
  // doesn't get checkpointed. If errors found, we still let auto-commit
  // proceed (the work might be partial-by-design) but the errors land
  // in stderr so the user sees them.
  if (flags.browserCheck) {
    const { browserCheck, formatBrowserCheck } = await import('./browser_check.js');
    const r = await browserCheck({ url: flags.browserCheckUrl ?? 'http://localhost:5173' });
    process.stderr.write(`${formatBrowserCheck(r)}\n`);
  }
  if (flags.autoCommit) {
    const { autoCommit } = await import('./auto_commit.js');
    const result = autoCommit(process.cwd(), runningSummary, flags.print ?? '');
    if (result.committed) {
      process.stderr.write(
        `[hip] auto-commit: ${result.message} (${result.files?.length ?? 0} files)\n`,
      );
      // The commit checkpoints the work in git; the sidecar summaries
      // that described it are now redundant. Clear them so the NEXT
      // resume doesn't re-apply stale "X is incomplete" lines for
      // things that have since been committed and resolved.
      runningSummary.length = 0;
      const checkpointSubject = result.message?.split('\n')[0] ?? 'committed work';
      runningSummary.push(`[checkpoint] ${checkpointSubject}`);
    } else {
      // Always surface SOMETHING when the user opted in - they should
      // know whether the sweep ran or was a no-op.
      process.stderr.write(`[hip] auto-commit: ${result.reason}\n`);
    }
  }
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
