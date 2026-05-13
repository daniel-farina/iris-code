// Tests for compactConv() - the in-loop conversation summarizer used
// by both --print and the TUI's /compact path. The key invariants:
//
//   1. Sanitized tail: the resulting conv must be a valid OpenAI chat
//      sequence; specifically, no orphan `role: tool` messages and no
//      assistant tool_calls without matching tool results in the tail.
//      MTPLX 422s otherwise.
//   2. Partial-vs-full: when there's enough history to summarize, keep
//      the recent N messages verbatim; otherwise fall back to full
//      compaction.
//   3. The new conv always starts with [system, user(summary), assistant(ack)].

import { describe, expect, it } from 'vitest';
import type { MtplxClient } from '../src/client.js';
import { compactConv } from '../src/compact.js';
import type { ChatMessage, ToolCall } from '../src/schema.js';

const sampling = { temperature: 0, top_p: 1, top_k: 1, max_tokens: 16 };

function fakeClient(summary = '## test\n- a\n- b'): MtplxClient {
  return {
    stream: async () => ({
      content: summary,
      tool_calls: [],
      finish_reason: 'stop',
    }),
  } as unknown as MtplxClient;
}

function tc(id: string, name = 'list'): ToolCall {
  return { id, type: 'function', function: { name, arguments: '{}' } };
}

describe('compactConv', () => {
  it('returns null for very short convs', async () => {
    const conv: ChatMessage[] = [
      { role: 'system', content: 'sys' },
      { role: 'user', content: 'hi' },
    ];
    const r = await compactConv({ client: fakeClient(), conv, sampling });
    expect(r).toBeNull();
  });

  it('keeps the last 6 messages verbatim by default (partial compaction)', async () => {
    const conv: ChatMessage[] = [
      { role: 'system', content: 'sys' },
      { role: 'user', content: 'u1' },
      { role: 'assistant', content: 'a1' },
      { role: 'user', content: 'u2' },
      { role: 'assistant', content: 'a2' },
      { role: 'user', content: 'u3' },
      { role: 'assistant', content: 'a3' },
      { role: 'user', content: 'u4' },
      { role: 'assistant', content: 'a4' },
      { role: 'user', content: 'u5' },
      { role: 'assistant', content: 'a5' },
    ];
    const r = await compactConv({ client: fakeClient(), conv, sampling });
    expect(r).not.toBeNull();
    // New conv = [system, user(summary), assistant(ack)] + tail(6)
    expect(r?.newConv.length).toBe(9);
    expect(r?.newConv[1]?.content).toContain('Compacted session summary');
    // Tail should be the last 6 from original conv.
    const tailContent = r?.newConv
      .slice(3)
      .map((m) => (typeof m.content === 'string' ? m.content : ''))
      .join('|');
    expect(tailContent).toContain('u3');
    expect(tailContent).toContain('a5');
  });

  it('drops orphan tool messages at the start of the tail', async () => {
    // Tail starts with a tool result whose tool_call_id (c-old) was in
    // the summarized head. Without sanitization MTPLX would 422.
    const conv: ChatMessage[] = [
      { role: 'system', content: 'sys' },
      { role: 'user', content: 'u1' },
      { role: 'assistant', content: '', tool_calls: [tc('c-old')] },
      { role: 'tool', tool_call_id: 'c-old', name: 'list', content: 'old-result' },
      { role: 'user', content: 'u2' },
      { role: 'assistant', content: '', tool_calls: [tc('c-new')] },
      // tail starts here (last 6) - but the first item is an ORPHAN
      // tool result if the tool_call message for c-old got summarized.
      { role: 'tool', tool_call_id: 'c-old', name: 'list', content: 'orphan' },
      { role: 'user', content: 'u3' },
      { role: 'assistant', content: '', tool_calls: [tc('c-new2')] },
      { role: 'tool', tool_call_id: 'c-new2', name: 'list', content: 'fresh' },
      { role: 'assistant', content: 'final' },
    ];
    const r = await compactConv({ client: fakeClient(), conv, sampling });
    expect(r).not.toBeNull();
    // The first message in the post-summary tail must NOT be a role:tool.
    const tailStart = r?.newConv[3];
    expect(tailStart?.role).not.toBe('tool');
  });

  it('falls back to full compaction when there is not enough head to summarize', async () => {
    // 5 total messages, keepTail=6 → no head, but min head=3 so it
    // falls back to summarizing everything.
    const conv: ChatMessage[] = [
      { role: 'system', content: 'sys' },
      { role: 'user', content: 'u1' },
      { role: 'assistant', content: 'a1' },
      { role: 'user', content: 'u2' },
      { role: 'assistant', content: 'a2' },
    ];
    const r = await compactConv({ client: fakeClient(), conv, sampling });
    expect(r).not.toBeNull();
    // Fallback path: [system, user(summary), assistant(ack)] with NO tail.
    expect(r?.newConv.length).toBe(3);
  });

  it('returns null when the model produces an empty summary', async () => {
    const conv: ChatMessage[] = [
      { role: 'system', content: 'sys' },
      { role: 'user', content: 'u1' },
      { role: 'assistant', content: 'a1' },
      { role: 'user', content: 'u2' },
      { role: 'assistant', content: 'a2' },
      { role: 'user', content: 'u3' },
      { role: 'assistant', content: 'a3' },
    ];
    const r = await compactConv({ client: fakeClient(''), conv, sampling });
    expect(r).toBeNull();
  });

  it('approxTokens grows with conv content size', async () => {
    const { approxTokens } = await import('../src/compact.js');
    const small = [{ role: 'user' as const, content: 'hi' }];
    const big = [{ role: 'user' as const, content: 'x'.repeat(40000) }];
    expect(approxTokens(big)).toBeGreaterThan(approxTokens(small));
  });
});
