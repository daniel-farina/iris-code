// Tests for glob, grep, diff - the second wave of tools.

import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { globTool } from '../src/tools/glob.js';
import { grepTool } from '../src/tools/grep.js';
import { diffTool } from '../src/tools/diff.js';
import { REGISTRY } from '../src/tools/index.js';

let tmp: string;

beforeEach(() => {
  tmp = mkdtempSync(join(tmpdir(), 'hip-tools-'));
});
afterEach(() => {
  rmSync(tmp, { recursive: true, force: true });
});

describe('registry', () => {
  it('exposes all 11 tools with unique names', () => {
    const names = REGISTRY.map((t) => t.name);
    expect(names.length).toBe(11);
    expect(new Set(names).size).toBe(11);
    expect(names).toEqual(
      expect.arrayContaining([
        'read',
        'search',
        'grep',
        'glob',
        'edit',
        'bash',
        'list',
        'tree',
        'diff',
        'peek_log',
      ]),
    );
  });
  it('each tool has a function spec with required parameters', () => {
    for (const t of REGISTRY) {
      expect(t.spec.type).toBe('function');
      expect(t.spec.function.name).toBe(t.name);
      expect(t.spec.function.parameters).toBeDefined();
    }
  });
});

describe('glob tool', () => {
  it('matches **/*.txt', async () => {
    mkdirSync(join(tmp, 'sub'));
    writeFileSync(join(tmp, 'a.txt'), 'x');
    writeFileSync(join(tmp, 'sub', 'b.txt'), 'y');
    writeFileSync(join(tmp, 'sub', 'c.md'), 'z');
    const out = await globTool.run({ pattern: '**/*.txt', path: tmp });
    expect(out).toContain('a.txt');
    expect(out).toContain('b.txt');
    expect(out).not.toContain('c.md');
  });

  it('returns no-matches message', async () => {
    const out = await globTool.run({ pattern: '**/*.never', path: tmp });
    expect(out).toContain('no matches');
  });
});

describe('grep tool', () => {
  it('finds hits across files', async () => {
    writeFileSync(join(tmp, 'a.txt'), 'hello\nworld\nfoo');
    writeFileSync(join(tmp, 'b.txt'), 'goodbye\nworld');
    const out = await grepTool.run({ pattern: 'world', path: tmp });
    expect(out).toMatch(/world/);
    expect(out).toMatch(/a\.txt|b\.txt/);
  });

  it('reports no matches cleanly', async () => {
    writeFileSync(join(tmp, 'a.txt'), 'nothing here');
    const out = await grepTool.run({ pattern: 'absent', path: tmp });
    expect(out).toMatch(/no matches/);
  });
});

describe('diff tool', () => {
  it('emits a single + hunk for an insertion', async () => {
    const a = join(tmp, 'a.txt');
    const b = join(tmp, 'b.txt');
    writeFileSync(a, 'line1\nline2\nline3\n');
    writeFileSync(b, 'line1\nline2\nINSERTED\nline3\n');
    const out = await diffTool.run({ path_a: a, path_b: b });
    expect(out).toContain('@@');
    expect(out).toContain('+INSERTED');
  });

  it('emits a - hunk for a deletion', async () => {
    const a = join(tmp, 'a.txt');
    const b = join(tmp, 'b.txt');
    writeFileSync(a, 'keep1\nDELETE_ME\nkeep2\n');
    writeFileSync(b, 'keep1\nkeep2\n');
    const out = await diffTool.run({ path_a: a, path_b: b });
    expect(out).toContain('-DELETE_ME');
  });

  it('reports no differences for identical files', async () => {
    const a = join(tmp, 'a.txt');
    const b = join(tmp, 'b.txt');
    writeFileSync(a, 'same\n');
    writeFileSync(b, 'same\n');
    const out = await diffTool.run({ path_a: a, path_b: b });
    expect(out).toContain('no differences');
  });
});
