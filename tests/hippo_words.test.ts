import { describe, expect, it } from 'vitest';
import { HIPPO_WORDS, nextHippoWord, randomHippoWord } from '../src/ui/hippo_words.js';

describe('HIPPO_WORDS', () => {
  it('has at least 31 words (6 seeds + 25 extras)', () => {
    expect(HIPPO_WORDS.length).toBeGreaterThanOrEqual(31);
  });

  it('includes the user-requested seed verbs', () => {
    for (const w of ['waddling', 'chomping', 'bouncing', 'zooming', 'tantruming', 'screaming']) {
      expect(HIPPO_WORDS).toContain(w);
    }
  });

  it('words are unique', () => {
    expect(new Set(HIPPO_WORDS).size).toBe(HIPPO_WORDS.length);
  });

  it('every word is a present-participle verb (-ing) per the claude-code aesthetic', () => {
    for (const w of HIPPO_WORDS) {
      expect(w.endsWith('ing'), `${w} should be a present-participle verb`).toBe(true);
    }
  });

  it('randomHippoWord returns something from the pool', () => {
    for (let i = 0; i < 20; i++) {
      expect(HIPPO_WORDS).toContain(randomHippoWord());
    }
  });

  it('nextHippoWord cycles in order', () => {
    const first = HIPPO_WORDS[0]!;
    const second = HIPPO_WORDS[1]!;
    expect(nextHippoWord(first)).toBe(second);
    // Wrap-around: last → first.
    const last = HIPPO_WORDS[HIPPO_WORDS.length - 1]!;
    expect(nextHippoWord(last)).toBe(first);
  });

  it('nextHippoWord with an unknown word returns a fresh random word', () => {
    const w = nextHippoWord('not-a-real-hippo-word');
    expect(HIPPO_WORDS).toContain(w);
  });
});
