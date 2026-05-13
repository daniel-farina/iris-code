// Agent loop. Streams model output, executes tool_calls, appends results,
// repeats until the model returns a tool-call-free response or hits the
// round cap. Ported behavior from hip's src/agent.rs:
//
//   - max_rounds caps the loop (default 30)
//   - finish_reason="length" triggers up to MAX_CONSECUTIVE_LENGTH_CONTINUES
//     auto-continues with an empty user turn (resets when a non-length
//     finish lands)
//   - tool results go in as role="tool" with tool_call_id back-pointing to
//     the originating call, content as plain text from the tool's run()
//   - errors during tool run are stringified and sent back so the model
//     can recover (don't blow up the loop)

import type { MtplxClient, SamplingOpts } from './client.js';
import {
  DEFAULT_MAX_ROUNDS,
  DEFAULT_POSTCOMMIT_DELAY_MS,
  MAX_CONSECUTIVE_LENGTH_CONTINUES,
} from './config.js';
import {
  type ChatMessage,
  type CompletionResult,
  assistantToolCalls,
  toolResultMessage,
} from './schema.js';
import { type Tool, findTool, toolSpecs } from './tools/index.js';

export interface AgentEvents {
  /** Streamed assistant content delta (after any client-side filtering). */
  onContent?: (text: string) => void;
  /** Fired with the tool name + parsed args before exec; useful for UI status. */
  onToolStart?: (name: string, args: Record<string, unknown>) => void;
  /** Fired after tool exec with the truncated tool output (already string-coerced). */
  onToolResult?: (name: string, ok: boolean, body: string) => void;
  /** Each agent round (one model response + zero-or-more tool exec). */
  onRound?: (round: number, info: { finishReason?: string; ctok?: number }) => void;
  /** Fired before a postcommit-yielding pause between rounds; lets the
   * UI show a "yielding to MTPLX..." status. maxMs is the upper bound. */
  onPostcommitWait?: (maxMs: number) => void;
  /** Fired after the postcommit wait finishes with the actual outcome:
   *  whether MTPLX confirmed the commit landed, and how long we waited. */
  onPostcommitDone?: (landed: boolean, elapsedMs: number) => void;
}

export interface RunLoopOptions {
  client: MtplxClient;
  conv: ChatMessage[];
  sampling: SamplingOpts;
  maxRounds?: number;
  events?: AgentEvents;
  /** Optional AbortSignal - aborting cancels the in-flight stream + ends
   * the loop on the next round boundary. Used by the TUI for Ctrl-C
   * during streaming. */
  signal?: AbortSignal;
  /** Maximum time (ms) to wait for MTPLX's KV-cache postcommit to land
   * between agent rounds (default 30000). The agent polls
   * /admin/sessions and returns as soon as the previous round's
   * postcommit is stored. Empirically observed commit times: 3-25s
   * depending on prefix size, device, and memory pressure. Set to 0
   * to disable polling entirely (cache will stay cold on big convs). */
  postcommitDelayMs?: number;
}

export interface LoopStats {
  rounds: number;
  toolCalls: number;
  totalMs: number;
  finishReason?: string;
}

export async function runLoop(opts: RunLoopOptions): Promise<LoopStats> {
  const { client, conv, sampling, events, signal } = opts;
  const maxRounds = opts.maxRounds ?? DEFAULT_MAX_ROUNDS;
  const postcommitDelayMs = opts.postcommitDelayMs ?? DEFAULT_POSTCOMMIT_DELAY_MS;
  const specs = toolSpecs();
  const startTotal = performance.now();
  let consecutiveLength = 0;
  let toolCallCount = 0;
  let malformedRetries = 0;

  for (let round = 0; round < maxRounds; round++) {
    if (signal?.aborted) {
      return finish(startTotal, round, toolCallCount, 'aborted');
    }
    let res: CompletionResult;
    try {
      res = await client.stream(conv, specs, sampling, signal);
    } catch (e) {
      if ((e as Error).name === 'AbortError' || signal?.aborted) {
        return finish(startTotal, round, toolCallCount, 'aborted');
      }
      // MTPLX 422 with "malformed tool_call" means the model emitted
      // invalid JSON for a tool_call's arguments (known qwen3 quirk).
      // The bad response never reaches us, so we can't thread a tool
      // result back. Instead inject a user nudge and retry. Cap retries
      // so we don't loop on a model that can't self-correct.
      const msg = (e as Error).message ?? '';
      if (/malformed tool_call|not valid JSON/i.test(msg) && malformedRetries < 2) {
        malformedRetries++;
        conv.push({
          role: 'user',
          content:
            'Your previous response had malformed JSON in a tool_call. Please retry the tool call, ensuring `arguments` is a single valid JSON object with proper quoting.',
        });
        events?.onRound?.(round + 1, { finishReason: 'malformed_retry' });
        continue;
      }
      throw e;
    }
    events?.onRound?.(round + 1, {
      finishReason: res.finish_reason,
      ctok: res.usage?.completion_tokens,
    });

    if (res.finish_reason === 'length' && res.tool_calls.length === 0) {
      // Auto-continue. Append the partial text and prompt an implicit
      // "keep going" by re-sending without tool_calls flag.
      consecutiveLength++;
      if (consecutiveLength > MAX_CONSECUTIVE_LENGTH_CONTINUES) {
        return finish(startTotal, round + 1, toolCallCount, res.finish_reason);
      }
      conv.push({ role: 'assistant', content: res.content });
      continue;
    }
    consecutiveLength = 0;

    if (res.tool_calls.length === 0) {
      // Done. Append the assistant text and return.
      if (res.content.trim().length > 0) conv.push({ role: 'assistant', content: res.content });
      return finish(startTotal, round + 1, toolCallCount, res.finish_reason);
    }

    // Otherwise: append assistant tool_calls message, exec each, append results.
    conv.push(assistantToolCalls(res.content, res.tool_calls));

    for (const call of res.tool_calls) {
      toolCallCount++;
      const tool: Tool | undefined = findTool(call.function.name);
      let args: Record<string, unknown> = {};
      try {
        args = call.function.arguments ? JSON.parse(call.function.arguments) : {};
      } catch (e) {
        const msg = `tool '${call.function.name}': invalid JSON arguments: ${(e as Error).message}`;
        events?.onToolResult?.(call.function.name, false, msg);
        conv.push(toolResultMessage(call.id, call.function.name, msg));
        continue;
      }
      events?.onToolStart?.(call.function.name, args);
      if (!tool) {
        const msg = `unknown tool '${call.function.name}'`;
        events?.onToolResult?.(call.function.name, false, msg);
        conv.push(toolResultMessage(call.id, call.function.name, msg));
        continue;
      }
      let body: string;
      let ok = true;
      try {
        body = await tool.run(args);
      } catch (e) {
        ok = false;
        body = `[error] ${(e as Error).message}`;
      }
      events?.onToolResult?.(call.function.name, ok, body);
      conv.push(toolResultMessage(call.id, call.function.name, body));
    }

    // Between agent-loop rounds, poll MTPLX's /admin/sessions until
    // the previous round's KV-cache postcommit lands (stored=true) or
    // we cap out. We always poll - the gate is gone because the first
    // round of any turn is usually small (3-4K prefix) yet still
    // produces a postcommit the next round will benefit from. The
    // poll is cheap when there's nothing pending: one HTTP round-trip
    // resolves immediately.
    if (postcommitDelayMs > 0) {
      events?.onPostcommitWait?.(postcommitDelayMs);
      const sid = client.getSessionId?.();
      const baseUrl = client.getBaseUrl?.();
      let result = { landed: false, elapsedMs: 0 };
      if (sid && baseUrl) {
        result = await waitForPostcommit(baseUrl, sid, postcommitDelayMs, signal);
      } else {
        // No way to know when commit landed (test fakes / non-MTPLX
        // backend). Skip the wait rather than sleep blindly.
        result = { landed: true, elapsedMs: 0 };
      }
      events?.onPostcommitDone?.(result.landed, result.elapsedMs);
    }
  }

  return finish(startTotal, maxRounds, toolCallCount, undefined);
}

/** Poll MTPLX's /admin/sessions until this session's postcommit lands
 *  (stored=true) or we hit maxMs. Returns landed=true on success. The
 *  admin endpoint lives on the MTPLX root, one level above /v1; we
 *  derive it by stripping the trailing /v1[/]. */
async function waitForPostcommit(
  v1BaseUrl: string,
  sessionId: string,
  maxMs: number,
  signal?: AbortSignal,
): Promise<{ landed: boolean; elapsedMs: number }> {
  const adminUrl = `${v1BaseUrl.replace(/\/v1\/?$/, '')}/admin/sessions`;
  const start = performance.now();
  const POLL_INTERVAL_MS = 300;
  let sawAnyPostcommit = false;
  while (performance.now() - start < maxMs) {
    if (signal?.aborted) return { landed: false, elapsedMs: performance.now() - start };
    try {
      const res = await fetch(adminUrl, { signal });
      if (!res.ok) {
        // Admin not available - sleep the cap and bail.
        await sleepWithAbort(maxMs - (performance.now() - start), signal);
        return { landed: false, elapsedMs: performance.now() - start };
      }
      const data = (await res.json()) as { sessions?: SessionEntry[] } | SessionEntry[];
      const sessions: SessionEntry[] = Array.isArray(data) ? data : (data.sessions ?? []);
      const session = sessions.find((s) => s.session_id === sessionId);
      // No record at all: first request was so quick MTPLX hasn't seen
      // a session worth tracking. Return immediately - nothing to wait for.
      if (!session) return { landed: true, elapsedMs: performance.now() - start };
      const pc = session.last_postcommit_outcome ?? {};
      // Stored: we're done, the next request will hit warm cache.
      if (pc.stored === true) return { landed: true, elapsedMs: performance.now() - start };
      // Explicit abort with no pending work: MTPLX gave up; waiting is pointless.
      if (session.pending_postcommit === false && pc.mode === 'aborted') {
        return { landed: false, elapsedMs: performance.now() - start };
      }
      // Track whether we've ever seen a postcommit attempt for this
      // session. If we never see one after a few polls, assume there's
      // nothing to wait for (first turn, request was too quick to commit).
      if (pc.mode || pc.stored !== undefined || session.pending_postcommit) {
        sawAnyPostcommit = true;
      } else if (!sawAnyPostcommit && performance.now() - start > 1500) {
        // 1.5s with no postcommit activity for this session: bail.
        return { landed: true, elapsedMs: performance.now() - start };
      }
    } catch {
      // Network/parse error - bail out without waiting the whole cap.
      break;
    }
    await sleepWithAbort(POLL_INTERVAL_MS, signal);
  }
  return { landed: false, elapsedMs: performance.now() - start };
}

interface SessionEntry {
  session_id: string;
  pending_postcommit?: boolean;
  last_postcommit_outcome?: {
    stored?: boolean;
    mode?: string;
    reason?: string;
  };
}

/** setTimeout-based sleep that resolves early when the AbortSignal fires. */
function sleepWithAbort(ms: number, signal?: AbortSignal): Promise<void> {
  if (ms <= 0) return Promise.resolve();
  return new Promise<void>((resolve) => {
    if (signal?.aborted) {
      resolve();
      return;
    }
    const timer = setTimeout(() => {
      signal?.removeEventListener('abort', onAbort);
      resolve();
    }, ms);
    const onAbort = () => {
      clearTimeout(timer);
      resolve();
    };
    signal?.addEventListener('abort', onAbort, { once: true });
  });
}

function finish(
  start: number,
  rounds: number,
  toolCalls: number,
  finishReason: string | undefined,
): LoopStats {
  return {
    rounds,
    toolCalls,
    totalMs: performance.now() - start,
    finishReason,
  };
}
