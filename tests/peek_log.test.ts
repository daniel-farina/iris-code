// peek_log tool test - reads recent sessions, supports failed-only filter.

import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

let tmp: string;

beforeEach(() => {
  tmp = mkdtempSync(join(tmpdir(), 'hip-peek-'));
  process.env.HIP_HOME = tmp;
});
afterEach(() => {
  rmSync(tmp, { recursive: true, force: true });
  delete process.env.HIP_HOME;
});

describe('peek_log tool', () => {
  it('reports empty log gracefully', async () => {
    const peek = await import('../src/tools/peek_log.js?fresh=' + Date.now());
    const out = await peek.peekLogTool.run({});
    expect(out).toMatch(/no sessions/);
  });

  it('lists recent sessions with metadata', async () => {
    const store = await import('../src/session_store.js?fresh=' + Date.now());
    const peek = await import('../src/tools/peek_log.js?fresh=' + Date.now());
    await store.appendSession({
      session_id: 'a1',
      ts_unix: 100,
      cwd: '/tmp',
      first_user: 'how do I list files',
      conv: [
        { role: 'user', content: 'how do I list files' },
        { role: 'assistant', content: 'use list' },
      ],
    });
    const out = await peek.peekLogTool.run({ n: 5 });
    expect(out).toMatch(/a1/);
    expect(out).toMatch(/msgs= +2/);
    expect(out).toMatch(/how do I list files/);
  });

  it('failed=true filters to error-looking sessions', async () => {
    const store = await import('../src/session_store.js?fresh=' + Date.now());
    const peek = await import('../src/tools/peek_log.js?fresh=' + Date.now());
    await store.appendSession({
      session_id: 'good',
      ts_unix: 1,
      cwd: '/tmp',
      first_user: 'q1',
      conv: [{ role: 'assistant', content: 'all good' }],
    });
    await store.appendSession({
      session_id: 'bad',
      ts_unix: 2,
      cwd: '/tmp',
      first_user: 'q2',
      conv: [{ role: 'assistant', content: 'error: cannot find module' }],
    });
    const out = await peek.peekLogTool.run({ failed: true });
    expect(out).toMatch(/bad/);
    expect(out).not.toMatch(/^.*\bgood\b.*$/m);
  });
});
