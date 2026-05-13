import type { ToolSpec } from '../schema.js';

export interface Tool {
  name: string;
  spec: ToolSpec;
  run(args: Record<string, unknown>): Promise<string>;
}

/** Coerce a JSON value to a finite non-negative integer, accepting either a
 * real number OR a numeric string. Mirrors hip's tools/read.rs arg_u64()
 * helper - same string-arg defense for when MTPLX delivers parameters as
 * strings (qwen3 XML parser edge cases). */
export function argU64(args: Record<string, unknown>, key: string): number | undefined {
  const v = args[key];
  if (typeof v === 'number' && Number.isFinite(v) && v >= 0) return Math.floor(v);
  if (typeof v === 'string') {
    const n = Number(v.trim());
    if (Number.isFinite(n) && n >= 0) return Math.floor(n);
  }
  return undefined;
}

export function argString(args: Record<string, unknown>, key: string): string | undefined {
  const v = args[key];
  return typeof v === 'string' ? v : undefined;
}

export function argBool(args: Record<string, unknown>, key: string): boolean | undefined {
  const v = args[key];
  if (typeof v === 'boolean') return v;
  if (typeof v === 'string') return v === 'true' || v === '1';
  return undefined;
}
