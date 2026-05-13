// Tests for the `search` tool (ripgrep wrapper). Assumes rg is on PATH
// (we exit with a friendly error if not). The model leans on this tool
// heavily so the output-mode + glob paths matter.

import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { searchTool } from '../src/tools/search.js';

let tmp: string;

beforeEach(() => {
  tmp = mkdtempSync(join(tmpdir(), 'hip-search-'));
});
afterEach(() => {
  rmSync(tmp, { recursive: true, force: true });
});

describe('search tool', () => {
  it('refuses missing pattern', async () => {
    await expect(searchTool.run({})).rejects.toThrow(/missing pattern/);
  });

  it('returns no-matches message when nothing hits', async () => {
    writeFileSync(join(tmp, 'a.txt'), 'hello world');
    const out = await searchTool.run({ pattern: 'zzz-not-here-zzz', path: tmp });
    expect(out).toContain('[no matches]');
  });

  it('files_with_matches mode returns file paths only', async () => {
    writeFileSync(join(tmp, 'one.txt'), 'foo\nbar');
    writeFileSync(join(tmp, 'two.txt'), 'bar\nbaz');
    const out = await searchTool.run({
      pattern: 'bar',
      path: tmp,
      output_mode: 'files_with_matches',
    });
    expect(out).toContain('one.txt');
    expect(out).toContain('two.txt');
    // Should NOT include line content in files_with_matches mode.
    expect(out).not.toContain('foo');
    expect(out).not.toContain('baz');
  });

  it('content mode includes line numbers and matched lines', async () => {
    writeFileSync(join(tmp, 'x.txt'), 'first\nNEEDLE\nthird');
    const out = await searchTool.run({
      pattern: 'NEEDLE',
      path: tmp,
      output_mode: 'content',
    });
    expect(out).toContain('NEEDLE');
    // rg's -n flag adds line numbers; line 2 contains NEEDLE.
    expect(out).toMatch(/:2:/);
  });

  it('content mode with context returns surrounding lines', async () => {
    writeFileSync(join(tmp, 'ctx.txt'), 'one\ntwo\nNEEDLE\nfour\nfive');
    const out = await searchTool.run({
      pattern: 'NEEDLE',
      path: tmp,
      output_mode: 'content',
      context: 1,
    });
    expect(out).toContain('NEEDLE');
    expect(out).toContain('two');
    expect(out).toContain('four');
  });

  it('glob restricts file type', async () => {
    writeFileSync(join(tmp, 'a.js'), 'NEEDLE in js');
    writeFileSync(join(tmp, 'b.ts'), 'NEEDLE in ts');
    const out = await searchTool.run({
      pattern: 'NEEDLE',
      path: tmp,
      glob: '*.ts',
      output_mode: 'files_with_matches',
    });
    expect(out).toContain('b.ts');
    expect(out).not.toContain('a.js');
  });

  it('accepts context as a numeric string (MTPLX qwen3 XML quirk)', async () => {
    writeFileSync(join(tmp, 'x.txt'), 'a\nNEEDLE\nb');
    const out = await searchTool.run({
      pattern: 'NEEDLE',
      path: tmp,
      output_mode: 'content',
      context: '1',
    });
    expect(out).toContain('NEEDLE');
    expect(out).toContain('a');
  });
});
