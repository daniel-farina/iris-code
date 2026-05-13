// Stale-session clearing: pickStale is pure, clearStaleSessions hits a
// fake admin server modeled on the same shape as MTPLX's /admin/sessions.

import { createServer } from 'node:http';
import { describe, expect, it } from 'vitest';
import { clearStaleSessions, pickStale } from '../src/clear_stale.js';

describe('pickStale', () => {
  it('keeps sessions younger than the cutoff', () => {
    const r = pickStale(
      [
        { session_id: 'a', last_access_s: 60 },
        { session_id: 'b', last_access_s: 30 * 60 + 1 },
        { session_id: 'c', last_access_s: 90 * 60 },
      ],
      30 * 60,
    );
    expect(r.toClear).toEqual(['b', 'c']);
  });

  it('skips the active session id even if stale', () => {
    const r = pickStale(
      [
        { session_id: 'active', last_access_s: 3600 },
        { session_id: 'old', last_access_s: 7200 },
      ],
      30 * 60,
      'active',
    );
    expect(r.toClear).toEqual(['old']);
    expect(r.skippedActive).toEqual(['active']);
  });

  it('drops entries with no last_access_s (cannot reason about staleness)', () => {
    const r = pickStale(
      [{ session_id: 'unknown' }, { session_id: 'old', last_access_s: 99999 }],
      30 * 60,
    );
    expect(r.toClear).toEqual(['old']);
  });
});

describe('clearStaleSessions', () => {
  it('GETs /admin/sessions and POSTs /clear for each stale entry, never the active id', async () => {
    const calls: { method: string; url: string }[] = [];
    const sessions = [
      { session_id: 'live-now', last_access_s: 10 },
      { session_id: 'stale-1', last_access_s: 60 * 60 },
      { session_id: 'stale-2', last_access_s: 90 * 60 },
      { session_id: 'active', last_access_s: 99 * 60 },
    ];
    const server = createServer((req, res) => {
      calls.push({ method: req.method ?? '?', url: req.url ?? '' });
      if (req.method === 'GET' && req.url === '/admin/sessions') {
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end(JSON.stringify({ sessions }));
        return;
      }
      if (req.method === 'POST' && req.url?.match(/^\/admin\/sessions\/[^/]+\/clear$/)) {
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end('{}');
        return;
      }
      res.writeHead(404).end();
    });
    const port = await new Promise<number>((resolve) =>
      server.listen(0, '127.0.0.1', () => {
        const addr = server.address();
        if (addr && typeof addr === 'object') resolve(addr.port);
      }),
    );
    try {
      const r = await clearStaleSessions({
        v1BaseUrl: `http://127.0.0.1:${port}/v1`,
        cutoffMinutes: 30,
        activeSessionId: 'active',
      });
      expect(r.cleared.sort()).toEqual(['stale-1', 'stale-2']);
      expect(r.skippedActive).toEqual(['active']);
      expect(r.errors).toEqual([]);
      // Confirm the active id was never POSTed to.
      const clears = calls.filter((c) => c.method === 'POST').map((c) => c.url);
      expect(clears.some((u) => u.includes('active'))).toBe(false);
      expect(clears.length).toBe(2);
    } finally {
      server.close();
    }
  });

  it('records HTTP errors per session without aborting the run', async () => {
    const server = createServer((req, res) => {
      if (req.method === 'GET' && req.url === '/admin/sessions') {
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end(
          JSON.stringify({
            sessions: [
              { session_id: 'ok', last_access_s: 7200 },
              { session_id: 'boom', last_access_s: 7200 },
            ],
          }),
        );
        return;
      }
      if (req.url?.includes('boom')) {
        res.writeHead(500).end('nope');
        return;
      }
      res.writeHead(200, { 'content-type': 'application/json' }).end('{}');
    });
    const port = await new Promise<number>((resolve) =>
      server.listen(0, '127.0.0.1', () => {
        const addr = server.address();
        if (addr && typeof addr === 'object') resolve(addr.port);
      }),
    );
    try {
      const r = await clearStaleSessions({
        v1BaseUrl: `http://127.0.0.1:${port}/v1`,
        cutoffMinutes: 30,
      });
      expect(r.cleared).toEqual(['ok']);
      expect(r.errors.map((e) => e.session_id)).toEqual(['boom']);
    } finally {
      server.close();
    }
  });
});
