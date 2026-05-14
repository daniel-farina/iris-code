// Tests for recoverInlineToolCalls - the qwen3 fallback parser that
// extracts tool_calls embedded in plain text content (the model
// sometimes emits them through the wrong channel).

import { describe, expect, it } from 'vitest';
import { recoverInlineToolCalls } from '../src/client.js';

describe('recoverInlineToolCalls', () => {
  it('recovers <function=name></function> with empty args (the bug we hit)', () => {
    const content = `Done with task 1.\n<tool_call>\n<function=task_next>\n</tool_call>`;
    const r = recoverInlineToolCalls(content);
    expect(r.recovered).toHaveLength(1);
    expect(r.recovered[0]!.function.name).toBe('task_next');
    expect(r.recovered[0]!.function.arguments).toBe('{}');
    expect(r.strippedContent).toBe('Done with task 1.');
  });

  it('recovers <function=name>{json}</function> with args', () => {
    const content = `<tool_call><function=task_update>{"id":"1","status":"completed"}</function></tool_call>`;
    const r = recoverInlineToolCalls(content);
    expect(r.recovered).toHaveLength(1);
    expect(r.recovered[0]!.function.name).toBe('task_update');
    expect(JSON.parse(r.recovered[0]!.function.arguments)).toEqual({
      id: '1',
      status: 'completed',
    });
  });

  it('recovers JSON-shaped {"name":..., "arguments":...} body', () => {
    const content = `<tool_call>{"name":"read","arguments":{"path":"src/foo.ts"}}</tool_call>`;
    const r = recoverInlineToolCalls(content);
    expect(r.recovered).toHaveLength(1);
    expect(r.recovered[0]!.function.name).toBe('read');
    expect(JSON.parse(r.recovered[0]!.function.arguments)).toEqual({ path: 'src/foo.ts' });
  });

  it('recovers multiple tool_call blocks in one content', () => {
    const content =
      `<tool_call><function=task_update>{"id":"1","status":"completed"}</function></tool_call>\n` +
      `<tool_call><function=task_next></tool_call>`;
    const r = recoverInlineToolCalls(content);
    expect(r.recovered).toHaveLength(2);
    expect(r.recovered[0]!.function.name).toBe('task_update');
    expect(r.recovered[1]!.function.name).toBe('task_next');
  });

  it('returns empty + original content when no tool_call XML present', () => {
    const content = `Just regular assistant text, nothing to recover.`;
    const r = recoverInlineToolCalls(content);
    expect(r.recovered).toHaveLength(0);
    expect(r.strippedContent).toBe(content);
  });
});
