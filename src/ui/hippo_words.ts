// Hippo-themed status verbs that cycle while the model is busy.
// Claude Code style: shows a present-participle verb + elapsed time so
// the user knows the agent is alive and roughly how long it has been
// thinking. Words mix real hippo behaviors with playful nonsense to
// keep the loop feeling friendly during long tool runs.

export const HIPPO_WORDS: readonly string[] = [
  // User's seed list:
  'waddling',
  'chomping',
  'bouncing',
  'zooming',
  'tantruming',
  'screaming',
  // 26 more — real hippo behaviors:
  'wallowing',
  'yawning',
  'snorting',
  'splashing',
  'submerging',
  'surfacing',
  'grazing',
  'galloping',
  'lounging',
  'honking',
  'grumbling',
  'nibbling',
  'munching',
  'grunting',
  'bellowing',
  'wading',
  'paddling',
  'stomping',
  'lumbering',
  'plodding',
  'flopping',
  'squelching',
  'snoozing',
  'pondering',
  'drooling',
  'blinking',
  'burping',
];

/** Pick a random word from the pool. Used at busy-start so each turn
 *  begins with a fresh verb rather than always "waddling". */
export function randomHippoWord(): string {
  const i = Math.floor(Math.random() * HIPPO_WORDS.length);
  return HIPPO_WORDS[i] ?? 'thinking';
}

/** Cycle to the next word, wrapping around. Used by the busy-rotation
 *  interval. Caller passes the previous word to keep state out of the
 *  pure function. */
export function nextHippoWord(prev: string): string {
  const idx = HIPPO_WORDS.indexOf(prev);
  if (idx < 0) return randomHippoWord();
  return HIPPO_WORDS[(idx + 1) % HIPPO_WORDS.length] ?? 'thinking';
}
