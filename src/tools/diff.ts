// `diff` tool: unified line diff between two text files. Mirrors hip's
// tools/diff.rs. Uses a Myers-ish LCS for minimal hunks. Hard-capped at
// 400 output lines so a massive diff can't blow the context window.

import { readFile, stat } from 'node:fs/promises';
import { type Tool, argString, argU64 } from './types.js';

const MAX_BYTES = 1_048_576;
const DEFAULT_CONTEXT = 3;
const MAX_OUTPUT_LINES = 400;

export const diffTool: Tool = {
  name: 'diff',
  spec: {
    type: 'function',
    function: {
      name: 'diff',
      description:
        'Unified-style line diff between two text files. Emits `@@ hunks @@` and `-/+` lines. Default context=3. Output capped at 400 lines.',
      parameters: {
        type: 'object',
        properties: {
          path_a: { type: 'string', description: 'left side (before/reference)' },
          path_b: { type: 'string', description: 'right side (after/candidate)' },
          context: { type: 'integer', description: 'context lines around each hunk (default 3)' },
        },
        required: ['path_a', 'path_b'],
      },
    },
  },
  async run(args) {
    const a = argString(args, 'path_a');
    const b = argString(args, 'path_b');
    if (!a || !b) throw new Error('diff: need path_a and path_b');
    const context = argU64(args, 'context') ?? DEFAULT_CONTEXT;

    for (const p of [a, b]) {
      const s = await stat(p);
      if (!s.isFile()) throw new Error(`diff: not a regular file: ${p}`);
      if (s.size > MAX_BYTES) throw new Error(`diff: ${p} is ${s.size} bytes (>1MB)`);
    }
    const aLines = (await readFile(a, 'utf8')).split('\n');
    const bLines = (await readFile(b, 'utf8')).split('\n');

    const ops = diffLines(aLines, bLines);
    const out: string[] = [`--- ${a}`, `+++ ${b}`];
    const hunks = collectHunks(ops, context);
    for (const h of hunks) {
      const headA = `${h.aStart + 1},${h.aLen}`;
      const headB = `${h.bStart + 1},${h.bLen}`;
      out.push(`@@ -${headA} +${headB} @@`);
      for (const line of h.body) out.push(line);
      if (out.length >= MAX_OUTPUT_LINES) {
        out.push(`[truncated at ${MAX_OUTPUT_LINES} lines]`);
        break;
      }
    }
    if (hunks.length === 0) out.push('[no differences]');
    return out.join('\n') + '\n';
  },
};

type Op = { kind: 'eq' | 'del' | 'add'; line: string };

/** Simple LCS-based line diff. Good enough for human-readable hunks on
 * file pairs under a few thousand lines. O(n*m) memory but capped by the
 * 1MB-per-side ceiling. */
function diffLines(a: string[], b: string[]): Op[] {
  const n = a.length;
  const m = b.length;
  // LCS table.
  const dp: number[][] = Array.from({ length: n + 1 }, () => new Array(m + 1).fill(0));
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      if (a[i] === b[j]) dp[i]![j] = dp[i + 1]![j + 1]! + 1;
      else dp[i]![j] = Math.max(dp[i + 1]![j]!, dp[i]![j + 1]!);
    }
  }
  const ops: Op[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (a[i] === b[j]) {
      ops.push({ kind: 'eq', line: a[i]! });
      i++;
      j++;
    } else if (dp[i + 1]![j]! >= dp[i]![j + 1]!) {
      ops.push({ kind: 'del', line: a[i]! });
      i++;
    } else {
      ops.push({ kind: 'add', line: b[j]! });
      j++;
    }
  }
  while (i < n) ops.push({ kind: 'del', line: a[i++]! });
  while (j < m) ops.push({ kind: 'add', line: b[j++]! });
  return ops;
}

interface Hunk {
  aStart: number;
  aLen: number;
  bStart: number;
  bLen: number;
  body: string[];
}

function collectHunks(ops: Op[], context: number): Hunk[] {
  const hunks: Hunk[] = [];
  let aIdx = 0;
  let bIdx = 0;
  let i = 0;
  while (i < ops.length) {
    if (ops[i]!.kind === 'eq') {
      aIdx++;
      bIdx++;
      i++;
      continue;
    }
    // Found a change. Scan to end of this contiguous diff segment.
    const startA = Math.max(0, aIdx - context);
    const startB = Math.max(0, bIdx - context);
    // back-pedal to include leading context.
    const preCtx = Math.min(context, i);
    const body: string[] = [];
    // Emit preceding context.
    const pre = i - preCtx;
    for (let k = pre; k < i; k++) body.push(' ' + ops[k]!.line);
    let aLen = aIdx - startA;
    let bLen = bIdx - startB;
    while (i < ops.length && ops[i]!.kind !== 'eq') {
      const op = ops[i]!;
      if (op.kind === 'del') {
        body.push('-' + op.line);
        aIdx++;
        aLen++;
      } else {
        body.push('+' + op.line);
        bIdx++;
        bLen++;
      }
      i++;
    }
    // Trailing context.
    let post = 0;
    while (i < ops.length && ops[i]!.kind === 'eq' && post < context) {
      body.push(' ' + ops[i]!.line);
      aIdx++;
      bIdx++;
      aLen++;
      bLen++;
      i++;
      post++;
    }
    hunks.push({ aStart: startA, aLen, bStart: startB, bLen, body });
  }
  return hunks;
}
