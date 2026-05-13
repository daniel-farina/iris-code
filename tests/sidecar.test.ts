// Sidecar client tests. Mocks fetch so we don't need a real Ollama.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { buildExchangeText, summarizeExchange } from '../src/sidecar.js';

const cfg = { url: 'http://localhost:11434', model: 'gemma4:e2b' };

const realFetch = globalThis.fetch;

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
});

afterEach(() => {
  vi.useRealTimers();
  globalThis.fetch = realFetch;
});

describe('summarizeExchange', () => {
  it('returns a trimmed sentence on success', async () => {
    globalThis.fetch = vi.fn(async () =>
      new Response(
        JSON.stringify({ message: { content: '  Edited ai.js to spawn 3 cars.  ' } }),
        { status: 200, headers: { 'content-type': 'application/json' } },
      ),
    ) as typeof fetch;
    const r = await summarizeExchange(cfg, 'fake exchange');
    expect(r).toBe('Edited ai.js to spawn 3 cars.');
  });

  it('strips surrounding quotes', async () => {
    globalThis.fetch = vi.fn(async () =>
      new Response(JSON.stringify({ message: { content: '"some quoted line"' } }), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      }),
    ) as typeof fetch;
    const r = await summarizeExchange(cfg, 'fake');
    expect(r).toBe('some quoted line');
  });

  it('returns null on HTTP error', async () => {
    globalThis.fetch = vi.fn(async () =>
      new Response('boom', { status: 500 }),
    ) as typeof fetch;
    const r = await summarizeExchange(cfg, 'fake');
    expect(r).toBeNull();
  });

  it('returns null on empty content', async () => {
    globalThis.fetch = vi.fn(async () =>
      new Response(JSON.stringify({ message: { content: '' } }), { status: 200 }),
    ) as typeof fetch;
    const r = await summarizeExchange(cfg, 'fake');
    expect(r).toBeNull();
  });

  it('returns null on network throw', async () => {
    globalThis.fetch = vi.fn(async () => {
      throw new Error('connection refused');
    }) as typeof fetch;
    const r = await summarizeExchange(cfg, 'fake');
    expect(r).toBeNull();
  });

  it('respects the caller AbortSignal', async () => {
    globalThis.fetch = vi.fn(async (_: unknown, init?: { signal?: AbortSignal }) => {
      // Wait for abort
      await new Promise<void>((_resolve, reject) => {
        init?.signal?.addEventListener('abort', () => reject(new Error('aborted')), {
          once: true,
        });
      });
      return new Response('never', { status: 200 });
    }) as typeof fetch;
    const ctrl = new AbortController();
    setTimeout(() => ctrl.abort(), 10);
    const r = await summarizeExchange(cfg, 'fake', ctrl.signal);
    expect(r).toBeNull();
  });
});

describe('buildExchangeText', () => {
  it('serializes role tags and clamps long content', () => {
    const text = buildExchangeText([
      { role: 'user', content: 'add a thing' },
      { role: 'assistant', content: 'x'.repeat(3000) },
    ]);
    expect(text).toContain('<user>add a thing</user>');
    expect(text).toContain('<assistant>');
    // 2000-char clamp applies per message
    expect(text.length).toBeLessThan(2500);
  });

  it('JSON-stringifies non-string content', () => {
    const text = buildExchangeText([
      { role: 'tool', content: ['a', 'b'] as unknown as string },
    ]);
    expect(text).toContain('<tool>["a","b"]</tool>');
  });
});
