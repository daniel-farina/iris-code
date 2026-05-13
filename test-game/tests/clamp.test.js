import { clamp } from '../src/util/clamp.js';
import { describe, expect, test } from 'bun:test';

describe('clamp', () => {
  test('within range returns value', () => {
    expect(clamp(5, 0, 10)).toBe(5);
  });

  test('below min returns min', () => {
    expect(clamp(-5, 0, 10)).toBe(0);
  });

  test('above max returns max', () => {
    expect(clamp(15, 0, 10)).toBe(10);
  });
});
