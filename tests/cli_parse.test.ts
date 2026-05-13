// Regression tests for CLI flag parsing. The big footgun is `--print
// --quiet "prompt"`: node:parseArgs treats string options as eagerly
// consuming the next token, so historically `--print --quiet ...` parsed
// as print="--quiet" and dropped the prompt. We switched --print to a
// boolean flag that always reads the prompt from positionals.

import { describe, it, expect } from 'vitest';
import { parseFlags } from '../src/cli.js';

describe('parseFlags', () => {
  it('reads prompt from positionals when --print is passed', () => {
    const f = parseFlags(['--print', 'hello', 'world']);
    expect(f.print).toBe('hello world');
  });

  it('--print --quiet "prompt" works in either flag order', () => {
    expect(parseFlags(['--print', '--quiet', 'foo bar']).print).toBe('foo bar');
    expect(parseFlags(['--quiet', '--print', 'baz']).print).toBe('baz');
    expect(parseFlags(['--print', '--quiet', 'foo']).quiet).toBe(true);
    expect(parseFlags(['--quiet', '--print', 'foo']).quiet).toBe(true);
  });

  it('bare --print yields empty prompt (caller should error)', () => {
    const f = parseFlags(['--print']);
    expect(f.print).toBe('');
  });

  it('no --print yields undefined prompt (TUI mode)', () => {
    const f = parseFlags([]);
    expect(f.print).toBeUndefined();
  });

  it('--one-shot is an alias for --print', () => {
    const f = parseFlags(['--one-shot', 'hi']);
    expect(f.print).toBe('hi');
  });

  it('numeric flags are coerced to Number', () => {
    const f = parseFlags(['--max-tokens', '2048', '--max-rounds', '15']);
    expect(f.maxTokens).toBe(2048);
    expect(f.maxRounds).toBe(15);
  });

  it('short flags work: -V, -h, -c, -q', () => {
    expect(parseFlags(['-V']).version).toBe(true);
    expect(parseFlags(['-h']).help).toBe(true);
    expect(parseFlags(['-c']).continueLast).toBe(true);
    expect(parseFlags(['-q']).quiet).toBe(true);
  });

  it('rejects non-numeric values on numeric flags', () => {
    expect(() => parseFlags(['--max-tokens', 'abc'])).toThrow(/--max-tokens/);
    expect(() => parseFlags(['--temperature', 'hot'])).toThrow(/--temperature/);
    expect(() => parseFlags(['--top-k', ''])).not.toThrow(); // empty -> fallback default
  });

  it('--system carries the prompt string', () => {
    const f = parseFlags(['--print', '--system', 'Be terse.', 'hi']);
    expect(f.system).toBe('Be terse.');
    expect(f.print).toBe('hi');
  });

  it('--list-tools is a boolean toggle', () => {
    expect(parseFlags(['--list-tools']).listTools).toBe(true);
    expect(parseFlags([]).listTools).toBe(false);
  });

  it('--max-time parses as a positive number', () => {
    expect(parseFlags(['--max-time', '30']).maxTime).toBe(30);
    expect(parseFlags([]).maxTime).toBe(0);
    expect(() => parseFlags(['--max-time', 'xyz'])).toThrow(/--max-time/);
  });

  it('autoCompact defaults to true, --no-auto-compact flips it', () => {
    expect(parseFlags([]).autoCompact).toBe(true);
    expect(parseFlags(['--no-auto-compact']).autoCompact).toBe(false);
  });
});
