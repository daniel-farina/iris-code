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
import { DEFAULT_MAX_ROUNDS, MAX_CONSECUTIVE_LENGTH_CONTINUES } from './config.js';
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
  }

  return finish(startTotal, maxRounds, toolCallCount, undefined);
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
