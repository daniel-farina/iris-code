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

/** Strip narrator prefixes ("The assistant", "I created", etc.) and
 *  trailing punctuation from a sidecar summary line. */
function cleanSummary(line: string): string {
  return line
    .trim()
    .replace(/^(?:The assistant|I|The tool|The model|The user)\s+/i, '')
    .replace(/[.!?]+$/, '');
}

/** Build a conventional commit message from the running summary
 *  + the user's first prompt. Subject = user's task (capped); body =
 *  bulleted sidecar lines showing what actually happened. This beats
 *  the prior "last-line-only" approach because the last line is often
 *  about minor verification (build/test) rather than the substantive
 *  work. Total subject stays under the 72-char conventional limit. */
export function deriveCommitMessage(
  runningSummary: readonly string[],
  fallbackUserPrompt: string,
): string {
  // Subject: derive from the user's first prompt (what they asked
  // for). That's a better representation of the work than any single
  // sidecar line. Strip the leading verb/imperative if it's too long.
  const promptHead = fallbackUserPrompt
    .trim()
    .split('\n')[0]
    ?.replace(/[.!?]+$/, '')
    .slice(0, 60);
  const subject = `chore(hip): ${promptHead || 'session changes'}`;

  // No summary lines? Just the subject is fine.
  const lines = runningSummary.map(cleanSummary).filter((s) => s.length > 0);
  if (lines.length === 0) return subject;

  // Body: bulleted sidecar lines. Conventional commits use one blank
  // line between subject and body. Cap each bullet so the body stays
  // readable in `git log --oneline` / `git show`. Up to 10 bullets;
  // beyond that the commit is probably TOO chunked anyway.
  const bullets = lines.slice(0, 10).map((l) => `- ${l.slice(0, 140)}`);
  return `${subject}\n\n${bullets.join('\n')}`;
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
