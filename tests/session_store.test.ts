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

  it('lastSessionForCwd filters by cwd', async () => {
    const mod = await import('../src/session_store.js?fresh=' + Date.now());
    await mod.appendSession({
      session_id: 'p1-a',
      ts_unix: 1,
      cwd: '/tmp/project-a',
      first_user: 'a1',
      conv: [{ role: 'user', content: 'a1' }],
    });
    await mod.appendSession({
      session_id: 'p2-a',
      ts_unix: 2,
      cwd: '/tmp/project-b',
      first_user: 'b1',
      conv: [{ role: 'user', content: 'b1' }],
    });
    await mod.appendSession({
      session_id: 'p1-b',
      ts_unix: 3,
      cwd: '/tmp/project-a',
      first_user: 'a2',
      conv: [{ role: 'user', content: 'a2' }],
    });
    // Global last is project-b session (most recent overall).
    expect((await mod.lastSession())?.session_id).toBe('p1-b');
    // Cwd-scoped picks the latest in /tmp/project-a (p1-b), even though
    // a more recent session exists in project-b.
    expect((await mod.lastSessionForCwd('/tmp/project-a'))?.session_id).toBe('p1-b');
    expect((await mod.lastSessionForCwd('/tmp/project-b'))?.session_id).toBe('p2-a');
    // No match → undefined.
    expect(await mod.lastSessionForCwd('/tmp/never-used')).toBeUndefined();
  });

  it('round-trips running_summary across persist + read', async () => {
    const mod = await import('../src/session_store.js?fresh=' + Date.now());
    await mod.updateSession({
      session_id: 'rs-1',
      ts_unix: 1,
      cwd: '/tmp/proj',
      first_user: 'first ask',
      conv: [{ role: 'user', content: 'hi' }],
      running_summary: ['Created src/foo.js', 'Wired foo into main.js'],
    });
    const rec = await mod.findSession('rs-1');
    expect(rec?.running_summary).toEqual([
      'Created src/foo.js',
      'Wired foo into main.js',
    ]);
  });

  it('omitted running_summary stays undefined on read', async () => {
    const mod = await import('../src/session_store.js?fresh=' + Date.now());
    await mod.updateSession({
      session_id: 'no-rs',
      ts_unix: 1,
      cwd: '/tmp/proj',
      first_user: '',
      conv: [{ role: 'user', content: 'hi' }],
    });
    const rec = await mod.findSession('no-rs');
    expect(rec?.running_summary).toBeUndefined();
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
