// `read` tool: ported from hip's src/tools/read.rs. Default limit 200 lines
// because blind reads blow the prompt budget on qwen3's quadratic prefill.
// 1MB file cap. Supports offset+limit and around+context modes.

import { readFile, stat } from 'node:fs/promises';
import { type Tool, argString, argU64 } from './types.js';

const MAX_BYTES = 1_048_576;
const DEFAULT_LIMIT = 200;

export const readTool: Tool = {
  name: 'read',
  spec: {
    type: 'function',
    function: {
      name: 'read',
      description:
        'Read a UTF-8 text file slice. Default: 200 lines from start (small on purpose - large blind reads blow the prompt budget). After a search hit, use `around=<line>` with `context` for a symmetric window. For a known section, use `offset`+`limit`. Output shows `[truncated at N/<total>]` when the file extends beyond what you asked for, so you know to request more if needed. Refuses files >1MB.',
      parameters: {
        type: 'object',
        properties: {
          path: { type: 'string', description: 'absolute or cwd-relative file path' },
          offset: { type: 'integer', description: '1-based starting line (default 1)' },
          limit: {
            type: 'integer',
            description: 'max lines to return (default 200; raise explicitly for larger reads)',
          },
          around: {
            type: 'integer',
            description:
              '1-based target line; overrides offset/limit. Returns ±context lines around it.',
          },
          context: { type: 'integer', description: 'context lines for `around` (default 20)' },
        },
        required: ['path'],
      },
    },
  },
  async run(args) {
    const path = argString(args, 'path');
    if (!path) throw new Error('read: missing path');

    const around = argU64(args, 'around');
    const context = argU64(args, 'context') ?? 20;
    let offset = 1;
    let limit = DEFAULT_LIMIT;

    if (around !== undefined && around >= 1) {
      offset = Math.max(1, around - context);
      limit = around - offset + context + 1;
    } else {
      offset = Math.max(1, argU64(args, 'offset') ?? 1);
      limit = argU64(args, 'limit') ?? DEFAULT_LIMIT;
    }

    const st = await stat(path);
    if (!st.isFile()) throw new Error(`read: not a regular file: ${path}`);
    if (st.size > MAX_BYTES)
      throw new Error(`read: file is ${st.size} bytes (>1MB), refuse. Use offset/limit.`);

    const content = await readFile(path, 'utf8');
    const lines = content.split('\n');
    const totalLines = lines.length;
    const out: string[] = [];
    let emitted = 0;
    for (let i = 0; i < lines.length; i++) {
      const lineno = i + 1;
      if (lineno < offset) continue;
      if (emitted >= limit) break;
      const marker = around === lineno ? '>' : ' ';
      out.push(`${marker}${String(lineno).padStart(5)}\t${lines[i]}`);
      emitted++;
    }
    if (out.length === 0) return `(empty range; file has ${totalLines} lines)\n`;
    if (emitted < totalLines - offset + 1) {
      out.push(`[truncated at ${offset + emitted - 1}/${totalLines}]`);
    }
    return out.join('\n') + '\n';
  },
};
