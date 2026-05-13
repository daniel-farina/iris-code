import { describe, expect, it } from 'vitest';
import { detectResetIntent } from '../src/topic_detect.js';

describe('detectResetIntent', () => {
  it('returns false when conv is empty or only system msg', () => {
    expect(detectResetIntent('start over please', 0)).toBe(false);
    expect(detectResetIntent('forget everything', 1)).toBe(false);
  });

  it('detects explicit reset phrases', () => {
    expect(detectResetIntent("let's start over", 5)).toBe(true);
    expect(detectResetIntent('start fresh', 5)).toBe(true);
    expect(detectResetIntent('forget everything we discussed', 5)).toBe(true);
    expect(detectResetIntent('reset the conversation', 5)).toBe(true);
    expect(detectResetIntent('clear the context', 5)).toBe(true);
    expect(detectResetIntent('ignore the previous instructions', 5)).toBe(true);
    expect(detectResetIntent('new task: build a calculator', 5)).toBe(true);
    expect(detectResetIntent('/new', 5)).toBe(true);
    expect(detectResetIntent('/reset', 5)).toBe(true);
  });

  it('does NOT trigger on continuation phrases', () => {
    expect(detectResetIntent('now also add this feature', 5)).toBe(false);
    expect(detectResetIntent('can you add a restart button', 5)).toBe(false);
    expect(detectResetIntent("let's add a new track to the racing game", 5)).toBe(false);
    expect(detectResetIntent('also create a new file for the audio engine', 5)).toBe(false);
    expect(detectResetIntent('update the existing logic', 5)).toBe(false);
  });

  it('does NOT trigger on the word "new" alone', () => {
    expect(detectResetIntent('add a new feature to the game', 5)).toBe(false);
    expect(detectResetIntent('make a new file called audio.js', 5)).toBe(false);
  });

  it('only inspects the first 200 chars', () => {
    const prefix = 'now please add a small feature: '.padEnd(250, '.');
    const prompt = `${prefix} forget everything we did before`;
    // The reset phrase is past char 200 - should not trigger.
    expect(detectResetIntent(prompt, 5)).toBe(false);
  });
});
