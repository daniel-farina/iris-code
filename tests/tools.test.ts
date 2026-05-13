// Unit tests for the tool registry. Run with `bun test` or `vitest`.

import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { readTool } from '../src/tools/read.js';
import { editTool } from '../src/tools/edit.js';
import { listTool } from '../src/tools/list.js';
import { treeTool } from '../src/tools/tree.js';
import { argU64 } from '../src/tools/types.js';
import { generateAutoSessionId, generateCwdStableSessionId } from '../src/config.js';

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
  it('replaces unique match', async () => {
    const p = join(tmp, 'e.txt');
    writeFileSync(p, 'before\nold_string here\nafter');
    const r = await editTool.run({ path: p, old_string: 'old_string here', new_string: 'NEW' });
    expect(r).toContain('[ok] edited');
  });

  it('refuses missing match', async () => {
    const p = join(tmp, 'f.txt');
    writeFileSync(p, 'hello');
    await expect(editTool.run({ path: p, old_string: 'xyz', new_string: 'q' })).rejects.toThrow(
      /not found/,
    );
  });

  it('refuses ambiguous unless replace_all', async () => {
    const p = join(tmp, 'g.txt');
    writeFileSync(p, 'foo\nfoo\nfoo');
    await expect(editTool.run({ path: p, old_string: 'foo', new_string: 'bar' })).rejects.toThrow(
      /matches 3 places/,
    );
    const r = await editTool.run({
      path: p,
      old_string: 'foo',
      new_string: 'bar',
      replace_all: true,
    });
    expect(r).toContain('3 replacement');
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
