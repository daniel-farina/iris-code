// `search` tool: ripgrep wrapper. Mirrors hip's tools/search.rs. Two
// output modes: files_with_matches (just filenames) and content (lines
// with optional ±context). Glob filtering supported.

import { spawn } from 'node:child_process';
import { type Tool, argString, argU64 } from './types.js';

export const searchTool: Tool = {
  name: 'search',
  spec: {
    type: 'function',
    function: {
      name: 'search',
      description:
        'Recursive ripgrep-style search. Two modes: output_mode="files_with_matches" returns only the file paths; output_mode="content" returns matching lines with ±context (default 0). Pass glob to restrict file types. Returns at most 200 matches before truncation. Use this BEFORE read - it is much cheaper than reading whole files.',
      parameters: {
        type: 'object',
        properties: {
          pattern: { type: 'string', description: 'regex pattern (PCRE2 if rg supports it)' },
          path: { type: 'string', description: 'directory to search (default cwd)' },
          glob: { type: 'string', description: 'file glob like "*.js" or "src/**/*.ts"' },
          output_mode: {
            type: 'string',
            description: '"files_with_matches" (default) or "content"',
          },
          context: {
            type: 'integer',
            description: 'lines of context above/below each match (content mode)',
          },
        },
        required: ['pattern'],
      },
    },
  },
  async run(args) {
    const pattern = argString(args, 'pattern');
    if (!pattern) throw new Error('search: missing pattern');
    const path = argString(args, 'path') ?? '.';
    const glob = argString(args, 'glob');
    const mode = argString(args, 'output_mode') ?? 'files_with_matches';
    const ctx = argU64(args, 'context') ?? 0;

    const argv = ['--max-count=200', '--no-heading'];
    if (mode === 'files_with_matches') argv.push('-l');
    else argv.push('-n', `-C${ctx}`);
    if (glob) argv.push('-g', glob);
    argv.push('--', pattern, path);

    return new Promise<string>((resolve) => {
      const child = spawn('rg', argv, { stdio: ['ignore', 'pipe', 'pipe'] });
      const out: Buffer[] = [];
      child.stdout.on('data', (d) => out.push(d));
      child.stderr.on('data', (d) => out.push(d));
      child.on('error', (err) => resolve(`[search: rg failed: ${err.message}; install ripgrep]\n`));
      child.on('close', (code) => {
        const text = Buffer.concat(out).toString('utf8');
        if (!text.trim() && code === 1) resolve('[no matches]\n');
        else resolve(text);
      });
    });
  },
};
