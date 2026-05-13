// Sidecar client tests. Mocks fetch so we don't need a real Ollama.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  autoDetectSidecar,
  buildExchangeText,
  probeSidecar,
  summarizeExchange,
} from '../src/sidecar.js';

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

describe('autoDetectSidecar', () => {
  it('returns the first preferred model that is installed', async () => {
    globalThis.fetch = vi.fn(async () =>
      new Response(
        JSON.stringify({
          models: [
            { name: 'qwen3.6:35b-a3b' },
            { name: 'gemma4:e2b' },
            { name: 'gemma3:1b' },
          ],
        }),
        { status: 200 },
      ),
    ) as typeof fetch;
    const r = await autoDetectSidecar('http://localhost:11434');
    // gemma4:e4b isn't installed; gemma4:e2b is next in the preference
    // list and IS installed.
    expect(r?.model).toBe('gemma4:e2b');
  });

  it('returns null if no preferred model is installed', async () => {
    globalThis.fetch = vi.fn(async () =>
      new Response(JSON.stringify({ models: [{ name: 'qwen3.6:35b-a3b' }] }), { status: 200 }),
    ) as typeof fetch;
    const r = await autoDetectSidecar('http://localhost:11434');
    expect(r).toBeNull();
  });

  it('returns null when Ollama unreachable', async () => {
    globalThis.fetch = vi.fn(async () => {
      throw new Error('ECONNREFUSED');
    }) as typeof fetch;
    const r = await autoDetectSidecar('http://localhost:11434');
    expect(r).toBeNull();
  });

  it('respects a custom preferred list', async () => {
    globalThis.fetch = vi.fn(async () =>
      new Response(JSON.stringify({ models: [{ name: 'phi4:mini' }, { name: 'gemma4:e2b' }] }), {
        status: 200,
      }),
    ) as typeof fetch;
    const r = await autoDetectSidecar('http://localhost:11434', ['phi4:mini']);
    expect(r?.model).toBe('phi4:mini');
  });
});

describe('probeSidecar', () => {
  it('returns ok with the first preferred model installed', async () => {
    globalThis.fetch = vi.fn(async () =>
      new Response(JSON.stringify({ models: [{ name: 'gemma4:e2b' }] }), { status: 200 }),
    ) as typeof fetch;
    const r = await probeSidecar('http://localhost:11434');
    expect(r.kind).toBe('ok');
    if (r.kind === 'ok') expect(r.config.model).toBe('gemma4:e2b');
  });

  it('returns no-model when Ollama up but nothing preferred installed', async () => {
    globalThis.fetch = vi.fn(async () =>
      new Response(JSON.stringify({ models: [{ name: 'codellama:13b' }] }), { status: 200 }),
    ) as typeof fetch;
    const r = await probeSidecar('http://localhost:11434');
    expect(r.kind).toBe('no-model');
    if (r.kind === 'no-model') {
      expect(r.installed).toContain('codellama:13b');
      expect(r.preferred[0]).toBe('gemma4:e4b');
    }
  });

  it('returns no-ollama when fetch throws', async () => {
    globalThis.fetch = vi.fn(async () => {
      throw new Error('ECONNREFUSED');
    }) as typeof fetch;
    const r = await probeSidecar('http://localhost:11434');
    expect(r.kind).toBe('no-ollama');
  });

  it('returns no-ollama on non-2xx HTTP status', async () => {
    globalThis.fetch = vi.fn(async () => new Response('', { status: 503 })) as typeof fetch;
    const r = await probeSidecar('http://localhost:11434');
    expect(r.kind).toBe('no-ollama');
    if (r.kind === 'no-ollama') expect(r.reason).toContain('503');
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
