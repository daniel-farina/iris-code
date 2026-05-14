// Unit tests for the tool registry. Run with `bun test` or `vitest`.

import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { readTool } from '../src/tools/read.js';
import { editTool, multiEditTool } from '../src/tools/edit.js';
import { listTool } from '../src/tools/list.js';
import { treeTool } from '../src/tools/tree.js';
import { argU64 } from '../src/tools/types.js';
import { generateAutoSessionId, generateCwdStableSessionId } from '../src/config.js';
import { readState } from '../src/read_state.js';

/** Mark a file as fully read so edit's pre-read requirement passes.
 *  Matches what the real read tool does: normalize CRLF/CR to LF
 *  before hashing so the staleness check lines up with edit's
 *  internally-normalized content. */
function markRead(path: string): void {
  const raw = readFileSync(path, 'utf8');
  const content = raw.replace(/\r\n/g, '\n').replace(/\r/g, '\n');
  readState.recordRead(path, content, false);
}

let tmp: string;

beforeEach(() => {
  tmp = mkdtempSync(join(tmpdir(), 'hip-test-'));
});
afterEach(() => {
  rmSync(tmp, { recursive: true, force: true });
});

describe('argU64', () => {
  it('accepts numbers', () => {
    expect(argU64({ x: 42 }, 'x')).toBe(42);
  });
  it('accepts numeric strings (MTPLX qwen3 XML edge case)', () => {
    expect(argU64({ x: '42' }, 'x')).toBe(42);
    expect(argU64({ x: '  42  ' }, 'x')).toBe(42);
  });
  it('rejects negative', () => {
    expect(argU64({ x: -1 }, 'x')).toBeUndefined();
    expect(argU64({ x: '-1' }, 'x')).toBeUndefined();
  });
  it('returns undefined for missing/invalid', () => {
    expect(argU64({}, 'x')).toBeUndefined();
    expect(argU64({ x: 'abc' }, 'x')).toBeUndefined();
  });
});

describe('read tool', () => {
  it('reads default first 200 lines with markers', async () => {
    const body = Array.from({ length: 10 }, (_, i) => `line${i + 1}`).join('\n');
    const p = join(tmp, 'a.txt');
    writeFileSync(p, body);
    const out = await readTool.run({ path: p });
    expect(out).toContain('    1\tline1');
    expect(out).toContain('   10\tline10');
  });

  it('honors offset (number)', async () => {
    const body = Array.from({ length: 100 }, (_, i) => `line${i + 1}`).join('\n');
    const p = join(tmp, 'b.txt');
    writeFileSync(p, body);
    const out = await readTool.run({ path: p, offset: 50, limit: 3 });
    expect(out).toContain('line50');
    expect(out).toContain('line52');
    expect(out).not.toContain('line1\n');
  });

  it('honors offset as string (MTPLX delivers args as strings sometimes)', async () => {
    const body = Array.from({ length: 100 }, (_, i) => `line${i + 1}`).join('\n');
    const p = join(tmp, 'c.txt');
    writeFileSync(p, body);
    const out = await readTool.run({ path: p, offset: '50', limit: '3' });
    expect(out).toContain('line50');
    expect(out).toContain('line52');
    expect(out).not.toContain('line1\n');
  });

  it('around+context returns symmetric window', async () => {
    const body = Array.from({ length: 50 }, (_, i) => `line${i + 1}`).join('\n');
    const p = join(tmp, 'd.txt');
    writeFileSync(p, body);
    const out = await readTool.run({ path: p, around: 25, context: 3 });
    expect(out).toContain('line22');
    expect(out).toContain('line25');
    expect(out).toContain('line28');
    expect(out).not.toContain('line29');
    expect(out).toContain('>   25\tline25');
  });
});

describe('edit tool', () => {
  beforeEach(() => {
    readState.clear();
  });

  it('replaces unique match', async () => {
    const p = join(tmp, 'e.txt');
    writeFileSync(p, 'before\nold_string here\nafter');
    markRead(p);
    const r = await editTool.run({ path: p, old_string: 'old_string here', new_string: 'NEW' });
    expect(r).toContain('[ok]');
  });

  it('refuses missing match', async () => {
    const p = join(tmp, 'f.txt');
    writeFileSync(p, 'hello');
    markRead(p);
    await expect(editTool.run({ path: p, old_string: 'xyz', new_string: 'q' })).rejects.toThrow(
      /not found/,
    );
  });

  it('refuses ambiguous unless replace_all', async () => {
    const p = join(tmp, 'g.txt');
    writeFileSync(p, 'foo\nfoo\nfoo');
    markRead(p);
    await expect(editTool.run({ path: p, old_string: 'foo', new_string: 'bar' })).rejects.toThrow(
      /matches 3 places/,
    );
    markRead(p); // file hasn't changed but the test reuses
    const r = await editTool.run({
      path: p,
      old_string: 'foo',
      new_string: 'bar',
      replace_all: true,
    });
    expect(r).toContain('3 edits applied');
  });

  it('returns [no-op] when the edit is already applied', async () => {
    const p = join(tmp, 'noop.txt');
    writeFileSync(p, 'before\nNEW\nafter');
    markRead(p);
    const r = await editTool.run({
      path: p,
      old_string: 'old_string here',
      new_string: 'NEW',
    });
    expect(r).toContain('[no-op]');
    expect(readFileSync(p, 'utf8')).toBe('before\nNEW\nafter');
  });

  it('matches across curly-to-straight quote normalization', async () => {
    const p = join(tmp, 'curly.md');
    writeFileSync(p, 'const msg = “hello”;');
    markRead(p);
    const r = await editTool.run({
      path: p,
      old_string: 'const msg = "hello";',
      new_string: 'const msg = "world";',
    });
    expect(r).toContain('[ok]');
    // Curly quotes preserved on write.
    expect(readFileSync(p, 'utf8')).toBe('const msg = “world”;');
  });

  it('matches CRLF file with LF needle, preserves file line endings', async () => {
    const p = join(tmp, 'crlf.txt');
    writeFileSync(p, 'alpha\r\nbeta\r\ngamma\r\n');
    markRead(p);
    const r = await editTool.run({
      path: p,
      old_string: 'beta',
      new_string: 'BETA',
    });
    expect(r).toContain('[ok]');
    expect(readFileSync(p, 'utf8')).toBe('alpha\r\nBETA\r\ngamma\r\n');
  });

  it('requires the file to be read first', async () => {
    const p = join(tmp, 'unread.txt');
    writeFileSync(p, 'hello world');
    // No markRead call - this should fail.
    await expect(
      editTool.run({ path: p, old_string: 'hello', new_string: 'hi' }),
    ).rejects.toThrow(/has not been read/);
  });

  it('accepts a partial read as sufficient (hash still catches stale)', async () => {
    const p = join(tmp, 'partial.txt');
    writeFileSync(p, 'a\nb\nc\nd\n');
    // Simulate a partial read - readState records the FULL content
    // (matching what the real read tool does) but flags partial=true.
    const content = readFileSync(p, 'utf8');
    readState.recordRead(p, content, true);
    const r = await editTool.run({ path: p, old_string: 'b', new_string: 'B' });
    expect(r).toContain('[ok]');
  });

  it('rejects edits when file has changed since read', async () => {
    const p = join(tmp, 'stale.txt');
    writeFileSync(p, 'original');
    markRead(p);
    // External modification (linter, user) between read and edit.
    writeFileSync(p, 'changed');
    await expect(
      editTool.run({ path: p, old_string: 'changed', new_string: 'new' }),
    ).rejects.toThrow(/modified since you last read/);
  });

  it('creates a new file when old_string is "" on missing path', async () => {
    const p = join(tmp, 'new.txt');
    const r = await editTool.run({
      path: p,
      old_string: '',
      new_string: 'hello world\n',
    });
    expect(r).toContain('[ok] created');
    expect(readFileSync(p, 'utf8')).toBe('hello world\n');
  });

  it('suggests a similar filename when path is missing', async () => {
    writeFileSync(join(tmp, 'main.ts'), 'export {};');
    await expect(
      editTool.run({ path: join(tmp, 'mian.ts'), old_string: 'x', new_string: 'y' }),
    ).rejects.toThrow(/did you mean/);
  });

  it('refuses to edit .ipynb files', async () => {
    const p = join(tmp, 'nb.ipynb');
    writeFileSync(p, '{}');
    markRead(p);
    await expect(
      editTool.run({ path: p, old_string: '{}', new_string: '{"a":1}' }),
    ).rejects.toThrow(/Jupyter notebook/);
  });

  it('normalizes leading whitespace (tabs ↔ spaces)', async () => {
    const p = join(tmp, 'tabs.txt');
    writeFileSync(p, '\tfoo bar\n\tbaz');
    markRead(p);
    // Model emits with 2-space indent instead of tabs.
    const r = await editTool.run({
      path: p,
      old_string: '  foo bar',
      new_string: '\tFOO bar',
    });
    expect(r).toContain('[ok]');
    expect(readFileSync(p, 'utf8')).toBe('\tFOO bar\n\tbaz');
  });

  it('validates JSON post-edit and refuses invalid result', async () => {
    const p = join(tmp, 'settings.json');
    writeFileSync(p, '{"key": "value"}');
    markRead(p);
    await expect(
      editTool.run({
        path: p,
        old_string: '"key": "value"',
        new_string: '"key": invalid',
      }),
    ).rejects.toThrow(/invalid JSON/);
    // File is unchanged.
    expect(readFileSync(p, 'utf8')).toBe('{"key": "value"}');
  });

  it('returns a unified diff in the success message', async () => {
    const p = join(tmp, 'diff.txt');
    writeFileSync(p, 'line one\nline two\nline three\n');
    markRead(p);
    const r = await editTool.run({
      path: p,
      old_string: 'line two',
      new_string: 'line TWO',
    });
    expect(r).toContain('@@');
    expect(r).toContain('-line two');
    expect(r).toContain('+line TWO');
  });
});

describe('multi_edit tool', () => {
  beforeEach(() => {
    readState.clear();
  });

  it('applies a sequence of edits atomically', async () => {
    const p = join(tmp, 'multi.ts');
    writeFileSync(p, 'export function foo() {}\nexport function bar() {}\n');
    markRead(p);
    const r = await multiEditTool.run({
      path: p,
      edits: [
        { old_string: 'foo()', new_string: 'foo(x: number)' },
        { old_string: 'bar()', new_string: 'bar(y: string)' },
      ],
    });
    expect(r).toContain('2 edits applied');
    expect(readFileSync(p, 'utf8')).toBe(
      'export function foo(x: number) {}\nexport function bar(y: string) {}\n',
    );
  });

  it('aborts on first failure - no partial writes', async () => {
    const p = join(tmp, 'abort.ts');
    const original = 'a\nb\nc\n';
    writeFileSync(p, original);
    markRead(p);
    await expect(
      multiEditTool.run({
        path: p,
        edits: [
          { old_string: 'a', new_string: 'A' },
          { old_string: 'NOPE', new_string: 'X' },
          { old_string: 'c', new_string: 'C' },
        ],
      }),
    ).rejects.toThrow(/not found/);
    // No partial write - file is unchanged.
    expect(readFileSync(p, 'utf8')).toBe(original);
  });

  it('accepts edits as a JSON-encoded string (qwen3 tool-call quirk)', async () => {
    const p = join(tmp, 'jsonstr.ts');
    writeFileSync(p, 'a\nb\n');
    markRead(p);
    const editsJson = JSON.stringify([
      { old_string: 'a', new_string: 'A' },
      { old_string: 'b', new_string: 'B' },
    ]);
    const r = await multiEditTool.run({ path: p, edits: editsJson });
    expect(r).toContain('[ok]');
    expect(readFileSync(p, 'utf8')).toBe('A\nB\n');
  });

  it('later edits see results of earlier edits', async () => {
    const p = join(tmp, 'chain.txt');
    writeFileSync(p, 'foo\n');
    markRead(p);
    const r = await multiEditTool.run({
      path: p,
      edits: [
        { old_string: 'foo', new_string: 'bar' },
        { old_string: 'bar', new_string: 'baz' },
      ],
    });
    expect(r).toContain('[ok]');
    expect(readFileSync(p, 'utf8')).toBe('baz\n');
  });
});

describe('list tool', () => {
  it('lists files + dirs', async () => {
    writeFileSync(join(tmp, 'a.txt'), 'hi');
    const out = await listTool.run({ path: tmp });
    expect(out).toContain('a.txt');
    expect(out).toMatch(/file.*a\.txt/);
  });
});

describe('tree tool', () => {
  it('respects depth', async () => {
    writeFileSync(join(tmp, 'top.txt'), 'hi');
    const out = await treeTool.run({ path: tmp, depth: 1 });
    expect(out).toContain('top.txt');
  });
});

describe('session id', () => {
  it('generateAutoSessionId matches auto-YYYYMMDD-HHMMSS-<hex> format', () => {
    const id = generateAutoSessionId();
    expect(id).toMatch(/^auto-\d{8}-\d{6}-[0-9a-f]{6}$/);
  });

  it('generateCwdStableSessionId is deterministic for the same cwd same day', () => {
    const a = generateCwdStableSessionId('/Users/dan/some/project');
    const b = generateCwdStableSessionId('/Users/dan/some/project');
    expect(a).toBe(b);
    expect(a).toMatch(/^cwd-[0-9a-f]{8}-\d{8}$/);
  });

  it('generateCwdStableSessionId differs across different cwds', () => {
    const a = generateCwdStableSessionId('/Users/dan/project-a');
    const b = generateCwdStableSessionId('/Users/dan/project-b');
    expect(a).not.toBe(b);
  });

  it('generateCwdStableSessionId falls back when cwd is empty', () => {
    const id = generateCwdStableSessionId('');
    expect(id).toMatch(/^auto-/); // fell through to generateAutoSessionId
  });
});
