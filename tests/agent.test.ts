// Agent loop unit tests. Uses a fake client that returns canned
// CompletionResult sequences so we can exercise tool-dispatch, length
// auto-continue, abort, and max_rounds termination without touching a
// real MTPLX server.

import { describe, it, expect } from 'vitest';
import { runLoop } from '../src/agent.js';
import type { MtplxClient } from '../src/client.js';
import type { CompletionResult, ChatMessage, ToolCall } from '../src/schema.js';

function fakeClient(scripted: CompletionResult[]): MtplxClient {
  let i = 0;
  return {
    stream: async () => {
      const r = scripted[i++];
      if (!r) throw new Error(`fakeClient: ran out of scripted responses at call ${i}`);
      return r;
    },
  } as unknown as MtplxClient;
}

function toolCall(id: string, name: string, args: Record<string, unknown>): ToolCall {
  return { id, type: 'function', function: { name, arguments: JSON.stringify(args) } };
}

const sampling = { temperature: 0, top_p: 1, top_k: 1, max_tokens: 16 };

describe('runLoop', () => {
  it('terminates after a single tool-free assistant turn (finish=stop)', async () => {
    const conv: ChatMessage[] = [{ role: 'user', content: 'hi' }];
    const client = fakeClient([
      { content: 'hello', tool_calls: [], finish_reason: 'stop' },
    ]);
    const stats = await runLoop({ client, conv, sampling });
    expect(stats.rounds).toBe(1);
    expect(stats.finishReason).toBe('stop');
    expect(stats.toolCalls).toBe(0);
    // Final assistant message appended to conv.
    expect(conv.at(-1)).toEqual({ role: 'assistant', content: 'hello' });
  });

  it('dispatches a tool call, appends the result, then completes', async () => {
    const conv: ChatMessage[] = [{ role: 'user', content: 'list src' }];
    // First round: assistant wants to call `list`. Second round: final
    // text answer that wraps the result.
    const client = fakeClient([
      {
        content: '',
        tool_calls: [toolCall('c1', 'list', { path: '/tmp' })],
        finish_reason: 'tool_calls',
      },
      { content: 'done', tool_calls: [], finish_reason: 'stop' },
    ]);
    const stats = await runLoop({ client, conv, sampling, maxRounds: 5 });
    expect(stats.rounds).toBe(2);
    expect(stats.toolCalls).toBe(1);
    expect(stats.finishReason).toBe('stop');
    // conv now has: user, assistant(tool_calls), tool(result), assistant(text)
    expect(conv.map((m) => m.role)).toEqual(['user', 'assistant', 'tool', 'assistant']);
    const toolMsg = conv[2]!;
    expect(toolMsg.role).toBe('tool');
    // The list tool either succeeded or returned an error string - either
    // way, the body should be a non-empty string back-referenced by id.
    expect(typeof toolMsg.content).toBe('string');
    expect((toolMsg.content as string).length).toBeGreaterThan(0);
  });

  it('gracefully reports unknown tool errors back to the model', async () => {
    const conv: ChatMessage[] = [{ role: 'user', content: 'do it' }];
    const client = fakeClient([
      {
        content: '',
        tool_calls: [toolCall('c1', 'nonexistent_tool', {})],
        finish_reason: 'tool_calls',
      },
      { content: 'ok', tool_calls: [], finish_reason: 'stop' },
    ]);
    const stats = await runLoop({ client, conv, sampling, maxRounds: 3 });
    expect(stats.toolCalls).toBe(1);
    const toolResult = conv.find((m) => m.role === 'tool');
    expect((toolResult?.content as string).toLowerCase()).toMatch(/unknown tool/);
  });

  it('auto-continues on finish=length up to MAX_CONSECUTIVE_LENGTH_CONTINUES', async () => {
    const conv: ChatMessage[] = [{ role: 'user', content: 'write a lot' }];
    // Three length turns then a stop.
    const client = fakeClient([
      { content: 'part1 ', tool_calls: [], finish_reason: 'length' },
      { content: 'part2 ', tool_calls: [], finish_reason: 'length' },
      { content: 'part3 ', tool_calls: [], finish_reason: 'length' },
      { content: 'done', tool_calls: [], finish_reason: 'stop' },
    ]);
    const stats = await runLoop({ client, conv, sampling, maxRounds: 10 });
    expect(stats.rounds).toBe(4);
    expect(stats.finishReason).toBe('stop');
  });

  it('caps at maxRounds when the model never stops', async () => {
    const conv: ChatMessage[] = [{ role: 'user', content: 'forever' }];
    const client = fakeClient([
      // Make each round a single tool call so we never get a stop.
      { content: '', tool_calls: [toolCall('a', 'list', { path: '/tmp' })], finish_reason: 'tool_calls' },
      { content: '', tool_calls: [toolCall('b', 'list', { path: '/tmp' })], finish_reason: 'tool_calls' },
      { content: '', tool_calls: [toolCall('c', 'list', { path: '/tmp' })], finish_reason: 'tool_calls' },
    ]);
    // Disable auto-compact - this test piles up /tmp listings which
    // can cross the 9K-token threshold and consume a scripted call
    // for the compact step.
    const stats = await runLoop({
      client,
      conv,
      sampling,
      maxRounds: 3,
      autoCompactThresholdTokens: 0,
    });
    expect(stats.rounds).toBe(3);
    expect(stats.finishReason).toBeUndefined();
    expect(stats.toolCalls).toBe(3);
  });

  it('returns finish="aborted" when the signal fires mid-flight', async () => {
    const conv: ChatMessage[] = [{ role: 'user', content: 'cancel me' }];
    const ctl = new AbortController();
    const client = {
      stream: async (_m: unknown, _t: unknown, _s: unknown, signal?: AbortSignal) => {
        // Wait briefly then check signal. Simulates an in-flight stream
        // that the runLoop aborts.
        await new Promise((r) => setTimeout(r, 5));
        if (signal?.aborted) {
          const e = new Error('aborted');
          e.name = 'AbortError';
          throw e;
        }
        return { content: 'never', tool_calls: [], finish_reason: 'stop' } satisfies CompletionResult;
      },
    } as unknown as MtplxClient;
    setTimeout(() => ctl.abort(), 1);
    const stats = await runLoop({ client, conv, sampling, signal: ctl.signal });
    expect(stats.finishReason).toBe('aborted');
  });

  it('recovers from MTPLX malformed tool_call 422 by nudging the model', async () => {
    // Simulate the qwen3 quirk: model emits bad JSON args, MTPLX 422s
    // before we see the tool_call. runLoop should inject a corrective
    // user message and retry instead of bubbling the error.
    let firstCall = true;
    const client = {
      stream: async () => {
        if (firstCall) {
          firstCall = false;
          throw new Error("MTPLX 422: malformed tool_call: assistant tool_call 'bash' arguments are not valid JSON");
        }
        return { content: 'ok', tool_calls: [], finish_reason: 'stop' } satisfies CompletionResult;
      },
    } as unknown as MtplxClient;
    const conv: ChatMessage[] = [{ role: 'user', content: 'do it' }];
    const stats = await runLoop({ client, conv, sampling, maxRounds: 5 });
    expect(stats.finishReason).toBe('stop');
    // The injected nudge should be in the conv.
    const nudge = conv.find(
      (m) => m.role === 'user' && typeof m.content === 'string' && m.content.includes('malformed JSON'),
    );
    expect(nudge).toBeDefined();
  });

  it('gives up after 2 malformed-retry attempts and rethrows', async () => {
    const client = {
      stream: async () => {
        throw new Error('MTPLX 422: malformed tool_call: not valid JSON');
      },
    } as unknown as MtplxClient;
    const conv: ChatMessage[] = [{ role: 'user', content: 'do it' }];
    await expect(runLoop({ client, conv, sampling, maxRounds: 10 })).rejects.toThrow(
      /malformed tool_call/,
    );
  });

  it('polls /admin/sessions and returns when postcommit stored=true', async () => {
    // Spin up a fake MTPLX admin endpoint that flips stored=true after
    // a short delay; agent should poll, see it, and return early
    // rather than waiting the full cap.
    const { createServer } = await import('node:http');
    let stored = false;
    setTimeout(() => {
      stored = true;
    }, 400); // postcommit "lands" at 400ms
    const server = createServer((req, res) => {
      if (req.url?.endsWith('/admin/sessions')) {
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end(
          JSON.stringify({
            sessions: [
              {
                session_id: 'test-poll-sid',
                last_postcommit_outcome: { stored, mode: stored ? 'stored' : 'in_progress' },
              },
            ],
          }),
        );
      } else {
        res.writeHead(404).end();
      }
    });
    const port = await new Promise<number>((resolve) =>
      server.listen(0, '127.0.0.1', () => {
        const addr = server.address();
        if (addr && typeof addr === 'object') resolve(addr.port);
      }),
    );

    let firstRound = true;
    const waitOutcomes: { landed: boolean; elapsedMs: number }[] = [];
    const client = {
      getBaseUrl: () => `http://127.0.0.1:${port}/v1`,
      getSessionId: () => 'test-poll-sid',
      stream: async () => {
        if (firstRound) {
          firstRound = false;
          return {
            content: '',
            tool_calls: [toolCall('c1', 'list', { path: '/tmp' })],
            finish_reason: 'tool_calls',
            usage: { prompt_tokens: 20000 },
          } satisfies CompletionResult;
        }
        return { content: 'done', tool_calls: [], finish_reason: 'stop' } satisfies CompletionResult;
      },
    } as unknown as MtplxClient;
    const conv: ChatMessage[] = [{ role: 'user', content: 'go' }];
    const t0 = Date.now();
    await runLoop({
      client,
      conv,
      sampling,
      maxRounds: 5,
      postcommitDelayMs: 5000, // cap at 5s; should return in ~400ms
      events: {
        onPostcommitDone: (landed, elapsedMs) => waitOutcomes.push({ landed, elapsedMs }),
      },
    });
    const elapsed = Date.now() - t0;
    server.close();
    expect(waitOutcomes).toHaveLength(1);
    expect(waitOutcomes[0]?.landed).toBe(true);
    expect(waitOutcomes[0]?.elapsedMs).toBeLessThan(1500); // returned soon after stored flipped
    expect(elapsed).toBeLessThan(2500); // entire run, well below the 5s cap
  });

  it('inserts a postcommit pause when prompt > threshold', async () => {
    let firstRound = true;
    const sleepCalls: number[] = [];
    const client = {
      stream: async () => {
        if (firstRound) {
          firstRound = false;
          return {
            content: '',
            tool_calls: [toolCall('c1', 'list', { path: '/tmp' })],
            finish_reason: 'tool_calls',
            usage: { prompt_tokens: 20000 },
          } satisfies CompletionResult;
        }
        return { content: 'done', tool_calls: [], finish_reason: 'stop' } satisfies CompletionResult;
      },
    } as unknown as MtplxClient;
    const conv: ChatMessage[] = [{ role: 'user', content: 'go' }];
    await runLoop({
      client,
      conv,
      sampling,
      maxRounds: 5,
      postcommitDelayMs: 25,
      events: { onPostcommitWait: (ms) => sleepCalls.push(ms) },
    });
    expect(sleepCalls).toEqual([25]);
  });

  it('skips polling entirely when postcommitDelayMs=0', async () => {
    let firstRound = true;
    const waitCalls: number[] = [];
    const client = {
      stream: async () => {
        if (firstRound) {
          firstRound = false;
          return {
            content: '',
            tool_calls: [toolCall('c1', 'list', { path: '/tmp' })],
            finish_reason: 'tool_calls',
          } satisfies CompletionResult;
        }
        return { content: 'done', tool_calls: [], finish_reason: 'stop' } satisfies CompletionResult;
      },
    } as unknown as MtplxClient;
    const conv: ChatMessage[] = [{ role: 'user', content: 'go' }];
    await runLoop({
      client,
      conv,
      sampling,
      maxRounds: 5,
      postcommitDelayMs: 0,
      events: { onPostcommitWait: (ms) => waitCalls.push(ms) },
    });
    expect(waitCalls).toEqual([]);
  });

  it('postcommit pause is interruptible via AbortSignal', async () => {
    const ctl = new AbortController();
    let firstRound = true;
    const client = {
      stream: async () => {
        if (firstRound) {
          firstRound = false;
          return {
            content: '',
            tool_calls: [toolCall('c1', 'list', { path: '/tmp' })],
            finish_reason: 'tool_calls',
            usage: { prompt_tokens: 20000 },
          } satisfies CompletionResult;
        }
        return { content: 'done', tool_calls: [], finish_reason: 'stop' } satisfies CompletionResult;
      },
    } as unknown as MtplxClient;
    const conv: ChatMessage[] = [{ role: 'user', content: 'go' }];
    setTimeout(() => ctl.abort(), 10);
    const t0 = Date.now();
    await runLoop({
      client,
      conv,
      sampling,
      maxRounds: 5,
      postcommitDelayMs: 5000,
      signal: ctl.signal,
    });
    expect(Date.now() - t0).toBeLessThan(500); // aborted, didn't wait the full 5s
  });

  it('auto-compacts mid-loop when conv crosses the token threshold', async () => {
    // Bloat the seed conv to >9K tokens via a fake earlier tool result.
    const big = 'x'.repeat(40000); // ~10K tokens at 4 char/tok estimate
    const conv: ChatMessage[] = [
      { role: 'system', content: 'sys' },
      { role: 'user', content: 'do work' },
      { role: 'assistant', content: '', tool_calls: [toolCall('c0', 'read', { path: '/' })] },
      { role: 'tool', tool_call_id: 'c0', name: 'read', content: big },
    ];
    let summarizeCalls = 0;
    let firstStreamCall = true;
    const client = {
      stream: async (msgs: ChatMessage[]) => {
        const last = msgs[msgs.length - 1];
        // The summarize step asks the model with a tools-free `user` msg
        // tail. Detect it and return a summary.
        const isSummarize =
          last?.role === 'user' &&
          typeof last.content === 'string' &&
          last.content.includes('Summarize the conversation');
        if (isSummarize) {
          summarizeCalls++;
          return {
            content: '- did stuff\n- need to keep going',
            tool_calls: [],
            finish_reason: 'stop',
          } satisfies CompletionResult;
        }
        if (firstStreamCall) {
          firstStreamCall = false;
          // After compact, conv should be small; just emit a stop.
          return {
            content: 'done',
            tool_calls: [],
            finish_reason: 'stop',
          } satisfies CompletionResult;
        }
        throw new Error('unexpected extra call');
      },
    } as unknown as MtplxClient;

    const events = { tokensBefore: 0, tokensAfter: 0 };
    await runLoop({
      client,
      conv,
      sampling,
      maxRounds: 5,
      autoCompactThresholdTokens: 9000,
      events: {
        onCompactDone: (before, after) => {
          events.tokensBefore = before;
          events.tokensAfter = after;
        },
      },
    });

    expect(summarizeCalls).toBe(1);
    expect(events.tokensBefore).toBeGreaterThan(9000);
    expect(events.tokensAfter).toBeLessThan(events.tokensBefore);
    // Conv is now the compacted shape: [system, user(summary), assistant(ack)].
    expect(conv.length).toBeLessThanOrEqual(4); // 3 from compact + maybe 1 final
  });

  it('skips auto-compact when threshold=0', async () => {
    const big = 'x'.repeat(40000);
    const conv: ChatMessage[] = [
      { role: 'system', content: 'sys' },
      { role: 'user', content: 'go' },
      { role: 'tool', tool_call_id: 'c0', name: 'read', content: big },
    ];
    let summarizeCalls = 0;
    const client = {
      stream: async (msgs: ChatMessage[]) => {
        const last = msgs[msgs.length - 1];
        if (
          last?.role === 'user' &&
          typeof last.content === 'string' &&
          last.content.includes('Summarize the conversation')
        ) {
          summarizeCalls++;
        }
        return {
          content: 'done',
          tool_calls: [],
          finish_reason: 'stop',
        } satisfies CompletionResult;
      },
    } as unknown as MtplxClient;
    await runLoop({
      client,
      conv,
      sampling,
      maxRounds: 3,
      autoCompactThresholdTokens: 0,
    });
    expect(summarizeCalls).toBe(0);
  });

  it('sanitizes malformed tool_call args so the conv stays valid for the next round', async () => {
    // qwen3 quirk: a tool_call's `arguments` string can be truncated
    // mid-value, leaving unparseable JSON in conv. Without sanitization
    // the next round 422s the whole conv. We replace bad args with a
    // stub that explains the failure to the model.
    let firstCall = true;
    const client = {
      stream: async () => {
        if (firstCall) {
          firstCall = false;
          return {
            content: '',
            tool_calls: [
              {
                id: 'c1',
                type: 'function' as const,
                function: {
                  name: 'bash',
                  // Truncated string - unterminated quote, invalid JSON.
                  arguments: '{"command":"cat > file << EOF\nlots of content',
                },
              },
            ],
            finish_reason: 'tool_calls',
          } satisfies CompletionResult;
        }
        return { content: 'ok', tool_calls: [], finish_reason: 'stop' } satisfies CompletionResult;
      },
    } as unknown as MtplxClient;
    const conv: ChatMessage[] = [{ role: 'user', content: 'do it' }];
    await runLoop({ client, conv, sampling, maxRounds: 3, postcommitDelayMs: 0 });
    // The pushed assistant message's tool_call args must now be valid JSON.
    const asst = conv.find((m) => m.role === 'assistant' && m.tool_calls);
    expect(asst).toBeDefined();
    const args = asst?.tool_calls?.[0]?.function?.arguments;
    expect(() => JSON.parse(args ?? '')).not.toThrow();
    const parsed = JSON.parse(args ?? '{}');
    expect(parsed._malformed).toBe(true);
    // And the tool result fed back to the model must explain the failure.
    const toolResult = conv.find((m) => m.role === 'tool');
    expect((toolResult?.content as string).toLowerCase()).toMatch(/truncated|smaller chunks/);
  });

  it('emits onToolStart/onToolResult/onRound events in order', async () => {
    const conv: ChatMessage[] = [{ role: 'user', content: 'go' }];
    const client = fakeClient([
      {
        content: '',
        tool_calls: [toolCall('c1', 'list', { path: '/tmp' })],
        finish_reason: 'tool_calls',
      },
      { content: 'fin', tool_calls: [], finish_reason: 'stop' },
    ]);
    const events: string[] = [];
    await runLoop({
      client,
      conv,
      sampling,
      maxRounds: 5,
      events: {
        onRound: (n) => events.push(`round:${n}`),
        onToolStart: (n) => events.push(`start:${n}`),
        onToolResult: (n, ok) => events.push(`result:${n}:${ok}`),
      },
    });
    // round 1 (with tool), tool start/result, round 2 (final).
    expect(events[0]).toBe('round:1');
    expect(events[1]).toBe('start:list');
    expect(events[2]?.startsWith('result:list:')).toBe(true);
    expect(events[3]).toBe('round:2');
  });
});
