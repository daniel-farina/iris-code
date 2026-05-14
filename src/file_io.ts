// File I/O helpers for the edit tools. Encapsulates encoding
// detection, line-ending preservation, and the path-similarity hint
// shown when a write target doesn't exist.
//
// Two encodings are supported:
//   - UTF-8 (default for everything without a UTF-16 BOM)
//   - UTF-16-LE (detected via 0xff 0xfe BOM)
// UTF-16-BE is rare enough that we don't bother and treat it as UTF-8.
//
// Line endings are tracked separately from encoding: a file may be
// UTF-8 with CRLF (Windows) or UTF-16-LE with LF (rare but legal).
// We always read into a canonical LF-only string for matching, then
// re-apply the original line endings on write.

import { existsSync, readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';

export type FileEncoding = 'utf8' | 'utf16le';
export type LineEnding = 'LF' | 'CRLF' | 'CR' | 'mixed';

export interface ReadFileResult {
  /** File content normalized to LF for matching. Always returned;
   *  callers re-apply `lineEnding` when writing. */
  content: string;
  /** Original byte size on disk. */
  bytes: number;
  encoding: FileEncoding;
  lineEnding: LineEnding;
  exists: boolean;
}

/** Read a text file. When the file is missing, returns a stub with
 *  exists=false; callers decide whether that's an error. */
export function readFileForEdit(path: string): ReadFileResult {
  const abs = resolve(path);
  if (!existsSync(abs)) {
    return {
      content: '',
      bytes: 0,
      encoding: 'utf8',
      lineEnding: 'LF',
      exists: false,
    };
  }
  const buf = readFileSync(abs);
  const encoding: FileEncoding =
    buf.length >= 2 && buf[0] === 0xff && buf[1] === 0xfe ? 'utf16le' : 'utf8';
  const raw = buf.toString(encoding);
  const lineEnding = detectLineEnding(raw);
  // Normalize for matching.
  const content = raw.replace(/\r\n/g, '\n').replace(/\r/g, '\n');
  return { content, bytes: buf.byteLength, encoding, lineEnding, exists: true };
}

/** Write a string back to a file using the encoding + line endings
 *  that were detected on read. Pass `firstWrite=true` for new files
 *  (no detection - default to UTF-8 + LF). */
export function writeFileForEdit(
  path: string,
  contentLF: string,
  encoding: FileEncoding,
  lineEnding: LineEnding,
): void {
  const abs = resolve(path);
  let out = contentLF;
  if (lineEnding === 'CRLF') out = contentLF.replace(/\n/g, '\r\n');
  else if (lineEnding === 'CR') out = contentLF.replace(/\n/g, '\r');
  // 'mixed' files are written as their dominant ending, which we
  // approximate as CRLF (Windows wins) - matches what most editors
  // do on save.
  else if (lineEnding === 'mixed') out = contentLF.replace(/\n/g, '\r\n');
  writeFileSync(abs, out, { encoding });
}

function detectLineEnding(s: string): LineEnding {
  const crlf = (s.match(/\r\n/g) ?? []).length;
  // Lone \r (not followed by \n) is classic Mac line ending - rare
  // but seen in some legacy files.
  const lf = (s.match(/(?<!\r)\n/g) ?? []).length;
  const cr = (s.match(/\r(?!\n)/g) ?? []).length;
  if (crlf === 0 && lf === 0 && cr === 0) return 'LF';
  if (crlf > 0 && lf === 0 && cr === 0) return 'CRLF';
  if (cr > 0 && crlf === 0 && lf === 0) return 'CR';
  if (lf > 0 && crlf === 0 && cr === 0) return 'LF';
  return 'mixed';
}

/** Find a file in the same directory with a similar basename, useful
 *  for "did you mean ...?" hints when the model picks a wrong path.
 *  Returns the absolute path or undefined. */
export function findSimilarFile(targetPath: string): string | undefined {
  const abs = resolve(targetPath);
  const dir = dirname(abs);
  if (!existsSync(dir)) return undefined;
  const base = basename(abs);
  const baseStem = base.replace(/\.[^./]*$/, ''); // strip extension
  let entries: string[];
  try {
    entries = readdirSync(dir);
  } catch {
    return undefined;
  }
  // Prefer exact-stem-different-extension matches first.
  const stemMatch = entries.find((e) => {
    const stem = e.replace(/\.[^./]*$/, '');
    return stem === baseStem && e !== base;
  });
  if (stemMatch) return join(dir, stemMatch);
  // Otherwise scan for the closest by Levenshtein distance, capped at
  // 3 edits so we don't return wildly unrelated files.
  let best: { name: string; dist: number } | undefined;
  for (const e of entries) {
    if (e === base) continue;
    const d = levenshtein(e, base);
    if (d > 3) continue;
    if (!best || d < best.dist) best = { name: e, dist: d };
  }
  return best ? join(dir, best.name) : undefined;
}

/** Suggest a file under cwd whose basename matches. Used when the
 *  model gives just "main.ts" but the file is actually at "src/main.ts". */
export function suggestPathUnderCwd(targetPath: string, cwd = process.cwd()): string | undefined {
  const target = basename(targetPath);
  if (!target) return undefined;
  // Scan up to ~500 entries to keep this cheap. Skip hidden dirs and
  // common large ones.
  const skip = new Set(['node_modules', '.git', '.next', 'dist', 'build', '.cache']);
  const queue: string[] = [cwd];
  let scanned = 0;
  while (queue.length > 0 && scanned < 500) {
    const dir = queue.shift()!;
    let entries: string[];
    try {
      entries = readdirSync(dir);
    } catch {
      continue;
    }
    for (const e of entries) {
      scanned++;
      if (e.startsWith('.') || skip.has(e)) continue;
      const full = join(dir, e);
      let st;
      try {
        st = statSync(full);
      } catch {
        continue;
      }
      if (st.isFile() && e === target) return full;
      if (st.isDirectory() && queue.length < 50) queue.push(full);
    }
  }
  return undefined;
}

/** Classic Levenshtein distance, capped at maxDist so we can early-exit. */
function levenshtein(a: string, b: string, maxDist = 4): number {
  if (Math.abs(a.length - b.length) > maxDist) return maxDist + 1;
  const dp: number[] = new Array(b.length + 1);
  for (let j = 0; j <= b.length; j++) dp[j] = j;
  for (let i = 1; i <= a.length; i++) {
    let prev = dp[0]!;
    dp[0] = i;
    let rowMin = i;
    for (let j = 1; j <= b.length; j++) {
      const tmp = dp[j]!;
      dp[j] =
        a[i - 1] === b[j - 1]
          ? prev
          : Math.min(prev, dp[j]!, dp[j - 1]!) + 1;
      prev = tmp;
      if (dp[j]! < rowMin) rowMin = dp[j]!;
    }
    if (rowMin > maxDist) return maxDist + 1;
  }
  return dp[b.length]!;
}
