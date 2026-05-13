// Configuration constants ported from hip Rust source. Same defaults so
// behavior is identical to mlx-code's hip 0.3.49+. Override via flags or env.

export const DEFAULT_MTPLX_URL = process.env['MLX_CODE_URL'] ?? 'http://127.0.0.1:8088/v1';
export const DEFAULT_MODEL = process.env['MLX_CODE_MODEL'] ?? 'mtplx-qwen36-27b-optimized-speed';

// Sampling defaults (matches hip's SamplingOpts::default() in v0.3.49).
export const DEFAULT_TEMPERATURE = 0.6;
export const DEFAULT_TOP_P = 0.95;
export const DEFAULT_TOP_K = 20;
// Bumped from Rust hip's 4096 to 6144 (1.5x) - the qwen3 tool_call JSON
// parser truncates mid-arg when long bash heredocs hit the old cap,
// producing malformed-JSON 422s. A small headroom bump shifts the
// failure mode from "fatal parse error" to "finish_reason=length" which
// auto-continues. Still much smaller than the model's 32k context window.
export const DEFAULT_MAX_TOKENS = 6144;
export const DEFAULT_MAX_ROUNDS = 30;
export const MAX_CONSECUTIVE_LENGTH_CONTINUES = 3;

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
