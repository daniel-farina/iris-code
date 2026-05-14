// Background task tools: bg_run, bg_list, bg_output, bg_stop. Thin
// wrappers around src/background_tasks.ts. Designed for the same shape
// as claude-code's Bash(run_in_background=true) + TaskOutput + TaskStop
// trio, but split into separate tools so the model can call them
// independently and the TUI can render distinct labels.

import {
  type BgTaskInfo,
  bgTasks,
  formatDuration,
  getOutputPath,
} from '../background_tasks.js';
import { argString, argU64, type Tool } from './types.js';

function fmtTask(t: BgTaskInfo): string {
  const dur = formatDuration(t.startedAt, t.endedAt);
  const exit =
    t.exitCode !== undefined ? ` exit=${t.exitCode}` : t.signal ? ` signal=${t.signal}` : '';
  const desc = t.description ? ` (${t.description})` : '';
  return `${t.id}  ${t.status.padEnd(9)}  ${dur.padStart(5)}${exit}  ${t.command}${desc}`;
}

export const bgRunTool: Tool = {
  name: 'bg_run',
  spec: {
    type: 'function',
    function: {
      name: 'bg_run',
      description:
        "Start a long-running shell command in the background. Use for dev servers, watchers, builds, log tails. NOT for commands <5s (use `bash`). You're notified when it completes.",
      parameters: {
        type: 'object',
        properties: {
          command: { type: 'string' },
          description: { type: 'string', description: 'label for the TUI bar' },
        },
        required: ['command'],
      },
    },
  },
  async run(args) {
    const command = argString(args, 'command');
    if (!command) throw new Error('bg_run: missing command');
    const description = argString(args, 'description');
    const info = bgTasks.start(command, description);
    return [
      `started bg task ${info.id} (pid ${info.pid ?? '?'})`,
      `  $ ${info.command}`,
      `  output: ${info.outputPath}`,
      `  use bg_output(id="${info.id}") to read, bg_stop(id="${info.id}") to kill`,
    ].join('\n');
  },
};

export const bgListTool: Tool = {
  name: 'bg_list',
  spec: {
    type: 'function',
    function: {
      name: 'bg_list',
      description: 'List background tasks (running + recently finished).',
      parameters: { type: 'object', properties: {}, required: [] },
    },
  },
  async run() {
    const list = bgTasks.list();
    if (list.length === 0) return 'no background tasks';
    const header = 'id      status     dur     exit  command';
    return [header, ...list.map(fmtTask)].join('\n');
  },
};

export const bgOutputTool: Tool = {
  name: 'bg_output',
  spec: {
    type: 'function',
    function: {
      name: 'bg_output',
      description:
        'Read the last N lines of a bg task (default 200, max 2000). Returns status + exit code + tail.',
      parameters: {
        type: 'object',
        properties: {
          id: { type: 'string', description: 'bg task id' },
          tail_lines: { type: 'integer', description: 'default 200, max 2000' },
        },
        required: ['id'],
      },
    },
  },
  async run(args) {
    const id = argString(args, 'id');
    if (!id) throw new Error('bg_output: missing id');
    const tail = Math.min(argU64(args, 'tail_lines') ?? 200, 2000);
    const r = bgTasks.output(id, tail);
    if (!r.found) return `no background task with id ${id}`;
    const meta =
      `[task ${id} · ${r.status}` +
      (r.exitCode !== undefined ? ` · exit ${r.exitCode}` : '') +
      (r.bytesWritten !== undefined ? ` · ${r.bytesWritten}B written` : '') +
      `]`;
    const truncNote = r.truncated ? '\n[... earlier output truncated ...]' : '';
    const body = r.output.trim().length > 0 ? r.output : '(no output yet)';
    return `${meta}${truncNote}\n${body}\n[full log: ${getOutputPath(id)}]`;
  },
};

export const bgStopTool: Tool = {
  name: 'bg_stop',
  spec: {
    type: 'function',
    function: {
      name: 'bg_stop',
      description: 'Stop a bg task (SIGTERM → SIGKILL fallback). No-op if finished.',
      parameters: {
        type: 'object',
        properties: {
          id: { type: 'string', description: 'bg task id' },
        },
        required: ['id'],
      },
    },
  },
  async run(args) {
    const id = argString(args, 'id');
    if (!id) throw new Error('bg_stop: missing id');
    const r = bgTasks.stop(id);
    return r.message;
  },
};
