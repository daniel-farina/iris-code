// Client streaming test: spin up a fake SSE server that emits two
// consecutive tool_calls in chunked frames and verify the client
// accumulates them in order. Mirrors what we'd hit against real MTPLX
// (post-v0.3.4 streaming parser fix) but doesn't need MTPLX to run.

import { describe, expect, it } from 'vitest';
import { createServer } from 'node:http';
import { MtplxClient } from '../src/client.js';

describe('MtplxClient', () => {
  it('accumulates two consecutive tool_calls by index', async () => {
    const frames = [
      '{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"tree","arguments":""}}]}}]}',
      '{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\\"depth\\":2}"}}]}}]}',
      '{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"b","function":{"name":"list","arguments":""}}]}}]}',
      '{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\\"path\\":\\"src\\"}"}}]}}]}',
      '{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}',
    ];
    const port = await startSseServer(frames);
    const client = new MtplxClient({
      baseUrl: `http://127.0.0.1:${port}/v1`,
      model: 'm',
      sessionId: 'test',
    });
    const res = await client.stream(
      [{ role: 'user', content: 'hi' }],
      undefined,
      { temperature: 0, top_p: 1, top_k: 1, max_tokens: 16 },
    );
    expect(res.finish_reason).toBe('tool_calls');
    expect(res.tool_calls).toHaveLength(2);
    expect(res.tool_calls[0]?.function.name).toBe('tree');
    expect(JSON.parse(res.tool_calls[0]!.function.arguments)).toEqual({ depth: 2 });
    expect(res.tool_calls[1]?.function.name).toBe('list');
    expect(JSON.parse(res.tool_calls[1]!.function.arguments)).toEqual({ path: 'src' });
  });

  it('aborts in-flight stream when AbortSignal fires', async () => {
    // Server that streams forever; we abort partway and confirm the
    // stream() call rejects with AbortError (or returns early per agent
    // loop semantics). The agent loop catches AbortError and treats it
    // as a cancel - here we just verify the client honors the signal.
    const port = await startInfiniteSseServer();
    const ctl = new AbortController();
    const client = new MtplxClient({
      baseUrl: `http://127.0.0.1:${port}/v1`,
      model: 'm',
      sessionId: 'test',
    });
    setTimeout(() => ctl.abort(), 20);
    let threw = false;
    try {
      await client.stream(
        [{ role: 'user', content: 'hi' }],
        undefined,
        { temperature: 0, top_p: 1, top_k: 1, max_tokens: 16 },
        ctl.signal,
      );
    } catch (e) {
      threw = true;
      // Either AbortError on the fetch, or an abort-typed Error from
      // our reader. Both are acceptable signals of clean cancellation.
      const name = (e as Error).name ?? '';
      expect(['AbortError', 'Error'].includes(name)).toBe(true);
    }
    expect(threw).toBe(true);
  });

  it('streams content deltas through onContent', async () => {
    const frames = [
      '{"choices":[{"index":0,"delta":{"content":"hello "}}]}',
      '{"choices":[{"index":0,"delta":{"content":"world"}}]}',
      '{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}',
    ];
    const port = await startSseServer(frames);
    let buf = '';
    const client = new MtplxClient({
      baseUrl: `http://127.0.0.1:${port}/v1`,
      model: 'm',
      sessionId: 'test',
      onContent: (c) => {
        buf += c;
      },
    });
    const res = await client.stream(
      [{ role: 'user', content: 'hi' }],
      undefined,
      { temperature: 0, top_p: 1, top_k: 1, max_tokens: 16 },
    );
    expect(buf).toBe('hello world');
    expect(res.content).toBe('hello world');
    expect(res.finish_reason).toBe('stop');
  });
});

async function startInfiniteSseServer(): Promise<number> {
  return new Promise<number>((resolve) => {
    const server = createServer((_req, res) => {
      res.writeHead(200, {
        'content-type': 'text/event-stream',
        'cache-control': 'no-cache',
      });
      // Emit content forever (well, until client disconnects).
      const handle = setInterval(() => {
        try {
          res.write(`data: {"choices":[{"index":0,"delta":{"content":"x"}}]}\n\n`);
        } catch {
          clearInterval(handle);
        }
      }, 5);
      res.on('close', () => {
        clearInterval(handle);
        setTimeout(() => server.close(), 5);
      });
    });
    server.listen(0, '127.0.0.1', () => {
      const addr = server.address();
      if (addr && typeof addr === 'object') resolve(addr.port);
    });
  });
}

async function startSseServer(frames: string[]): Promise<number> {
  return new Promise<number>((resolve) => {
    const server = createServer((_req, res) => {
      res.writeHead(200, {
        'content-type': 'text/event-stream',
        'cache-control': 'no-cache',
      });
      let i = 0;
      const tick = () => {
        if (i < frames.length) {
          res.write(`data: ${frames[i]}\n\n`);
          i++;
          setTimeout(tick, 1);
        } else {
          res.write('data: [DONE]\n\n');
          res.end();
          // Close the server so the test process can exit cleanly.
          setTimeout(() => server.close(), 5);
        }
      };
      tick();
    });
    server.listen(0, '127.0.0.1', () => {
      const addr = server.address();
      if (addr && typeof addr === 'object') resolve(addr.port);
    });
  });
}
