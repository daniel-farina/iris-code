import { describe, it, expect } from 'vitest';
import { emptyStats, formatStats, formatPerTool } from '../src/ui/stats.js';

describe('stats', () => {
  it('emptyStats returns zeroed counters and empty perTool', () => {
    const s = emptyStats();
    expect(s.rounds).toBe(0);
    expect(s.toolCalls).toBe(0);
    expect(s.perTool).toEqual({});
  });

  it('formatStats renders core counters', () => {
    const s = emptyStats();
    s.rounds = 3;
    s.toolCalls = 5;
    s.completionTokens = 100;
    s.totalMs = 1234;
    const out = formatStats(s);
    expect(out).toContain('rounds=3');
    expect(out).toContain('tool_calls=5');
    expect(out).toContain('tokens=100');
    expect(out).toContain('ms=1234');
  });

  it('formatPerTool sorts by call count descending', () => {
    const s = emptyStats();
    s.perTool = {
      read: { calls: 2, errors: 0, totalMs: 50 },
      grep: { calls: 10, errors: 1, totalMs: 200 },
      edit: { calls: 5, errors: 0, totalMs: 75 },
    };
    const out = formatPerTool(s);
    // grep should come first (highest calls).
    const grepIdx = out.indexOf('grep');
    const editIdx = out.indexOf('edit');
    const readIdx = out.indexOf('read');
    expect(grepIdx).toBeLessThan(editIdx);
    expect(editIdx).toBeLessThan(readIdx);
    // Errors marker only shown when err > 0.
    expect(out).toContain('err=1');
    // Average is computed (200/10 = 20).
    expect(out).toContain('avg=  20ms');
  });

  it('formatPerTool says no calls when empty', () => {
    const s = emptyStats();
    expect(formatPerTool(s)).toMatch(/no tool calls/i);
  });
});
