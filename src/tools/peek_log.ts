// `peek_log` tool: show the last N entries from hip's session log
// (~/.hip/sessions.jsonl). Lets the model self-inspect prior runs
// when the user references "the last session" or asks for stats. Mirrors
// hip's tools/peek_log.rs concept but reads from hip's JSONL.

import { homeDir, readAllSessions } from '../session_store.js';
import { type Tool, argBool, argU64 } from './types.js';

export const peekLogTool: Tool = {
  name: 'peek_log',
  spec: {
    type: 'function',
    function: {
      name: 'peek_log',
      description:
        'Show the last N hip session log entries (~/.hip/sessions.jsonl). Each line: id, ts, cwd, first user message, message count. Default n=10. Pass failed=true to filter to sessions whose last assistant message looks like an error.',
      parameters: {
        type: 'object',
        properties: {
          n: { type: 'integer', description: 'how many recent entries (default 10, max 100)' },
          failed: { type: 'boolean', description: 'filter to error-looking sessions' },
        },
      },
    },
  },
  async run(args) {
    const n = Math.min(argU64(args, 'n') ?? 10, 100);
    const failedOnly = argBool(args, 'failed') ?? false;
    const all = await readAllSessions();
    if (all.length === 0) return `[no sessions yet] (${homeDir()}/sessions.jsonl is empty)\n`;
    let pool = [...all].reverse();
    if (failedOnly) {
      pool = pool.filter((r) => {
        const last = r.conv[r.conv.length - 1];
        if (!last) return false;
        const txt = typeof last.content === 'string' ? last.content.toLowerCase() : '';
        return last.role === 'assistant' && /error|fail|cannot|unable/i.test(txt);
      });
    }
    const slice = pool.slice(0, n);
    const lines = slice.map((r) => {
      const ts = new Date(r.ts_unix * 1000).toISOString().slice(0, 19).replace('T', ' ');
      const first = r.first_user.replace(/\s+/g, ' ').slice(0, 50);
      return `  ${ts}  ${r.session_id.slice(0, 30).padEnd(30)}  msgs=${String(r.conv.length).padStart(3)}  cwd=${r.cwd}\n    ${first}`;
    });
    return `${slice.length} session(s) (most recent first${failedOnly ? ', failed-only filter active' : ''}):\n${lines.join('\n')}\n`;
  },
};
