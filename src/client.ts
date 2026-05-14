// MTPLX HTTP/SSE client. Streams /v1/chat/completions, accumulates per-index
// tool_call deltas into completed ToolCalls, and emits content tokens to
// stdout (or a callback for the TUI).
//
// Ported behavior parity with hip's src/client.rs - same SSE framing
// (data: <json>\n\n), same per-index accumulation by `index` field, same
// stop conditions (finish_reason, [DONE], stream end).

import { generateAutoSessionId } from './config.js';
import type { ChatMessage, CompletionResult, SSEChunk, ToolCall, ToolSpec } from './schema.js';

export interface SamplingOpts {
  temperature: number;
  top_p: number;
  top_k: number;
  max_tokens: number;
}

export interface StreamOptions {
  baseUrl: string;
  model: string;
  sessionId?: string;
  /** Called with each content delta as it arrives - includes any
   *  `<think>…</think>` reasoning the model emits. We do not filter
   *  thinking; the TUI renders it inline so users can see the model's
   *  reasoning. */
  onContent?: (text: string) => void;
  /** Called once when the first byte arrives - useful for TTFT timing. */
  onTtft?: () => void;
  /** Called when a tool_call gains a function name (i.e. its open-frame). */
  onToolCallStart?: (call: ToolCall) => void;
  signal?: AbortSignal;
}

export class MtplxClient {
  private opts: StreamOptions;

  constructor(opts: StreamOptions) {
    this.opts = opts;
  }

  /** /v1/chat/completions base; used by the agent loop to derive the
   *  admin endpoint (root + /admin/sessions) for postcommit polling. */
  getBaseUrl(): string {
    return this.opts.baseUrl;
  }

  /** Effective session id (caller-supplied or auto-generated). The
   *  agent loop uses this to look up its own session in /admin/sessions. */
  getSessionId(): string | undefined {
    return this.opts.sessionId;
  }

  async stream(
    messages: ChatMessage[],
    tools: ToolSpec[] | undefined,
    sampling: SamplingOpts,
    signal?: AbortSignal,
  ): Promise<CompletionResult> {
    const sessionId = this.opts.sessionId ?? generateAutoSessionId();
    const url = `${this.opts.baseUrl.replace(/\/$/, '')}/chat/completions`;
    const body = {
      model: this.opts.model,
      messages,
      ...(tools && tools.length > 0 ? { tools } : {}),
      stream: true,
      temperature: sampling.temperature,
      top_p: sampling.top_p,
      top_k: sampling.top_k,
      max_tokens: sampling.max_tokens,
    };

    const started = performance.now();
    let ttftSet = false;

    const res = await fetch(url, {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        accept: 'text/event-stream',
        'x-mtplx-session-id': sessionId,
      },
      body: JSON.stringify(body),
      signal: signal ?? this.opts.signal,
    });

    if (!res.ok || !res.body) {
      const text = await res.text().catch(() => '');
      throw new Error(`MTPLX ${res.status}: ${text.slice(0, 200)}`);
    }

    const reader = res.body.getReader();
    const decoder = new TextDecoder();
    let buffer = '';
    let content = '';
    let finishReason: string | undefined;
    let usage: SSEChunk['usage'];

    // per-index accumulator: [id, name, argsBuffer]
    const acc = new Map<number, { id: string; name: string; args: string; emitted: boolean }>();

    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      if (!ttftSet) {
        ttftSet = true;
        this.opts.onTtft?.();
      }
      buffer += decoder.decode(value, { stream: true });

      // Split on \n\n SSE record boundary, keep the partial tail.
      let split: number;
      while ((split = buffer.indexOf('\n\n')) !== -1) {
        const frame = buffer.slice(0, split);
        buffer = buffer.slice(split + 2);
        const line = frame.startsWith('data: ') ? frame.slice(6) : frame;
        if (line === '[DONE]') {
          finishReason ??= 'stop';
          break;
        }
        let chunk: SSEChunk;
        try {
          chunk = JSON.parse(line);
        } catch {
          continue;
        }
        if (chunk.usage) usage = chunk.usage;
        for (const ch of chunk.choices ?? []) {
          if (ch.finish_reason) finishReason = ch.finish_reason;
          if (ch.delta.content) {
            content += ch.delta.content;
            this.opts.onContent?.(ch.delta.content);
          }
          if (ch.delta.tool_calls) {
            for (const d of ch.delta.tool_calls) {
              const idx = d.index;
              let slot = acc.get(idx);
              if (!slot) {
                slot = { id: '', name: '', args: '', emitted: false };
                acc.set(idx, slot);
              }
              if (d.id) slot.id = d.id;
              if (d.function?.name) slot.name = d.function.name;
              if (d.function?.arguments) slot.args += d.function.arguments;
              if (!slot.emitted && slot.name) {
                slot.emitted = true;
                this.opts.onToolCallStart?.({
                  id: slot.id || `call_${idx}`,
                  type: 'function',
                  function: { name: slot.name, arguments: slot.args },
                });
              }
            }
          }
        }
      }
    }

    const tool_calls: ToolCall[] = [...acc.entries()]
      .filter(([, s]) => s.name.length > 0)
      .sort(([a], [b]) => a - b)
      .map(([idx, s]) => ({
        id: s.id || `call_${idx}`,
        type: 'function' as const,
        function: { name: s.name, arguments: s.args },
      }));

    // Recovery path: qwen3 sometimes emits the tool-call XML as plain
    // text content instead of through the structured tool_calls delta
    // channel. When that happens our acc map is empty and the model's
    // intended call is lost, the agent loop ends with no work done.
    // Scan the accumulated content for `<tool_call>` blocks and
    // recover them as structured tool_calls; strip the recovered XML
    // from content so we don't double-report it.
    if (tool_calls.length === 0 && content.includes('<tool_call>')) {
      const { recovered, strippedContent } = recoverInlineToolCalls(content);
      if (recovered.length > 0) {
        for (const tc of recovered) tool_calls.push(tc);
        content = strippedContent;
      }
    }

    return {
      content,
      tool_calls,
      finish_reason: finishReason,
      usage,
      totalMs: performance.now() - started,
    };
  }
}

/** Recover tool_call entries that the model emitted as plain text
 *  content. Tolerates several qwen3 emission shapes:
 *
 *    <tool_call><function=NAME>{ARGS_JSON}</function></tool_call>
 *    <tool_call><function=NAME></tool_call>                       (no-arg tools)
 *    <tool_call>{"name":"NAME","arguments":{...}}</tool_call>
 *    <tool_call>{"name":"NAME"}</tool_call>
 *
 *  Returns the parsed calls + the content with all recovered XML
 *  stripped (so callers don't double-render it).
 *
 *  Exported for testing. */
export function recoverInlineToolCalls(content: string): {
  recovered: ToolCall[];
  strippedContent: string;
} {
  const recovered: ToolCall[] = [];
  const stripRanges: [number, number][] = [];
  // Capture each <tool_call>...</tool_call> block. The body is matched
  // non-greedily; we'll parse the body separately. \s* allows the
  // newlines qwen3 typically inserts.
  const blockRe = /<tool_call>\s*([\s\S]*?)\s*<\/tool_call>/g;
  let m: RegExpExecArray | null;
  while ((m = blockRe.exec(content)) !== null) {
    const body = m[1] ?? '';
    const parsed = parseToolCallBody(body);
    if (parsed) {
      recovered.push({
        id: `recovered_${recovered.length}`,
        type: 'function',
        function: { name: parsed.name, arguments: parsed.args },
      });
      stripRanges.push([m.index, m.index + m[0].length]);
    }
  }
  if (stripRanges.length === 0) return { recovered, strippedContent: content };
  // Remove from end to start so indices stay valid.
  let out = content;
  for (let i = stripRanges.length - 1; i >= 0; i--) {
    const [s, e] = stripRanges[i]!;
    out = out.slice(0, s) + out.slice(e);
  }
  return { recovered, strippedContent: out.trim() };
}

function parseToolCallBody(body: string): { name: string; args: string } | undefined {
  // Shape A: <function=NAME>{ARGS}</function> or <function=NAME></function>
  const fnTag = /<function=([A-Za-z_][\w-]*)>([\s\S]*?)(?:<\/function>|$)/;
  const tag = fnTag.exec(body);
  if (tag) {
    const name = tag[1] ?? '';
    let argsRaw = (tag[2] ?? '').trim();
    if (!argsRaw) argsRaw = '{}';
    // Make sure args parses as JSON; if not, wrap.
    try {
      JSON.parse(argsRaw);
    } catch {
      argsRaw = JSON.stringify({ _raw: argsRaw });
    }
    return { name, args: argsRaw };
  }
  // Shape B: JSON object {"name": ..., "arguments": ...}
  const jsonStart = body.indexOf('{');
  if (jsonStart !== -1) {
    const jsonText = body.slice(jsonStart);
    try {
      const obj = JSON.parse(jsonText) as { name?: unknown; arguments?: unknown };
      if (typeof obj.name === 'string') {
        const args =
          typeof obj.arguments === 'string'
            ? obj.arguments
            : JSON.stringify(obj.arguments ?? {});
        return { name: obj.name, args };
      }
    } catch {
      /* not JSON; fall through */
    }
  }
  return undefined;
}
