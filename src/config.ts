// Configuration constants ported from hip Rust source. Same defaults so
// behavior is identical to mlx-code's hip 0.3.49+. Override via flags or env.

export const DEFAULT_MTPLX_URL = process.env['MLX_CODE_URL'] ?? 'http://127.0.0.1:8088/v1';
export const DEFAULT_MODEL = process.env['MLX_CODE_MODEL'] ?? 'mtplx-qwen36-27b-optimized-speed';

// Sampling defaults (matches hip's SamplingOpts::default() in v0.3.49).
export const DEFAULT_TEMPERATURE = 0.6;
export const DEFAULT_TOP_P = 0.95;
export const DEFAULT_TOP_K = 20;
// Raised 6144 -> 8192 -> 12288: long code-write rounds (writing a
// fresh module like physics.js or track.js from scratch) routinely
// hit the 8K cap and trigger finish_reason=length auto-continues,
// which re-tokenize the full conv prefix on the next round (mlx-lm
// doesn't persist KV across requests). One round at 12K is cheaper
// than two rounds at 8K for code-heavy turns. Per-turn cost on M3
// sustained profile: ~12K tok ÷ 40 tok/s ≈ 5 min worst case, but
// rarely fills (most rounds still emit 50-2000 tok).
export const DEFAULT_MAX_TOKENS = 12288;
// Auto-compact threshold (approximate tokens). Empirically MTPLX's
// session-bank entries can grow to ~24K tokens before getting evicted
// (observed on a 32-entry bank with MTPLX_SESSION_BANK_MAX_ENTRIES=32).
// Crossing that wall flips every subsequent turn from WARM (~0.3s
// TTFT) to COLD (~45s TTFT full prefill). Raised 18000 -> 24000 to
// give conv-heavy stretches more headroom before /compact kicks in;
// this sits ~at the cliff rather than ~75% short of it - the bet is
// that fewer compact cycles is worth the occasional cold prefill if
// a turn lands right past the wall. Drop back to ~22000 if cold-TTFT
// turns become frequent. Override via --auto-compact-tokens.
export const DEFAULT_AUTO_COMPACT_THRESHOLD_TOKENS = 24000;

// Round cap. With auto-compact + postcommit polling each round is
// fast and warm, so the bottleneck for multi-feature refactors used
// to be this cap. Bumped from 30 to 60: a "create 5 files" task
// needs ~5 * (touch + 3-5 appends + verify) = ~25-35 tool calls,
// which often spans 30-50 rounds in qwen3-style 1-2-tools-per-round
// emission. Auto-compact mid-loop keeps the prefix small even at 60.
export const DEFAULT_MAX_ROUNDS = 60;
export const MAX_CONSECUTIVE_LENGTH_CONTINUES = 3;

// Between agent-loop rounds, wait for MTPLX's KV-cache postcommit to
// land before firing the next request. The agent polls MTPLX's
// /admin/sessions endpoint and returns as soon as it sees the commit
// stored - this is an UPPER BOUND, not a fixed sleep. Lets us scale
// gracefully to slower devices / bigger prefixes (observed commit time
// ranges from ~3s for a 4K prefix on M3 to ~11s for a 17K prefix).
// Tunable via --postcommit-delay; set to 0 to disable polling entirely.
export const DEFAULT_POSTCOMMIT_DELAY_MS = 30000;

// Auto-generated session id matches hip's format: auto-YYYYMMDD-HHMMSS-<6hex>.
// Without this the prefix cache mixes unrelated dispatches.
export function generateAutoSessionId(): string {
  const now = new Date();
  const pad = (n: number, w: number) => n.toString().padStart(w, '0');
  const stamp = `${now.getUTCFullYear()}${pad(now.getUTCMonth() + 1, 2)}${pad(now.getUTCDate(), 2)}-${pad(now.getUTCHours(), 2)}${pad(now.getUTCMinutes(), 2)}${pad(now.getUTCSeconds(), 2)}`;
  const rand = Math.floor(Math.random() * 0xffffff)
    .toString(16)
    .padStart(6, '0');
  return `auto-${stamp}-${rand}`;
}

// CWD-stable session id: `cwd-<8charhash>-<YYYYMMDD>`. Reused across
// every hip run in the same directory on the same day, so MTPLX's
// session-bank LRU keeps the prefix cache warm without the user
// having to remember a session id. Rolls over at midnight so the
// session doesn't grow unboundedly large across multi-day work.
// Falls back to a per-invocation uniqe id if cwd can't be resolved.
export function generateCwdStableSessionId(cwd: string): string {
  if (!cwd) return generateAutoSessionId();
  // 32-bit FNV-1a is plenty for ~uniqueness across a user's dirs; not
  // a crypto context. Faster + zero-dep vs crypto.createHash.
  let h = 0x811c9dc5;
  for (let i = 0; i < cwd.length; i++) {
    h ^= cwd.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  const hash = (h >>> 0).toString(16).padStart(8, '0');
  const now = new Date();
  const pad = (n: number) => n.toString().padStart(2, '0');
  const date = `${now.getUTCFullYear()}${pad(now.getUTCMonth() + 1)}${pad(now.getUTCDate())}`;
  return `cwd-${hash}-${date}`;
}
