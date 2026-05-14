// `read` tool: ported from hip's src/tools/read.rs. Default limit 200 lines
// because blind reads blow the prompt budget on qwen3's quadratic prefill.
// 1MB file cap. Supports offset+limit and around+context modes.

import { readFile, stat } from 'node:fs/promises';
import { readState } from '../read_state.js';
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
        'Read a UTF-8 text file. Default 200 lines from start. Use `around=N`+`context` for a window around a search hit, or `offset`+`limit` for a known range. Returns `[truncated at N/total]` when more remains. Refuses files >1MB.',
      parameters: {
        type: 'object',
        properties: {
          path: { type: 'string' },
          offset: { type: 'integer', description: '1-based start line (default 1)' },
          limit: { type: 'integer', description: 'max lines (default 200)' },
          around: { type: 'integer', description: '1-based target line; overrides offset/limit' },
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

    const raw = await readFile(path, 'utf8');
    // Normalize CRLF/CR to LF so the model sees uniform line breaks
    // and the content hash matches what `edit` computes (edit also
    // LF-normalizes via file_io.readFileForEdit).
    const content = raw.replace(/\r\n/g, '\n').replace(/\r/g, '\n');
    const lines = content.split('\n');
    const totalLines = lines.length;
    // Record this read so `edit` can verify the model has actually
    // seen this file. A read covering offset=1 with no limit cap
    // counts as a full read; anything else is partial and won't
    // satisfy the edit pre-read check.
    const partial = !(offset === 1 && limit >= totalLines && around === undefined);
    readState.recordRead(path, content, partial);
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
