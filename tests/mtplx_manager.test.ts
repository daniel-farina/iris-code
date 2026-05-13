import { describe, expect, it } from 'vitest';
import { RECOMMENDED_ENV, diffConfig, formatDiff } from '../src/mtplx_manager.js';

describe('RECOMMENDED_ENV', () => {
  it('includes MTPLX_SESSION_BANK_MAX_ENTRIES=32 (the main lever)', () => {
    expect(RECOMMENDED_ENV.MTPLX_SESSION_BANK_MAX_ENTRIES).toBe('32');
  });
});

describe('diffConfig', () => {
  it('marks recommended keys missing when current env is empty', () => {
    const diff = diffConfig({}, { FOO: 'bar', BAZ: 'qux' });
    expect(diff.missing).toHaveLength(2);
    expect(diff.matching).toHaveLength(0);
    expect(diff.missing.find((m) => m.key === 'FOO')?.current).toBeUndefined();
  });

  it('marks recommended keys missing when current env has wrong value', () => {
    const diff = diffConfig({ FOO: 'wrong' }, { FOO: 'right' });
    expect(diff.missing).toEqual([{ key: 'FOO', current: 'wrong', recommended: 'right' }]);
    expect(diff.matching).toEqual([]);
  });

  it('marks keys matching when value is exact', () => {
    const diff = diffConfig({ FOO: 'bar' }, { FOO: 'bar' });
    expect(diff.missing).toEqual([]);
    expect(diff.matching).toEqual([{ key: 'FOO', value: 'bar' }]);
  });

  it('ignores unrelated env keys in current', () => {
    const diff = diffConfig({ HOME: '/x', FOO: 'bar' }, { FOO: 'bar' });
    expect(diff.matching).toHaveLength(1);
    expect(diff.missing).toHaveLength(0);
  });

  it('handles undefined current map', () => {
    const diff = diffConfig(undefined, { FOO: 'bar' });
    expect(diff.missing).toHaveLength(1);
    expect(diff.missing[0]?.current).toBeUndefined();
  });
});

describe('venv detection regex', () => {
  // The regex is internal but the behavior is critical (the v0.4.1-dev
  // setup flow crashed once because we passed the framework-python
  // instead of the venv python to spawn). Pin the expected match
  // shapes so a future refactor can't silently break it.
  const venvRe = /(\/[^\s]+?)\/lib\/python[\d.]+\/site-packages\//;

  it('extracts /opt/homebrew/var/mtplx/venv-0.3.3 from a typical lsof line', () => {
    const line =
      'Python  25425  dan  txt  REG  1,17  154848  16330493 /opt/homebrew/var/mtplx/venv-0.3.3/lib/python3.13/site-packages/numpy/linalg/_umath_linalg.cpython-313-darwin.so';
    expect(venvRe.exec(line)?.[1]).toBe('/opt/homebrew/var/mtplx/venv-0.3.3');
  });

  it('handles python3.12 too', () => {
    const line = 'Python  1  x  txt  REG  0,0  0  0 /home/x/venv/lib/python3.12/site-packages/foo.so';
    expect(venvRe.exec(line)?.[1]).toBe('/home/x/venv');
  });

  it('does NOT match the framework-python install path (no site-packages segment)', () => {
    const line =
      'Python  1  x  txt  REG  0,0  0  0 /opt/homebrew/Cellar/python@3.13/3.13.13_1/Frameworks/Python.framework/Versions/3.13/Resources/Python.app/Contents/MacOS/Python';
    expect(venvRe.exec(line)).toBeNull();
  });
});

describe('formatDiff', () => {
  it('says MTPLX not running when status.running is false', () => {
    const out = formatDiff(
      { running: false, baseUrl: 'http://127.0.0.1:8088' },
      { missing: [], matching: [] },
    );
    expect(out).toContain('not running');
    expect(out).toContain('http://127.0.0.1:8088');
  });

  it('renders matching + missing sections when running', () => {
    const out = formatDiff(
      {
        running: true,
        baseUrl: 'http://127.0.0.1:8088',
        pid: 42,
        versionHint: 'qwen3-model',
      },
      {
        matching: [{ key: 'A', value: '1' }],
        missing: [{ key: 'B', current: undefined, recommended: '2' }],
      },
    );
    expect(out).toContain('pid 42');
    expect(out).toContain('qwen3-model');
    expect(out).toContain('A=1');
    expect(out).toContain('B: (unset) → 2');
  });

  it('shows previous value when key is set but wrong', () => {
    const out = formatDiff(
      { running: true, baseUrl: 'http://x', pid: 1 },
      { matching: [], missing: [{ key: 'X', current: 'old', recommended: 'new' }] },
    );
    expect(out).toContain('X: old → new');
  });
});
