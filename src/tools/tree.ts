// `tree` tool: depth-limited directory tree. Mirrors hip's tools/tree.rs.
// Skips common noise (node_modules, .git, target, dist, build, .next, .venv).

import { readdir, stat } from 'node:fs/promises';
import { join } from 'node:path';
import { type Tool, argBool, argString, argU64 } from './types.js';

const IGNORED = new Set([
  '.git',
  'node_modules',
  'target',
  'dist',
  'build',
  '.next',
  '.venv',
  '.cache',
  '.DS_Store',
]);
const MAX_ENTRIES = 400;

export const treeTool: Tool = {
  name: 'tree',
  spec: {
    type: 'function',
    function: {
      name: 'tree',
      description:
        'Depth-limited directory tree (skips node_modules/.git/target/dist/build/.next/.venv). Default depth=2. Hidden files hidden unless hidden=true. Output capped at 400 entries.',
      parameters: {
        type: 'object',
        properties: {
          path: { type: 'string', description: 'root path; default cwd' },
          depth: { type: 'integer', description: 'max depth (default 2)' },
          hidden: { type: 'boolean', description: 'include dotfiles (default false)' },
        },
      },
    },
  },
  async run(args) {
    const root = argString(args, 'path') ?? process.cwd();
    const maxDepth = argU64(args, 'depth') ?? 2;
    const hidden = argBool(args, 'hidden') ?? false;

    const lines: string[] = [];
    let totalSize = 0;
    let fileCount = 0;
    let dirCount = 0;
    let entryCount = 0;
    let truncated = false;
    lines.push(`${root} (depth=${maxDepth}, hidden=${hidden})`);

    async function walk(dir: string, depth: number, prefix: string) {
      if (truncated || depth > maxDepth) return;
      let entries: string[];
      try {
        entries = await readdir(dir);
      } catch {
        return;
      }
      entries.sort();
      for (const name of entries) {
        if (!hidden && name.startsWith('.')) continue;
        if (IGNORED.has(name)) continue;
        if (entryCount >= MAX_ENTRIES) {
          truncated = true;
          return;
        }
        entryCount++;
        const full = join(dir, name);
        let s: import('node:fs').Stats;
        try {
          s = await stat(full);
        } catch {
          continue;
        }
        const isDir = s.isDirectory();
        if (isDir) dirCount++;
        else {
          fileCount++;
          totalSize += s.size;
        }
        const sizeStr = isDir ? '' : `  (${humanSize(s.size)})`;
        lines.push(`${prefix}${name}${isDir ? '/' : ''}${sizeStr}`);
        if (isDir && depth < maxDepth) await walk(full, depth + 1, `${prefix}  `);
      }
    }
    await walk(root, 1, '  ');
    lines.push('');
    lines.push(
      `(${fileCount} files, ${dirCount} dirs, ${totalSize} b${truncated ? `; truncated at ${MAX_ENTRIES}` : ''})`,
    );
    return lines.join('\n') + '\n';
  },
};

function humanSize(n: number): string {
  if (n < 1024) return `${n} b`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}
