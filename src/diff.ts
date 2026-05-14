// Minimal unified-diff generator for the edit tools. We don't pull
// in the `diff` npm package - a single edit just affects one
// contiguous region, so we can render a hunk by locating the changed
// lines and surrounding them with `context` unchanged lines on each
// side. Matches `git diff --unified=N` for this restricted case.
//
// For multi-edit, callers pass the BEFORE and AFTER content of the
// whole file; we compute the LCS-based line diff to render multiple
// hunks. The LCS is O(N*M) but capped on file size so it stays under
// 50ms even for the largest edits we accept (1 GiB cap on the file
// itself; lines are usually <50k).

const CONTEXT_LINES = 3;

export interface Hunk {
  oldStart: number;
  oldLines: number;
  newStart: number;
  newLines: number;
  lines: string[]; // each starts with ' ', '-', or '+'
}

export interface UnifiedDiff {
  path: string;
  hunks: Hunk[];
  /** Human-readable patch (file headers + hunks). */
  patch: string;
  additions: number;
  deletions: number;
}

export function makeUnifiedDiff(path: string, before: string, after: string): UnifiedDiff {
  const beforeLines = before.split('\n');
  const afterLines = after.split('\n');
  const hunks = computeHunks(beforeLines, afterLines);
  let additions = 0;
  let deletions = 0;
  for (const h of hunks) {
    for (const l of h.lines) {
      if (l.startsWith('+')) additions++;
      else if (l.startsWith('-')) deletions++;
    }
  }
  const patch = renderPatch(path, hunks);
  return { path, hunks, patch, additions, deletions };
}

/** LCS-based line-diff. Returns a list of hunks - contiguous runs of
 *  changes plus CONTEXT_LINES of unchanged lines on each side. */
function computeHunks(before: string[], after: string[]): Hunk[] {
  // Build LCS table. O(N*M) space; for very large files this could
  // blow memory. Cap at 50k lines per side and fall back to a single
  // "whole file changed" hunk if exceeded.
  if (before.length > 50_000 || after.length > 50_000) {
    return [
      {
        oldStart: 1,
        oldLines: before.length,
        newStart: 1,
        newLines: after.length,
        lines: [...before.map((l) => `-${l}`), ...after.map((l) => `+${l}`)],
      },
    ];
  }
  const n = before.length;
  const m = after.length;
  const dp: number[][] = Array.from({ length: n + 1 }, () => new Array(m + 1).fill(0));
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      dp[i]![j] = before[i] === after[j] ? dp[i + 1]![j + 1]! + 1 : Math.max(dp[i + 1]![j]!, dp[i]![j + 1]!);
    }
  }
  // Walk the table to emit ' ', '-', '+' ops.
  type Op = { kind: ' ' | '-' | '+'; line: string; oldLine?: number; newLine?: number };
  const ops: Op[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (before[i] === after[j]) {
      ops.push({ kind: ' ', line: before[i]!, oldLine: i + 1, newLine: j + 1 });
      i++;
      j++;
    } else if (dp[i + 1]![j]! >= dp[i]![j + 1]!) {
      ops.push({ kind: '-', line: before[i]!, oldLine: i + 1 });
      i++;
    } else {
      ops.push({ kind: '+', line: after[j]!, newLine: j + 1 });
      j++;
    }
  }
  while (i < n) ops.push({ kind: '-', line: before[i++]!, oldLine: i });
  while (j < m) ops.push({ kind: '+', line: after[j++]!, newLine: j });

  // Group ops into hunks: a run of changes flanked by CONTEXT_LINES
  // of context. Adjacent change-clusters merge if their context
  // windows overlap.
  const hunks: Hunk[] = [];
  let cur: { startIdx: number; endIdx: number } | null = null;
  for (let k = 0; k < ops.length; k++) {
    const op = ops[k]!;
    if (op.kind === ' ') continue;
    if (!cur) {
      cur = { startIdx: Math.max(0, k - CONTEXT_LINES), endIdx: Math.min(ops.length, k + 1 + CONTEXT_LINES) };
    } else if (k - cur.endIdx < CONTEXT_LINES * 2) {
      cur.endIdx = Math.min(ops.length, k + 1 + CONTEXT_LINES);
    } else {
      hunks.push(opsToHunk(ops, cur.startIdx, cur.endIdx));
      cur = { startIdx: Math.max(0, k - CONTEXT_LINES), endIdx: Math.min(ops.length, k + 1 + CONTEXT_LINES) };
    }
  }
  if (cur) hunks.push(opsToHunk(ops, cur.startIdx, cur.endIdx));
  return hunks;
}

function opsToHunk(
  ops: { kind: ' ' | '-' | '+'; line: string; oldLine?: number; newLine?: number }[],
  start: number,
  end: number,
): Hunk {
  const slice = ops.slice(start, end);
  let oldStart = 0;
  let newStart = 0;
  let oldLines = 0;
  let newLines = 0;
  for (const op of slice) {
    if (op.kind !== '+' && op.oldLine !== undefined) {
      if (!oldStart) oldStart = op.oldLine;
      oldLines++;
    }
    if (op.kind !== '-' && op.newLine !== undefined) {
      if (!newStart) newStart = op.newLine;
      newLines++;
    }
  }
  return {
    oldStart: oldStart || 1,
    oldLines,
    newStart: newStart || 1,
    newLines,
    lines: slice.map((o) => `${o.kind}${o.line}`),
  };
}

function renderPatch(path: string, hunks: Hunk[]): string {
  const out: string[] = [`--- a/${path}`, `+++ b/${path}`];
  for (const h of hunks) {
    out.push(`@@ -${h.oldStart},${h.oldLines} +${h.newStart},${h.newLines} @@`);
    out.push(...h.lines);
  }
  return out.join('\n');
}

/** Compact diff for tool result display - the unified patch capped
 *  to a few hundred lines (so a giant file rewrite doesn't blow the
 *  prompt budget). */
export function formatDiffForResult(diff: UnifiedDiff, maxLines = 60): string {
  const lines = diff.patch.split('\n');
  if (lines.length <= maxLines) return diff.patch;
  return `${lines.slice(0, maxLines).join('\n')}\n[... patch truncated; ${lines.length - maxLines} more lines, +${diff.additions} -${diff.deletions} total]`;
}
