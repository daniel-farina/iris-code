// Constructor tests for schema.ts. These are trivial functions but the
// wire format is shared with Rust hip - if a field name changes the model
// will silently get confused. Tests pin the exact shape.

import { describe, it, expect } from 'vitest';
import {
  systemMessage,
  userMessage,
  assistantText,
  assistantToolCalls,
  toolResultMessage,
} from '../src/schema.js';

describe('schema constructors', () => {
  it('systemMessage shape', () => {
    expect(systemMessage('hi')).toEqual({ role: 'system', content: 'hi' });
  });

  it('userMessage shape', () => {
    expect(userMessage('hi')).toEqual({ role: 'user', content: 'hi' });
  });

  it('assistantText shape', () => {
    expect(assistantText('ok')).toEqual({ role: 'assistant', content: 'ok' });
  });

  it('assistantToolCalls keeps content as null when empty (OpenAI parity)', () => {
    const m = assistantToolCalls('', [
      { id: 'c1', type: 'function', function: { name: 'list', arguments: '{}' } },
    ]);
    expect(m.role).toBe('assistant');
    expect(m.content).toBe(null);
    expect(m.tool_calls).toHaveLength(1);
  });

  it('assistantToolCalls preserves non-empty text', () => {
    const m = assistantToolCalls('reasoning here', [
      { id: 'c2', type: 'function', function: { name: 'read', arguments: '{}' } },
    ]);
    expect(m.content).toBe('reasoning here');
  });

  it('toolResultMessage threads call_id and name back', () => {
    const m = toolResultMessage('c-abc', 'grep', '0 hits');
    expect(m).toEqual({
      role: 'tool',
      tool_call_id: 'c-abc',
      name: 'grep',
      content: '0 hits',
    });
  });
});
