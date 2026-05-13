// Pure helpers for the TUI's auto-/compact trigger.
//
// Motivation: MTPLX's paged-KV cache thrashes once a session's prefix
// crosses ~10K tokens (session-bank eviction + page reclaim under
// memory pressure). Compacting the conv before we cross that cliff
// keeps prefixes small and the bank slot warm.
//
// Token estimate: JSON.stringify(conv).length / 4 is a coarse but
// stable approximation - good enough for a threshold check that just
// needs to fire near 10K. Extracted here so it's testable without Ink.

import type { ChatMessage } from '../schema.js';

/** Conv-size cliff (in tokens) above which MTPLX starts evicting the
 *  session from its 8-slot bank. We trigger compact a bit below this. */
export const AUTO_COMPACT_TOKEN_THRESHOLD = 9000;

/** Cheap token estimate: ~4 chars/token on average for English + JSON. */
export function estimateTokens(conv: ChatMessage[]): number {
  return Math.floor(JSON.stringify(conv).length / 4);
}

/** Returns true if auto-compact should fire for this conv. Pure: takes
 *  the conv length so callers can guard on "we have something to
 *  summarize" (compacting <=3 msgs is pointless and round-trips for
 *  nothing). */
export function shouldAutoCompact(conv: ChatMessage[], enabled: boolean): boolean {
  if (!enabled) return false;
  if (conv.length <= 3) return false;
  return estimateTokens(conv) >= AUTO_COMPACT_TOKEN_THRESHOLD;
}

/** Format token counts for the system transcript line. 9234 -> "9.2K". */
export function formatTokens(n: number): string {
  if (n < 1000) return String(n);
  return (n / 1000).toFixed(1) + 'K';
}
