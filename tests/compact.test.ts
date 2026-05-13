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
    expect(r?.newConv[1]?.content).toContain('Compacted background context');
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

  it('shrinks the tail when it exceeds keepTailMaxTokens', async () => {
    // 6 recent messages, two of them are huge tool spam. Use a
    // 500-token tail budget so all the huge ones must be dropped.
    const huge = 'x'.repeat(8000); // ~2000 tokens each
    const conv: ChatMessage[] = [
      { role: 'system', content: 'sys' },
      { role: 'user', content: 'u1' },
      { role: 'assistant', content: 'a1' },
      { role: 'user', content: 'u2' },
      { role: 'assistant', content: 'a2' },
      { role: 'user', content: 'u3' },
      { role: 'assistant', content: huge },
      { role: 'user', content: 'u4' },
      { role: 'assistant', content: huge },
      { role: 'user', content: 'u5-recent' },
      { role: 'assistant', content: 'small recent' },
    ];
    const r = await compactConv({
      client: fakeClient(),
      conv,
      sampling,
      systemPrompt: 'sys',
      keepTailMaxTokens: 500,
    });
    expect(r).not.toBeNull();
    // The most-recent message must always survive.
    const last = r?.newConv[r.newConv.length - 1];
    expect(last?.content).toBe('small recent');
    // None of the kept tail should be one of the huge tool-spam messages.
    const tailContent =
      r?.newConv
        .slice(3)
        .map((m) => (typeof m.content === 'string' ? m.content : ''))
        .join('|') ?? '';
    expect(tailContent.includes(huge)).toBe(false);
  });

  it('always keeps the most-recent message when budget is tiny', async () => {
    const conv: ChatMessage[] = [
      { role: 'system', content: 'sys' },
      { role: 'user', content: 'u1' },
      { role: 'assistant', content: 'a1' },
      { role: 'user', content: 'u2' },
      { role: 'assistant', content: 'a2' },
      { role: 'user', content: 'u3' },
      { role: 'assistant', content: 'a3' },
      { role: 'user', content: 'u4-recent' },
      { role: 'assistant', content: 'a4-recent' },
    ];
    const r = await compactConv({
      client: fakeClient(),
      conv,
      sampling,
      systemPrompt: 'sys',
      keepTailMaxTokens: 1, // effectively zero budget
    });
    expect(r).not.toBeNull();
    // Must end with the most-recent message even when budget would
    // force a fully empty tail.
    const last = r?.newConv[r.newConv.length - 1];
    expect(last?.content).toBe('a4-recent');
  });

  it('uses runningSummary when provided (skips main-model summarize)', async () => {
    // Make a client that THROWS if called - to prove we don't hit it.
    const noNetClient = {
      stream: async () => {
        throw new Error('main model should not be called when runningSummary is given');
      },
    } as unknown as MtplxClient;
    const conv: ChatMessage[] = [
      { role: 'system', content: 'sys' },
      { role: 'user', content: 'u1' },
      { role: 'assistant', content: 'a1' },
      { role: 'user', content: 'u2' },
      { role: 'assistant', content: 'a2' },
      { role: 'user', content: 'u3' },
      { role: 'assistant', content: 'a3' },
      { role: 'user', content: 'u4-recent' },
      { role: 'assistant', content: 'a4-recent' },
    ];
    const r = await compactConv({
      client: noNetClient,
      conv,
      sampling,
      systemPrompt: 'sys',
      runningSummary: ['edited ai.js to spawn 3 cars', 'fixed wall clipping in physics.js'],
    });
    expect(r).not.toBeNull();
    expect(r?.summary).toContain('edited ai.js to spawn 3 cars');
    expect(r?.summary).toContain('fixed wall clipping in physics.js');
  });

  it('falls back to main-model summarize when runningSummary is empty', async () => {
    let calls = 0;
    const client = {
      stream: async () => {
        calls++;
        return { content: 'fallback summary', tool_calls: [], finish_reason: 'stop' };
      },
    } as unknown as MtplxClient;
    const conv: ChatMessage[] = [
      { role: 'system', content: 'sys' },
      { role: 'user', content: 'u1' },
      { role: 'assistant', content: 'a1' },
      { role: 'user', content: 'u2' },
      { role: 'assistant', content: 'a2' },
      { role: 'user', content: 'u3' },
      { role: 'assistant', content: 'a3' },
    ];
    const r = await compactConv({
      client,
      conv,
      sampling,
      systemPrompt: 'sys',
      runningSummary: [], // empty - should call main model
    });
    expect(r).not.toBeNull();
    expect(calls).toBeGreaterThan(0);
  });

  it('reinjects latest user msg when tool calls pushed it out of the tail', async () => {
    // Simulate the leaderboard-test failure mode: the user's prompt is
    // followed by 8 tool-call/tool-result messages, so tail-of-6 doesn't
    // contain the user msg anymore. After compact the user msg should
    // be reinjected at the start of the post-summary tail.
    const conv: ChatMessage[] = [
      { role: 'system', content: 'sys' },
      { role: 'user', content: 'old1' },
      { role: 'assistant', content: 'a1' },
      { role: 'user', content: 'old2' },
      { role: 'assistant', content: 'a2' },
      // THE NEW USER PROMPT
      { role: 'user', content: 'NEW: add a leaderboard with localStorage' },
      // followed by tool calls...
      { role: 'assistant', content: '', tool_calls: [{ id: 'c1', type: 'function', function: { name: 'read', arguments: '{}' } }] },
      { role: 'tool', tool_call_id: 'c1', name: 'read', content: 'output1' },
      { role: 'assistant', content: '', tool_calls: [{ id: 'c2', type: 'function', function: { name: 'read', arguments: '{}' } }] },
      { role: 'tool', tool_call_id: 'c2', name: 'read', content: 'output2' },
      { role: 'assistant', content: '', tool_calls: [{ id: 'c3', type: 'function', function: { name: 'grep', arguments: '{}' } }] },
      { role: 'tool', tool_call_id: 'c3', name: 'grep', content: 'output3' },
      { role: 'assistant', content: 'investigating...' },
    ];
    const r = await compactConv({
      client: fakeClient(),
      conv,
      sampling,
      systemPrompt: 'sys',
    });
    expect(r).not.toBeNull();
    // Find the reinjected user message in the new conv. It should be
    // in there even though it's NOT in the natural last-6-tail anymore.
    const userMsgs = r?.newConv
      .filter((m) => m.role === 'user')
      .map((m) => (typeof m.content === 'string' ? m.content : ''));
    expect(userMsgs?.some((c) => c.includes('add a leaderboard with localStorage'))).toBe(true);
  });

  it('approxTokens grows with conv content size', async () => {
    const { approxTokens } = await import('../src/compact.js');
    const small = [{ role: 'user' as const, content: 'hi' }];
    const big = [{ role: 'user' as const, content: 'x'.repeat(40000) }];
    expect(approxTokens(big)).toBeGreaterThan(approxTokens(small));
  });
});
