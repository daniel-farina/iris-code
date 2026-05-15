// Session lock: prevents two `hip --print --resume <id>` runs from
// hitting the same session concurrently. Concurrent writes raced on
// our v7 game session and dropped task-state updates on the floor.
// Lock file is `~/.hip/locks/<session_id>.lock` containing the pid.
// A stale lock (pid no longer running) is silently reclaimed.

import { existsSync, mkdirSync, readFileSync, unlinkSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

function lockDir(): string {
  const d = join(homedir(), '.hip', 'locks');
  if (!existsSync(d)) mkdirSync(d, { recursive: true });
  return d;
}

function lockPath(sessionId: string): string {
  return join(lockDir(), `${sessionId}.lock`);
}

function pidAlive(pid: number): boolean {
  if (!Number.isInteger(pid) || pid <= 0) return false;
  try {
    process.kill(pid, 0); // signal 0 = existence check, no actual kill
    return true;
  } catch {
    return false;
  }
}

export interface LockHandle {
  release(): void;
}

export interface LockAcquireFailure {
  reason: 'held';
  holderPid: number;
  path: string;
}

/** Try to acquire an exclusive lock on this session_id.
 *  Returns the LockHandle on success, or a failure descriptor when
 *  another live hip process is holding it. Stale locks (dead pid)
 *  are silently overwritten. */
export function tryAcquireSessionLock(
  sessionId: string,
): LockHandle | LockAcquireFailure {
  const path = lockPath(sessionId);
  if (existsSync(path)) {
    try {
      const raw = readFileSync(path, 'utf8').trim();
      const holderPid = Number.parseInt(raw, 10);
      if (pidAlive(holderPid) && holderPid !== process.pid) {
        return { reason: 'held', holderPid, path };
      }
      // Stale lock: holder is dead, reclaim it.
    } catch {
      // Unreadable lockfile is treated as stale.
    }
  }
  writeFileSync(path, String(process.pid), 'utf8');
  return {
    release() {
      try {
        // Only unlink if we still own it (defensive; another run could
        // have overwritten after declaring us stale).
        if (existsSync(path)) {
          const raw = readFileSync(path, 'utf8').trim();
          if (Number.parseInt(raw, 10) === process.pid) {
            unlinkSync(path);
          }
        }
      } catch {
        /* best-effort */
      }
    },
  };
}
