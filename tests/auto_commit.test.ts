// auto_commit tests. Uses a real tmp git repo so spawnSync paths
// exercise the actual git binary.

import { spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { autoCommit, deriveCommitMessage } from '../src/auto_commit.js';

let repo: string;

function git(args: string[], cwd = repo): { code: number; out: string; err: string } {
  const r = spawnSync('git', args, { cwd, encoding: 'utf8' });
  return { code: r.status ?? -1, out: r.stdout, err: r.stderr };
}

beforeEach(() => {
  repo = mkdtempSync(join(tmpdir(), 'hip-autocommit-'));
  // git -c suppresses user.* config requirements
  git(['init', '-q', '-b', 'main']);
  git(['config', 'user.email', 'test@example.com']);
  git(['config', 'user.name', 'Test']);
  git(['commit', '--allow-empty', '-m', 'init']);
});
afterEach(() => {
  rmSync(repo, { recursive: true, force: true });
});

describe('deriveCommitMessage', () => {
  it('uses the LAST running-summary line trimmed of fluff', () => {
    const msg = deriveCommitMessage(
      ['First thing happened', 'The assistant added a new module called foo.js'],
      'fallback prompt',
    );
    expect(msg).toContain('chore(hip):');
    expect(msg).toContain('added a new module called foo.js');
    expect(msg).not.toContain('The assistant');
  });

  it('falls back to the user prompt when no summary lines', () => {
    const msg = deriveCommitMessage([], 'add a beach theme');
    expect(msg).toBe('chore(hip): add a beach theme');
  });

  it('caps the subject under 72 chars', () => {
    const huge = 'x'.repeat(200);
    const msg = deriveCommitMessage([huge], '');
    expect(msg.length).toBeLessThanOrEqual(72);
  });
});

describe('autoCommit', () => {
  it('returns not-a-repo when cwd is not a git repo', () => {
    const noRepo = mkdtempSync(join(tmpdir(), 'hip-noprepo-'));
    const r = autoCommit(noRepo, [], 'whatever');
    expect(r.attempted).toBe(false);
    expect(r.reason).toBe('not a git repo');
    rmSync(noRepo, { recursive: true, force: true });
  });

  it('returns no-changes when working tree is clean', () => {
    const r = autoCommit(repo, [], 'whatever');
    expect(r.attempted).toBe(false);
    expect(r.reason).toBe('no uncommitted changes');
  });

  it('commits dirty changes with a derived message', () => {
    writeFileSync(join(repo, 'foo.js'), 'const x = 1;\n');
    writeFileSync(join(repo, 'bar.js'), 'const y = 2;\n');
    const r = autoCommit(
      repo,
      ['Created foo.js and bar.js with shared constants'],
      'add foo and bar',
    );
    expect(r.attempted).toBe(true);
    expect(r.committed).toBe(true);
    expect(r.message).toContain('Created foo.js and bar.js');
    expect(r.files?.length).toBe(2);

    // Verify git log has the new commit
    const log = git(['log', '--oneline']).out;
    expect(log).toContain('foo.js');
  });

  it('returns committed=false when git status is OK but there are NO actual changes', () => {
    // edge case: spawning .gitignore but not tracking it shouldn't cause spurious commits
    writeFileSync(join(repo, '.gitignore'), 'node_modules\n');
    git(['add', '.gitignore']);
    git(['commit', '-m', 'add gitignore']);
    const r = autoCommit(repo, [], 'noop');
    expect(r.attempted).toBe(false);
    expect(r.reason).toBe('no uncommitted changes');
  });
});
