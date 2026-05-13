import { describe, it, expect } from 'vitest';
import { compactToolLabel } from '../src/ui/tool_label.js';

describe('compactToolLabel', () => {
  it('formats read/list/tree/glob/diff/peek with path', () => {
    expect(compactToolLabel('read', '{"path":"src/main.ts"}')).toBe('read src/main.ts');
    expect(compactToolLabel('list', '{"path":"src"}')).toBe('list src');
    expect(compactToolLabel('tree', '{"path":"."}')).toBe('tree .');
    expect(compactToolLabel('glob', '{"pattern":"**/*.ts"}')).toBe('glob **/*.ts');
    expect(compactToolLabel('diff', '{"path":"a.txt"}')).toBe('diff a.txt');
  });

  it('formats grep/search with pattern and optional path', () => {
    expect(compactToolLabel('grep', '{"pattern":"foo","path":"src"}')).toBe('grep "foo" in src');
    expect(compactToolLabel('grep', '{"pattern":"foo"}')).toBe('grep "foo"');
    expect(compactToolLabel('search', '{"query":"bar"}')).toBe('search "bar"');
  });

  it('formats edit with path', () => {
    expect(compactToolLabel('edit', '{"path":"a.ts","old_string":"x","new_string":"y"}')).toBe(
      'edit a.ts',
    );
  });

  it('formats bash as $ command', () => {
    expect(compactToolLabel('bash', '{"command":"ls -la"}')).toBe('bash $ ls -la');
  });

  it('truncates long bash commands', () => {
    const long = 'echo ' + 'a'.repeat(200);
    const label = compactToolLabel('bash', JSON.stringify({ command: long }));
    expect(label.startsWith('bash $ ')).toBe(true);
    expect(label.length).toBeLessThanOrEqual('bash $ '.length + 80);
  });

  it('formats peek_log with optional filters', () => {
    expect(compactToolLabel('peek_log', '{"n":5}')).toBe('peek_log n=5');
    expect(compactToolLabel('peek_log', '{"failed":"true","n":3}')).toBe(
      'peek_log failed=true n=3',
    );
    expect(compactToolLabel('peek_log', '{}')).toBe('peek_log');
  });

  it('falls back to name + truncated args for unknown tools', () => {
    expect(compactToolLabel('mystery', '{"x":1}')).toBe('mystery {"x":1}');
  });

  it('handles malformed JSON gracefully', () => {
    expect(compactToolLabel('read', 'not json')).toBe('read not json');
  });
});
