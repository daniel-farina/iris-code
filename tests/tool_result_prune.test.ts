import { describe, expect, it } from 'vitest';
import type { ChatMessage } from '../src/schema.js';
import { pruneOldToolResults } from '../src/tool_result_prune.js';

function tool(id: string, name: string, content: string): ChatMessage {
  return { role: 'tool', tool_call_id: id, name, content };
}

describe('pruneOldToolResults', () => {
  it('does nothing when there are fewer tool results than keepLastK', () => {
    const conv: ChatMessage[] = [
      { role: 'user', content: 'hi' },
      tool('c1', 'read', 'x'.repeat(5000)),
      tool('c2', 'read', 'x'.repeat(5000)),
    ];
    const r = pruneOldToolResults(conv, { keepLastK: 3 });
    expect(r.pruned).toBe(0);
    expect(r.charsSaved).toBe(0);
    expect(conv[1]?.content).toHaveLength(5000);
  });

  it('does nothing when older results are small enough', () => {
    const conv: ChatMessage[] = [
      tool('c1', 'list', 'small'),
      tool('c2', 'list', 'small'),
      tool('c3', 'list', 'small'),
      tool('c4', 'list', 'small'),
      tool('c5', 'list', 'small'),
    ];
    const r = pruneOldToolResults(conv, { keepLastK: 3, pruneOverChars: 1500 });
    expect(r.pruned).toBe(0);
    expect(conv[0]?.content).toBe('small');
  });

  it('prunes old large tool results, keeps last K verbatim', () => {
    const big = 'x'.repeat(5000);
    const conv: ChatMessage[] = [
      tool('c1', 'read', big),
      tool('c2', 'read', big),
      tool('c3', 'read', big),
      tool('c4', 'read', big),
      tool('c5', 'read', big),
    ];
    const r = pruneOldToolResults(conv, { keepLastK: 3, pruneOverChars: 1500 });
    expect(r.pruned).toBe(2);
    expect(r.charsSaved).toBeGreaterThan(9000);
    expect(conv[0]?.content).toContain('[redacted:');
    expect(conv[1]?.content).toContain('[redacted:');
    expect(conv[2]?.content).toBe(big);
    expect(conv[3]?.content).toBe(big);
    expect(conv[4]?.content).toBe(big);
  });

  it('preserves tool message structure (tool_call_id, name)', () => {
    const big = 'x'.repeat(5000);
    const conv: ChatMessage[] = [
      tool('c1', 'read', big),
      tool('c2', 'list', big),
      tool('c3', 'search', 'small'),
      tool('c4', 'read', 'small'),
    ];
    pruneOldToolResults(conv, { keepLastK: 1, pruneOverChars: 1000 });
    // c1 + c2 are large + old → pruned. c3 is small. c4 is last → kept.
    expect((conv[0] as { tool_call_id: string }).tool_call_id).toBe('c1');
    expect((conv[0] as { name: string }).name).toBe('read');
    expect(conv[0]?.content).toContain('read');
    expect((conv[1] as { name: string }).name).toBe('list');
    expect(conv[1]?.content).toContain('list');
  });

  it('is idempotent: re-running on a pruned conv does nothing', () => {
    const big = 'x'.repeat(5000);
    const conv: ChatMessage[] = [
      tool('c1', 'read', big),
      tool('c2', 'read', big),
      tool('c3', 'read', big),
      tool('c4', 'read', big),
    ];
    const r1 = pruneOldToolResults(conv, { keepLastK: 2, pruneOverChars: 1500 });
    expect(r1.pruned).toBe(2);
    const r2 = pruneOldToolResults(conv, { keepLastK: 2, pruneOverChars: 1500 });
    expect(r2.pruned).toBe(0);
    expect(r2.charsSaved).toBe(0);
  });

  it('ignores non-tool messages entirely', () => {
    const conv: ChatMessage[] = [
      { role: 'system', content: 'x'.repeat(8000) },
      { role: 'user', content: 'x'.repeat(8000) },
      { role: 'assistant', content: 'x'.repeat(8000) },
      tool('c1', 'read', 'x'.repeat(5000)),
      tool('c2', 'read', 'x'.repeat(5000)),
      tool('c3', 'read', 'x'.repeat(5000)),
      tool('c4', 'read', 'x'.repeat(5000)),
    ];
    pruneOldToolResults(conv, { keepLastK: 3, pruneOverChars: 1500 });
    expect(conv[0]?.content).toHaveLength(8000); // system unchanged
    expect(conv[1]?.content).toHaveLength(8000); // user unchanged
    expect(conv[2]?.content).toHaveLength(8000); // assistant unchanged
    expect(conv[3]?.content).toContain('[redacted:'); // tool 1 (oldest) → pruned
  });
});
