// `bash` tool: shell out with a timeout. Mirrors hip's tools/bash.rs:
// default timeout 30s, stdout+stderr merged, returns the combined output.

import { spawn } from 'node:child_process';
import { type Tool, argString, argU64 } from './types.js';

export const bashTool: Tool = {
  name: 'bash',
  spec: {
    type: 'function',
    function: {
      name: 'bash',
      description:
        'Run a shell command (combined stdout+stderr, 30s default timeout, output capped at last 16KB). For long-running commands use `bg_run`.',
      parameters: {
        type: 'object',
        properties: {
          command: { type: 'string', description: 'shell command to run' },
          timeout_s: {
            type: 'integer',
            description: 'timeout in seconds (default 30, max 600)',
          },
        },
        required: ['command'],
      },
    },
  },
  async run(args) {
    const cmd = argString(args, 'command');
    if (!cmd) throw new Error('bash: missing command');
    const timeout_s = Math.min(argU64(args, 'timeout_s') ?? 30, 600);

    return new Promise<string>((resolve) => {
      // detached: true creates a new process group so we can SIGKILL
      // the whole group when the timeout fires. Otherwise on Linux,
      // `/bin/sh -c "sleep 5"` may fork sleep as a child; killing the
      // shell alone leaves sleep running and stdio pipes open, so
      // child.on('close') waits until sleep exits naturally.
      const child = spawn('/bin/sh', ['-c', cmd], {
        stdio: ['ignore', 'pipe', 'pipe'],
        detached: true,
      });
      const chunks: Buffer[] = [];
      let timedOut = false;
      const timer = setTimeout(() => {
        timedOut = true;
        try {
          if (child.pid) process.kill(-child.pid, 'SIGKILL'); // negative pid = whole group
        } catch {
          // Group may already be dead; fall back to direct kill.
          child.kill('SIGKILL');
        }
      }, timeout_s * 1000);

      child.stdout.on('data', (d: Buffer) => chunks.push(d));
      child.stderr.on('data', (d: Buffer) => chunks.push(d));
      child.on('close', (code, sig) => {
        clearTimeout(timer);
        let out = Buffer.concat(chunks).toString('utf8');
        // Truncate to last 16KB so a runaway command doesn't blow context.
        const CAP = 16 * 1024;
        if (out.length > CAP)
          out = `[... truncated ${out.length - CAP} bytes ...]\n${out.slice(-CAP)}`;
        const status = timedOut
          ? `[killed: timeout after ${timeout_s}s]`
          : sig
            ? `[exit signal ${sig}]`
            : `[exit ${code ?? 0}]`;
        resolve(`${out}${out.endsWith('\n') ? '' : '\n'}${status}\n`);
      });
    });
  },
};
