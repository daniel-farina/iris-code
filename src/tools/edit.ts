// `edit` tool: exact string replace. Ported from hip's tools/edit.rs.
// Refuses if old_string not found, refuses if old_string is ambiguous
// (multiple matches without replace_all=true), refuses if new_string ==
// old_string.

import { readFile, stat, writeFile } from 'node:fs/promises';
import { type Tool, argBool, argString } from './types.js';

export const editTool: Tool = {
  name: 'edit',
  spec: {
    type: 'function',
    function: {
      name: 'edit',
      description:
        'Replace exact text in a file. Requires unique old_string unless replace_all=true. Refuses if new_string equals old_string. Returns the unified diff of the change. Use search first to find the exact bytes; do NOT guess - whitespace, indentation, and case must match.',
      parameters: {
        type: 'object',
        properties: {
          path: { type: 'string', description: 'file path' },
          old_string: { type: 'string', description: 'text to replace; must match exactly' },
          new_string: { type: 'string', description: 'replacement text' },
          replace_all: {
            type: 'boolean',
            description: 'replace every match; default false (require unique match)',
          },
        },
        required: ['path', 'old_string', 'new_string'],
      },
    },
  },
  async run(args) {
    const path = argString(args, 'path');
    const oldS = argString(args, 'old_string');
    const newS = argString(args, 'new_string');
    if (!path || oldS === undefined || newS === undefined)
      throw new Error('edit: need path, old_string, new_string');
    if (oldS === newS) throw new Error('edit: new_string equals old_string (no-op)');
    if (oldS === '') throw new Error('edit: old_string is empty (use a unique anchor)');
    const replaceAll = argBool(args, 'replace_all') ?? false;

    await stat(path); // throws if missing
    const original = await readFile(path, 'utf8');
    const occurrences = countOccurrences(original, oldS);
    if (occurrences === 0)
      throw new Error(`edit: old_string not found in ${path} (check whitespace/case/CRLF)`);
    if (occurrences > 1 && !replaceAll)
      throw new Error(
        `edit: old_string matches ${occurrences} places in ${path}. Pass replace_all=true or extend old_string to make it unique.`,
      );

    const updated = replaceAll ? original.split(oldS).join(newS) : original.replace(oldS, newS);
    await writeFile(path, updated, 'utf8');
    return `[ok] edited ${path} (${replaceAll ? occurrences : 1} replacement${replaceAll && occurrences !== 1 ? 's' : ''}, net ${signedDelta(original.length, updated.length)} byte(s))\n`;
  },
};

function countOccurrences(haystack: string, needle: string): number {
  let count = 0;
  let pos = 0;
  while ((pos = haystack.indexOf(needle, pos)) !== -1) {
    count++;
    pos += needle.length;
  }
  return count;
}

function signedDelta(before: number, after: number): string {
  const d = after - before;
  return d >= 0 ? `+${d}` : `${d}`;
}
