// session_store roundtrip tests. Uses HIP_HOME=tmpdir so we don't
// pollute the real ~/.hip.

import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

let tmp: string;

beforeEach(() => {
  tmp = mkdtempSync(join(tmpdir(), 'hip-session-'));
  process.env.HIP_HOME = tmp;
});
afterEach(() => {
  rmSync(tmp, { recursive: true, force: true });
  delete process.env.HIP_HOME;
});

describe('session_store', () => {
  it('returns empty list before any writes', async () => {
    // Fresh import so HIP_HOME is picked up.
    const mod = await import('../src/session_store.js?fresh=' + Date.now());
    const all = await mod.readAllSessions();
    expect(all).toEqual([]);
    expect(await mod.lastSession()).toBeUndefined();
  });

  it('appendSession then lastSession returns the most recent', async () => {
    const mod = await import('../src/session_store.js?fresh=' + Date.now());
    await mod.appendSession({
      session_id: 'auto-1',
      ts_unix: 1,
      cwd: '/tmp',
      first_user: 'hi',
      conv: [{ role: 'user', content: 'hi' }],
    });
    await mod.appendSession({
      session_id: 'auto-2',
      ts_unix: 2,
      cwd: '/tmp',
      first_user: 'bye',
      conv: [{ role: 'user', content: 'bye' }],
    });
    const last = await mod.lastSession();
    expect(last?.session_id).toBe('auto-2');
  });

  it('findSession returns latest record for an id with duplicates', async () => {
    const mod = await import('../src/session_store.js?fresh=' + Date.now());
    await mod.appendSession({
      session_id: 'dup',
      ts_unix: 1,
      cwd: '/tmp',
      first_user: 'v1',
      conv: [{ role: 'user', content: 'v1' }],
    });
    await mod.appendSession({
      session_id: 'dup',
      ts_unix: 2,
      cwd: '/tmp',
      first_user: 'v2',
      conv: [{ role: 'user', content: 'v2' }],
    });
    const rec = await mod.findSession('dup');
    expect(rec?.first_user).toBe('v2');
  });

  it('updateSession overwrites the matching record', async () => {
    const mod = await import('../src/session_store.js?fresh=' + Date.now());
    await mod.appendSession({
      session_id: 'a',
      ts_unix: 1,
      cwd: '/tmp',
      first_user: 'one',
      conv: [{ role: 'user', content: 'one' }],
    });
    await mod.updateSession({
      session_id: 'a',
      ts_unix: 2,
      cwd: '/tmp',
      first_user: 'one-updated',
      conv: [
        { role: 'user', content: 'one' },
        { role: 'assistant', content: 'two' },
      ],
    });
    const rec = await mod.findSession('a');
    expect(rec?.first_user).toBe('one-updated');
    expect(rec?.conv).toHaveLength(2);
  });
});
