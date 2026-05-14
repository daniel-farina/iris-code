// Background task manager. Spawns long-running shell commands detached
// from the agent loop so dev servers, watchers, build daemons, and log
// tails can run in parallel with hip's main MTPLX stream.
//
// Mirrors the design of claude-code-main's LocalShellTask + TaskOutput
// + TaskStop trio, condensed into a single module-level singleton:
//
//   bgTasks.start(cmd)     → spawn detached, return BgTaskInfo
//   bgTasks.stop(id)       → SIGTERM the process group, fallback SIGKILL
//   bgTasks.list()         → all known tasks (running + recently finished)
//   bgTasks.output(id, n)  → last n lines of stdout+stderr (capped)
//   bgTasks.subscribe(cb)  → notified on any state change (TUI re-render)
//   bgTasks.drainCompletions() → tasks that finished since last drain;
//                                 used to inject `[bg done]` notices into
//                                 the next agent round.
//
// Output is written to /tmp/hip-bg-<id>.log on disk AND kept in a 4KB
// ring buffer in memory for fast tail reads. Disk file outlives the
// hip process so the user can `tail -f` it after exit.

import { spawn } from 'node:child_process';
import { existsSync, openSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const OUTPUT_RING_BYTES = 64 * 1024; // 64KB in-memory tail per task
const MAX_FINISHED_RETAINED = 20; // gc threshold for completed tasks

export type BgTaskStatus = 'running' | 'completed' | 'failed' | 'killed';

export interface BgTaskInfo {
  id: string;
  command: string;
  description?: string;
  status: BgTaskStatus;
  pid?: number;
  startedAt: number;
  endedAt?: number;
  exitCode?: number;
  signal?: string;
  outputPath: string;
  /** Last non-empty line of output - used by the TUI bottom bar. */
  preview: string;
  /** Total bytes written to disk (uncapped). */
  bytesWritten: number;
}

interface BgTaskState extends BgTaskInfo {
  ring: Buffer[]; // chunks; trimmed when total > OUTPUT_RING_BYTES
  ringBytes: number;
  pgid?: number;
  notified: boolean; // drainCompletions consumed it
}

class BackgroundTaskManager {
  private tasks = new Map<string, BgTaskState>();
  private listeners = new Set<() => void>();
  private installedExitHook = false;

  start(command: string, description?: string): BgTaskInfo {
    if (!command || !command.trim()) {
      throw new Error('background_tasks.start: empty command');
    }
    const id = newId();
    const outputPath = join(tmpdir(), `hip-bg-${id}.log`);
    writeFileSync(outputPath, `=== hip background task ${id} ===\n$ ${command}\n\n`);
    const out = openSync(outputPath, 'a');

    // detached: true puts the child in a new process group so we can
    // SIGTERM the whole group (including grandchildren) when the user
    // asks us to stop it - same trick the bash tool uses for timeouts.
    const child = spawn('/bin/sh', ['-c', command], {
      detached: true,
      stdio: ['ignore', 'pipe', 'pipe'],
      cwd: process.cwd(),
      env: process.env,
    });

    const state: BgTaskState = {
      id,
      command,
      description,
      status: 'running',
      pid: child.pid,
      pgid: child.pid, // detached:true makes pid == pgid
      startedAt: Date.now(),
      outputPath,
      preview: '',
      bytesWritten: 0,
      ring: [],
      ringBytes: 0,
      notified: false,
    };
    this.tasks.set(id, state);
    this.installExitHookOnce();

    const onData = (chunk: Buffer) => {
      try {
        require('node:fs').writeSync(out, chunk);
      } catch {
        /* disk full / fd closed - drop */
      }
      state.bytesWritten += chunk.length;
      state.ring.push(chunk);
      state.ringBytes += chunk.length;
      while (state.ringBytes > OUTPUT_RING_BYTES && state.ring.length > 0) {
        const dropped = state.ring.shift()!;
        state.ringBytes -= dropped.length;
      }
      // Cheap preview: last non-empty line of the tail.
      const tail = chunk.toString('utf8');
      const lines = tail.split('\n').map((l) => l.trim()).filter(Boolean);
      const lastLine = lines[lines.length - 1];
      if (lastLine) state.preview = lastLine.slice(0, 80);
      this.emit();
    };
    child.stdout?.on('data', onData);
    child.stderr?.on('data', onData);

    child.on('close', (code, signal) => {
      try {
        require('node:fs').closeSync(out);
      } catch {
        /* already closed */
      }
      if (state.status === 'running') {
        if (signal) {
          state.status = 'killed';
          state.signal = signal;
        } else if (code === 0) {
          state.status = 'completed';
        } else {
          state.status = 'failed';
        }
        state.exitCode = code ?? undefined;
        state.endedAt = Date.now();
      }
      this.gcFinished();
      this.emit();
    });

    child.on('error', (err) => {
      state.status = 'failed';
      state.endedAt = Date.now();
      state.preview = `spawn error: ${err.message}`.slice(0, 80);
      this.emit();
    });

    // Don't block hip's event loop on the child.
    child.unref();
    return toInfo(state);
  }

  stop(id: string): { ok: boolean; message: string } {
    const state = this.tasks.get(id);
    if (!state) return { ok: false, message: `no background task with id ${id}` };
    if (state.status !== 'running') {
      return { ok: false, message: `task ${id} already ${state.status}` };
    }
    const pgid = state.pgid ?? state.pid;
    if (!pgid) return { ok: false, message: `task ${id} has no pid` };
    try {
      // Negative pid = whole process group.
      process.kill(-pgid, 'SIGTERM');
    } catch {
      try {
        process.kill(pgid, 'SIGTERM');
      } catch {
        /* already dead */
      }
    }
    // SIGKILL fallback after 2s if it's still alive.
    setTimeout(() => {
      if (state.status === 'running') {
        try {
          process.kill(-pgid, 'SIGKILL');
        } catch {
          try {
            process.kill(pgid, 'SIGKILL');
          } catch {
            /* gone */
          }
        }
      }
    }, 2000).unref();
    state.status = 'killed';
    state.endedAt = Date.now();
    this.emit();
    return { ok: true, message: `sent SIGTERM to task ${id} (pgid ${pgid})` };
  }

  list(): BgTaskInfo[] {
    return [...this.tasks.values()]
      .sort((a, b) => b.startedAt - a.startedAt)
      .map(toInfo);
  }

  running(): BgTaskInfo[] {
    return this.list().filter((t) => t.status === 'running');
  }

  get(id: string): BgTaskInfo | undefined {
    const s = this.tasks.get(id);
    return s ? toInfo(s) : undefined;
  }

  /** Returns the last `tailLines` lines of merged stdout+stderr. The ring
   *  buffer covers the last ~64KB; if more was written, we fall back to
   *  reading the tail of the disk file. */
  output(id: string, tailLines = 200): {
    found: boolean;
    status?: BgTaskStatus;
    exitCode?: number;
    output: string;
    bytesWritten?: number;
    truncated?: boolean;
  } {
    const state = this.tasks.get(id);
    if (!state) return { found: false, output: '' };
    let text: string;
    let truncated = false;
    if (state.ringBytes >= state.bytesWritten) {
      text = Buffer.concat(state.ring).toString('utf8');
    } else if (existsSync(state.outputPath)) {
      // Disk has more than the ring - re-read tail from file.
      try {
        const full = readFileSync(state.outputPath, 'utf8');
        text = full;
        truncated = full.length < state.bytesWritten;
      } catch {
        text = Buffer.concat(state.ring).toString('utf8');
        truncated = true;
      }
    } else {
      text = Buffer.concat(state.ring).toString('utf8');
    }
    const lines = text.split('\n');
    const tail = lines.slice(Math.max(0, lines.length - tailLines)).join('\n');
    return {
      found: true,
      status: state.status,
      exitCode: state.exitCode,
      output: tail,
      bytesWritten: state.bytesWritten,
      truncated,
    };
  }

  subscribe(cb: () => void): () => void {
    this.listeners.add(cb);
    return () => this.listeners.delete(cb);
  }

  /** Returns and marks-as-notified any tasks that have completed/failed/
   *  killed since the last drain. The agent loop uses this to inject
   *  one-line `[bg done]` notes into the next user turn. */
  drainCompletions(): Array<Pick<BgTaskInfo, 'id' | 'command' | 'status' | 'exitCode'>> {
    const out: Array<Pick<BgTaskInfo, 'id' | 'command' | 'status' | 'exitCode'>> = [];
    for (const state of this.tasks.values()) {
      if (state.status !== 'running' && !state.notified) {
        state.notified = true;
        out.push({
          id: state.id,
          command: state.command,
          status: state.status,
          exitCode: state.exitCode,
        });
      }
    }
    return out;
  }

  killAll(): void {
    for (const state of this.tasks.values()) {
      if (state.status === 'running') this.stop(state.id);
    }
  }

  private emit(): void {
    for (const cb of this.listeners) {
      try {
        cb();
      } catch {
        /* listener bugs are not our problem */
      }
    }
  }

  private gcFinished(): void {
    const finished = [...this.tasks.values()].filter((t) => t.status !== 'running');
    if (finished.length <= MAX_FINISHED_RETAINED) return;
    finished
      .sort((a, b) => (a.endedAt ?? 0) - (b.endedAt ?? 0))
      .slice(0, finished.length - MAX_FINISHED_RETAINED)
      .forEach((t) => this.tasks.delete(t.id));
  }

  private installExitHookOnce(): void {
    if (this.installedExitHook) return;
    this.installedExitHook = true;
    const handler = () => {
      // Best-effort cleanup. Don't block exit waiting for SIGKILL.
      for (const state of this.tasks.values()) {
        if (state.status !== 'running') continue;
        const pgid = state.pgid ?? state.pid;
        if (!pgid) continue;
        try {
          process.kill(-pgid, 'SIGTERM');
        } catch {
          /* already gone */
        }
      }
    };
    process.once('exit', handler);
    process.once('SIGINT', () => {
      handler();
      process.exit(130);
    });
    process.once('SIGTERM', () => {
      handler();
      process.exit(143);
    });
  }
}

function toInfo(s: BgTaskState): BgTaskInfo {
  return {
    id: s.id,
    command: s.command,
    description: s.description,
    status: s.status,
    pid: s.pid,
    startedAt: s.startedAt,
    endedAt: s.endedAt,
    exitCode: s.exitCode,
    signal: s.signal,
    outputPath: s.outputPath,
    preview: s.preview,
    bytesWritten: s.bytesWritten,
  };
}

function newId(): string {
  // 6 hex chars is plenty - we only retain ~20 finished + running at any time.
  return Math.floor(Math.random() * 0xffffff)
    .toString(16)
    .padStart(6, '0');
}

/** Format a duration since startedAt as a short suffix (e.g. "12s", "3m"). */
export function formatDuration(startedAt: number, endedAt?: number): string {
  const ms = (endedAt ?? Date.now()) - startedAt;
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  return `${h}h${m % 60}m`;
}

/** Compact label for the TUI bottom bar. */
export function shortLabel(t: BgTaskInfo, maxLen = 24): string {
  const base = t.description ?? t.command;
  return base.length > maxLen ? `${base.slice(0, maxLen - 1)}…` : base;
}

/** Optional helper for callers that need to read the disk file directly
 *  (e.g. user-facing "tail -f" hint). Safe even after task ends. */
export function getOutputPath(id: string): string {
  return join(tmpdir(), `hip-bg-${id}.log`);
}

/** Lightweight size check used by tools to surface progress. */
export function getOutputSize(id: string): number | undefined {
  const path = getOutputPath(id);
  try {
    return statSync(path).size;
  } catch {
    return undefined;
  }
}

// Module-level singleton. Callers import this directly.
export const bgTasks = new BackgroundTaskManager();
