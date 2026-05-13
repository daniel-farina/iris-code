// Safety-net auto-commit for --print mode. If the model finished
// without running `git commit` itself, hip can sweep up any
// uncommitted changes into a single conventional commit so the user
// has a clean revertable point.
//
// Why this exists: per the bg test 2026-05-13, qwen3.6 sometimes
// ignores the system-prompt "commit as you go" rule entirely - the
// model does meaningful edits but never invokes bash for `git commit`.
// Auto-commit gives the user a fallback without needing to type
// anything.
//
// Opt-in via --auto-commit or HIP_AUTO_COMMIT=1. Never runs unless
// explicitly requested - committing on someone's behalf is irreversible
// (visible in git log) so the default stays off.

import { spawnSync } from 'node:child_process';

export interface AutoCommitResult {
  attempted: boolean;
  committed: boolean;
  reason: string;
  message?: string;
  /** Files added in the commit (best-effort short list). */
  files?: string[];
}

/** Build a conventional commit subject from the running summary +
 *  fallback to the first user prompt. Caps at 72 chars. */
export function deriveCommitMessage(
  runningSummary: readonly string[],
  fallbackUserPrompt: string,
): string {
  // Prefer the last summary line - it's usually the most recent piece
  // of work, which is what we're committing. Trim noise prefixes
  // ("The assistant", "I created", "The tool", etc.).
  const last = runningSummary.at(-1)?.trim();
  if (last) {
    const cleaned = last
      .replace(/^(?:The assistant|I|The tool|The model)\s+/i, '')
      .replace(/[.!?]+$/, '');
    // Subject prefix "chore(hip): " is 12 chars; cap content at 60 so
    // total subject stays under the 72-char conventional-commit limit.
    return `chore(hip): ${cleaned.slice(0, 60)}`;
  }
  const promptHead = fallbackUserPrompt.trim().split('\n')[0]?.slice(0, 60) ?? 'update';
  return `chore(hip): ${promptHead}`;
}

/** Run a sweep commit if cwd is a git repo with uncommitted changes.
 *  Pure side effect - returns a result describing what (if anything)
 *  was committed. NEVER throws. */
export function autoCommit(
  cwd: string,
  runningSummary: readonly string[],
  fallbackUserPrompt: string,
): AutoCommitResult {
  // 1. Is this a git repo?
  const repoCheck = spawnSync('git', ['rev-parse', '--git-dir'], { cwd, encoding: 'utf8' });
  if (repoCheck.status !== 0) {
    return { attempted: false, committed: false, reason: 'not a git repo' };
  }
  // 2. Anything to commit?
  const status = spawnSync('git', ['status', '--porcelain'], { cwd, encoding: 'utf8' });
  if (status.status !== 0) {
    return { attempted: false, committed: false, reason: 'git status failed' };
  }
  const lines = status.stdout
    .split('\n')
    .map((l) => l.trim())
    .filter((l) => l.length > 0);
  if (lines.length === 0) {
    return { attempted: false, committed: false, reason: 'no uncommitted changes' };
  }
  const files = lines.map((l) => l.replace(/^.. /, '')).slice(0, 10);
  const message = deriveCommitMessage(runningSummary, fallbackUserPrompt);
  // 3. Stage + commit.
  const add = spawnSync('git', ['add', '-A'], { cwd, encoding: 'utf8' });
  if (add.status !== 0) {
    return {
      attempted: true,
      committed: false,
      reason: `git add failed: ${add.stderr.slice(0, 200)}`,
    };
  }
  const commit = spawnSync('git', ['commit', '-m', message], { cwd, encoding: 'utf8' });
  if (commit.status !== 0) {
    return {
      attempted: true,
      committed: false,
      reason: `git commit failed: ${commit.stderr.slice(0, 200) || commit.stdout.slice(0, 200)}`,
      message,
      files,
    };
  }
  return { attempted: true, committed: true, reason: 'ok', message, files };
}
