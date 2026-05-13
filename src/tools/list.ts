// `list` tool: structured directory listing. Mirrors hip's tools/list.rs.

import { readdir, stat } from 'node:fs/promises';
import { join } from 'node:path';
import { type Tool, argString } from './types.js';

export const listTool: Tool = {
  name: 'list',
  spec: {
    type: 'function',
    function: {
      name: 'list',
      description: 'List directory entries (one per line, kind + size + name).',
      parameters: {
        type: 'object',
        properties: { path: { type: 'string', description: 'directory path; default cwd' } },
      },
    },
  },
  async run(args) {
    const path = argString(args, 'path') ?? process.cwd();
    const entries = await readdir(path);
    if (entries.length === 0) throw new Error(`(empty directory: ${path})`);
    type Row = { kind: string; size: number; name: string };
    const rows: Row[] = [];
    for (const name of entries) {
      try {
        const s = await stat(join(path, name));
        const kind = s.isDirectory() ? 'dir' : s.isSymbolicLink() ? 'link' : 'file';
        rows.push({ kind, size: s.size, name });
      } catch {
        rows.push({ kind: '?', size: 0, name });
      }
    }
    rows.sort((a, b) => a.kind.localeCompare(b.kind) || a.name.localeCompare(b.name));
    const out = [`${path}`];
    for (const r of rows)
      out.push(`  ${r.kind.padEnd(4)} ${String(r.size).padStart(10)} ${r.name}`);
    return out.join('\n') + '\n';
  },
};
