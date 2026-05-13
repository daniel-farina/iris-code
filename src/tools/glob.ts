// `glob` tool: match files by pattern. Mirrors hip's tools/glob.rs.
// Uses node's built-in glob (Node 22+) which honors .gitignore via opts.

import { glob } from 'node:fs/promises';
import { resolve } from 'node:path';
import { type Tool, argString } from './types.js';

const MAX_RESULTS = 200;

export const globTool: Tool = {
  name: 'glob',
  spec: {
    type: 'function',
    function: {
      name: 'glob',
      description:
        "Find files matching a glob pattern (e.g. '**/*.ts', 'src/**/*.{js,jsx}'). Returns up to 200 paths sorted. Honors common ignores (node_modules, .git, dist, target).",
      parameters: {
        type: 'object',
        properties: {
          pattern: { type: 'string', description: 'glob pattern' },
          path: { type: 'string', description: 'root dir; default cwd' },
        },
        required: ['pattern'],
      },
    },
  },
  async run(args) {
    const pattern = argString(args, 'pattern');
    if (!pattern) throw new Error('glob: missing pattern');
    const root = argString(args, 'path') ?? process.cwd();
    const cwd = resolve(root);
    const exclude = (p: string) =>
      /\/(node_modules|\.git|target|dist|build|\.next|\.venv|\.cache)(\/|$)/.test(p);
    const matches: string[] = [];
    for await (const p of glob(pattern, { cwd })) {
      if (exclude(p)) continue;
      matches.push(p);
      if (matches.length >= MAX_RESULTS) break;
    }
    matches.sort();
    if (matches.length === 0) return `[no matches for ${pattern} under ${cwd}]\n`;
    const truncated = matches.length === MAX_RESULTS ? ` (truncated at ${MAX_RESULTS})` : '';
    return `${matches.length} match(es)${truncated}:\n${matches.join('\n')}\n`;
  },
};
