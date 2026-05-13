// `grep` tool: regex search across files. Mirrors hip's tools/grep.rs.
// Uses ripgrep when available (faster + .gitignore aware), else falls back
// to native walk + regex. Returns up to 50 file:line:text hits.

import { spawn } from 'node:child_process';
import { readFile, readdir, stat } from 'node:fs/promises';
import { join, relative } from 'node:path';
import { type Tool, argString } from './types.js';

const MAX_HITS = 50;
const IGNORE_DIRS = new Set([
  '.git',
  'node_modules',
  'target',
  'dist',
  'build',
  '.next',
  '.venv',
  '.cache',
]);

export const grepTool: Tool = {
  name: 'grep',
  spec: {
    type: 'function',
    function: {
      name: 'grep',
      description:
        'Search files for a regex. Honors .gitignore. Returns up to 50 file:line:text hits. For larger result sets or files-only mode use `search` instead.',
      parameters: {
        type: 'object',
        properties: {
          pattern: { type: 'string', description: 'regex pattern (PCRE2 via rg) or literal' },
          path: { type: 'string', description: 'directory or file to search; default cwd' },
          glob: { type: 'string', description: 'file glob filter, e.g. *.ts' },
        },
        required: ['pattern'],
      },
    },
  },
  async run(args) {
    const pattern = argString(args, 'pattern');
    if (!pattern) throw new Error('grep: missing pattern');
    const path = argString(args, 'path') ?? process.cwd();
    const globPattern = argString(args, 'glob');

    // Prefer ripgrep for speed + correctness; fall back if absent.
    const rgOut = await tryRipgrep(pattern, path, globPattern);
    if (rgOut !== null) return rgOut;
    return await fallbackWalk(pattern, path, globPattern);
  },
};

async function tryRipgrep(
  pattern: string,
  path: string,
  globPattern: string | undefined,
): Promise<string | null> {
  return new Promise<string | null>((resolve) => {
    const argv = [`-n`, `--no-heading`, `--max-count=${MAX_HITS}`, `--smart-case`];
    if (globPattern) argv.push('-g', globPattern);
    argv.push('--', pattern, path);
    const child = spawn('rg', argv, { stdio: ['ignore', 'pipe', 'pipe'] });
    const out: Buffer[] = [];
    child.stdout.on('data', (d) => out.push(d));
    child.stderr.on('data', (d) => out.push(d));
    child.on('error', () => resolve(null)); // rg not installed
    child.on('close', (code) => {
      if (code === 1 && out.length === 0) {
        resolve('[no matches]\n');
        return;
      }
      const lines = Buffer.concat(out).toString('utf8').split('\n').filter(Boolean);
      const hits = lines.slice(0, MAX_HITS);
      const note = lines.length > MAX_HITS ? `\n(truncated to ${MAX_HITS} hits)` : '';
      resolve(`${hits.length} hit(s):\n${hits.join('\n')}${note}\n`);
    });
  });
}

async function fallbackWalk(
  pattern: string,
  root: string,
  globPattern: string | undefined,
): Promise<string> {
  const re = new RegExp(pattern);
  const globRe = globPattern ? globToRegex(globPattern) : null;
  const hits: string[] = [];
  const stack: string[] = [root];
  while (stack.length > 0 && hits.length < MAX_HITS) {
    const cur = stack.pop()!;
    let entries: string[];
    try {
      const s = await stat(cur);
      if (s.isFile()) {
        await scanFile(cur, re, globRe, hits, root);
        continue;
      }
      entries = await readdir(cur);
    } catch {
      continue;
    }
    for (const name of entries) {
      if (IGNORE_DIRS.has(name)) continue;
      if (name.startsWith('.')) continue;
      stack.push(join(cur, name));
    }
  }
  if (hits.length === 0) return '[no matches]\n';
  return `${hits.length} hit(s):\n${hits.join('\n')}\n`;
}

async function scanFile(
  file: string,
  re: RegExp,
  globRe: RegExp | null,
  hits: string[],
  root: string,
): Promise<void> {
  if (globRe && !globRe.test(file)) return;
  // Crude binary check: skip files with > 0.5% NUL bytes in first 4KB.
  let content: string;
  try {
    const head = await readFile(file);
    if (head.length > 1_048_576) return; // 1MB cap per file
    let nuls = 0;
    for (let i = 0; i < Math.min(head.length, 4096); i++) if (head[i] === 0) nuls++;
    if (nuls > 4096 * 0.005) return;
    content = head.toString('utf8');
  } catch {
    return;
  }
  const rel = relative(root, file) || file;
  const lines = content.split('\n');
  for (let i = 0; i < lines.length; i++) {
    if (hits.length >= MAX_HITS) return;
    if (re.test(lines[i] ?? '')) hits.push(`${rel}:${i + 1}:${lines[i]}`);
  }
}

function globToRegex(g: string): RegExp {
  // Tiny glob: * -> [^/]*, ** -> .*, ? -> [^/]
  const escaped = g.replace(/[.+^${}()|[\]\\]/g, '\\$&');
  const re = escaped
    .replace(/\*\*/g, '__DOUBLESTAR__')
    .replace(/\*/g, '[^/]*')
    .replace(/__DOUBLESTAR__/g, '.*')
    .replace(/\?/g, '[^/]');
  return new RegExp(`(^|/)${re}$`);
}
