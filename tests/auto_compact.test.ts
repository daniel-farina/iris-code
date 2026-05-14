// Unit tests for the auto-/compact threshold helpers. These run without
// Ink, so they're cheap and stable. The TUI integration is intentionally
// not exercised here - that's covered by manual smoke testing of the REPL.

import { describe, expect, it } from 'vitest';
import { type ChatMessage, systemMessage, userMessage } from '../src/schema.js';
import {
  AUTO_COMPACT_TOKEN_THRESHOLD,
  estimateTokens,
  formatTokens,
  shouldAutoCompact,
} from '../src/ui/auto_compact.js';

describe('auto_compact helpers', () => {
  it('estimateTokens scales with conv size', () => {
    const small: ChatMessage[] = [systemMessage('hi'), userMessage('hi')];
    const big: ChatMessage[] = [
      systemMessage('hi'),
      ...Array.from({ length: 100 }, () => userMessage('x'.repeat(500))),
    ];
    expect(estimateTokens(big)).toBeGreaterThan(estimateTokens(small) * 10);
  });

  it('shouldAutoCompact returns false when disabled', () => {
    const big: ChatMessage[] = [
      systemMessage('sys'),
      ...Array.from({ length: 100 }, () => userMessage('x'.repeat(1000))),
    ];
    expect(estimateTokens(big)).toBeGreaterThan(AUTO_COMPACT_TOKEN_THRESHOLD);
    expect(shouldAutoCompact(big, false)).toBe(false);
  });

  it('shouldAutoCompact returns false for tiny convs even when enabled', () => {
    const tiny: ChatMessage[] = [systemMessage('s'), userMessage('hi')];
    expect(shouldAutoCompact(tiny, true)).toBe(false);
  });

  it('shouldAutoCompact returns false just below threshold', () => {
    // Stay well under threshold: tokens ≈ chars/4.
    const undersized: ChatMessage[] = [
      systemMessage('sys'),
      userMessage('u1'),
      userMessage('u2'),
      userMessage('u3'),
      userMessage('a small body'),
    ];
    expect(estimateTokens(undersized)).toBeLessThan(AUTO_COMPACT_TOKEN_THRESHOLD);
    expect(shouldAutoCompact(undersized, true)).toBe(false);
  });

  it('shouldAutoCompact returns true above threshold', () => {
    // Generate enough content that JSON.stringify is well over the
    // threshold-bytes mark (threshold * 4 chars).
    const need = AUTO_COMPACT_TOKEN_THRESHOLD * 4 + 8000;
    const perTurn = 1200;
    const turns = Math.ceil(need / perTurn);
    const big: ChatMessage[] = [
      systemMessage('sys'),
      ...Array.from({ length: turns }, (_, i) => userMessage(`turn ${i}: ${'x'.repeat(perTurn)}`)),
    ];
    expect(estimateTokens(big)).toBeGreaterThan(AUTO_COMPACT_TOKEN_THRESHOLD);
    expect(shouldAutoCompact(big, true)).toBe(true);
  });

  it('formatTokens prints "K" suffix above 1K', () => {
    expect(formatTokens(500)).toBe('500');
    expect(formatTokens(1200)).toBe('1.2K');
    expect(formatTokens(9234)).toBe('9.2K');
  });
});
