// Client-side mitigation for MTPLX's 8-slot session-bank eviction: at
// startup, ask the admin endpoint which sessions have been idle for >N
// minutes and POST /admin/sessions/<id>/clear on each. Frees slots so
// hip's session doesn't get evicted under memory pressure.
//
// Defensive: never clears the session hip is about to use (active id).
// Returns the count of cleared sessions so the CLI can surface it.

interface AdminSessionEntry {
  session_id: string;
  /** Seconds since this session was last accessed; MTPLX exposes this
   *  on /admin/sessions. Sessions without the field are kept (we can't
   *  reason about their staleness). */
  last_access_s?: number;
}

export interface ClearStaleResult {
  cleared: string[];
  skippedActive: string[];
  errors: { session_id: string; reason: string }[];
}

/** Fetches /admin/sessions, filters by last_access_s > cutoffMinutes*60,
 *  excludes the active session id, and POSTs the clear endpoint for each.
 *  v1BaseUrl is hip's --url (ends with /v1); admin lives one level above. */
export async function clearStaleSessions(opts: {
  v1BaseUrl: string;
  cutoffMinutes: number;
  activeSessionId?: string;
  fetchImpl?: typeof fetch;
  signal?: AbortSignal;
}): Promise<ClearStaleResult> {
  const f = opts.fetchImpl ?? fetch;
  const adminUrl = `${opts.v1BaseUrl.replace(/\/v1\/?$/, '')}/admin/sessions`;
  const cutoffSec = opts.cutoffMinutes * 60;
  const result: ClearStaleResult = { cleared: [], skippedActive: [], errors: [] };

  const res = await f(adminUrl, { signal: opts.signal });
  if (!res.ok) {
    throw new Error(`admin/sessions returned ${res.status}`);
  }
  const data = (await res.json()) as { sessions?: AdminSessionEntry[] } | AdminSessionEntry[];
  const sessions: AdminSessionEntry[] = Array.isArray(data) ? data : (data.sessions ?? []);

  const candidates = pickStale(sessions, cutoffSec, opts.activeSessionId);
  for (const sid of candidates.toClear) {
    try {
      const r = await f(`${adminUrl}/${encodeURIComponent(sid)}/clear`, {
        method: 'POST',
        signal: opts.signal,
      });
      if (!r.ok) {
        result.errors.push({ session_id: sid, reason: `HTTP ${r.status}` });
      } else {
        result.cleared.push(sid);
      }
    } catch (e) {
      result.errors.push({ session_id: sid, reason: (e as Error).message });
    }
  }
  result.skippedActive = candidates.skippedActive;
  return result;
}

/** Pure helper: split sessions into clear/skip lists by cutoff +
 *  active-session guard. Exported for tests. */
export function pickStale(
  sessions: AdminSessionEntry[],
  cutoffSec: number,
  activeSessionId?: string,
): { toClear: string[]; skippedActive: string[] } {
  const toClear: string[] = [];
  const skippedActive: string[] = [];
  for (const s of sessions) {
    if (typeof s.last_access_s !== 'number') continue;
    if (s.last_access_s < cutoffSec) continue;
    if (activeSessionId && s.session_id === activeSessionId) {
      skippedActive.push(s.session_id);
      continue;
    }
    toClear.push(s.session_id);
  }
  return { toClear, skippedActive };
}
