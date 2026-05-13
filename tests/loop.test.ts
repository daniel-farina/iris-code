// /loop parser tests. Matches the Claude Code /loop skill grammar exactly:
//   - leading token "^\d+[smhd]$" wins
//   - else trailing "every Nu" / "every N <unit-word>"
//   - else dynamic mode (return null)

import { describe, expect, it } from 'vitest';
import { parseLoopInput, scheduleLoop, stopAllLoops, listLoops } from '../src/ui/loop.js';

describe('parseLoopInput', () => {
  it('leading token 5m', () => {
    const p = parseLoopInput('5m run smoke tests');
    expect(p).toMatchObject({
      intervalMs: 5 * 60 * 1000,
      cadence: 'every 5 minutes',
      prompt: 'run smoke tests',
    });
  });

  it('leading token 30s minimum boundary', () => {
    expect(parseLoopInput('30s ping')).toMatchObject({
      intervalMs: 30_000,
      cadence: 'every 30 seconds',
      prompt: 'ping',
    });
  });

  it('rejects intervals under 5s', () => {
    expect(parseLoopInput('2s x')).toEqual({ error: 'minimum interval is 5s' });
  });

  it('leading token 2h', () => {
    expect(parseLoopInput('2h hourly check')).toMatchObject({
      intervalMs: 2 * 60 * 60 * 1000,
      cadence: 'every 2 hours',
      prompt: 'hourly check',
    });
  });

  it('trailing "every Nu" form', () => {
    expect(parseLoopInput('check the deploy every 20m')).toMatchObject({
      intervalMs: 20 * 60 * 1000,
      cadence: 'every 20 minutes',
      prompt: 'check the deploy',
    });
  });

  it('trailing "every N <unit-word>" form', () => {
    expect(parseLoopInput('run tests every 5 minutes')).toMatchObject({
      intervalMs: 5 * 60 * 1000,
      cadence: 'every 5 minutes',
      prompt: 'run tests',
    });
  });

  it('trailing "every N hours"', () => {
    expect(parseLoopInput('build every 2 hours')).toMatchObject({
      intervalMs: 2 * 3600 * 1000,
      prompt: 'build',
    });
  });

  it('no interval -> dynamic mode (null)', () => {
    expect(parseLoopInput('check the deploy')).toBeNull();
  });

  it('"every PR" is NOT a time expression - dynamic', () => {
    expect(parseLoopInput('check every PR')).toBeNull();
  });

  it('empty input', () => {
    expect(parseLoopInput('   ')).toEqual({ error: 'usage: /loop [interval] <prompt>' });
  });

  it('leading interval with no prompt', () => {
    expect(parseLoopInput('5m')).toBeNull();
  });

  it('multiline prompt body', () => {
    const p = parseLoopInput('10s line1\nline2\nline3');
    expect(p).toMatchObject({
      intervalMs: 10_000,
      prompt: 'line1\nline2\nline3',
    });
  });
});

describe('scheduleLoop runtime', () => {
  it('fires on schedule and is cancellable', async () => {
    let fires = 0;
    const job = scheduleLoop(20, 'every 20 ms', 'x', () => {
      fires++;
    });
    expect(listLoops()).toHaveLength(1);
    await new Promise((r) => setTimeout(r, 75));
    expect(fires).toBeGreaterThanOrEqual(2);
    const stopped = stopAllLoops();
    expect(stopped).toBe(1);
    expect(listLoops()).toHaveLength(0);
    // After stop, dispatch should not fire again.
    const before = fires;
    await new Promise((r) => setTimeout(r, 40));
    expect(fires).toBe(before);
    // Reference job to silence unused
    expect(job.id).toMatch(/^[0-9a-f]{8}$/);
  });
});
