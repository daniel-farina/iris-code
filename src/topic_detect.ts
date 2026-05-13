// Detect whether a user's incoming prompt is an EXPLICIT new-task /
// reset signal (so the agent can wipe the conv and start fresh) vs. a
// continuation of the current work. Conservative on purpose: false
// negatives just mean we keep stale conv around for a turn longer;
// false positives lose work. Only triggers on explicit keywords.
//
// In hip's cwd-stable session id model, every invocation in the same
// directory on the same day appends to the same conv. This is great
// for iterative work but wasteful when the user pivots to a brand new
// task in the same dir. Detecting that lets us reseed the conv to
// just [system] and start clean, saving prefix bloat and giving the
// model a clear context.

/** Phrases that strongly suggest the user wants to forget the prior
 *  conversation and start fresh. Tested as case-insensitive substring
 *  matches on the first 200 chars of the prompt. */
const RESET_PHRASES: readonly RegExp[] = [
  /\bforget (that|everything|all of (this|that)|the previous|previous)\b/i,
  /\b(start|begin) (over|fresh|anew|again)\b/i,
  /\b(reset|clear) (the )?(conversation|context|state|history)\b/i,
  /\bignore (the |all )?previous\b/i,
  /\bnew task[\s:.\-]/i,
  /^\s*(\/new|\/reset|\/forget)\b/i,
] as const;

/** Returns true iff the prompt contains an explicit reset signal AND
 *  the existing conv has meaningful state to discard (more than just
 *  a system message). Caller decides what to do (wipe conv, /compact,
 *  inform the user). */
export function detectResetIntent(prompt: string, convLength: number): boolean {
  if (convLength <= 1) return false; // nothing to reset
  const head = prompt.slice(0, 200);
  return RESET_PHRASES.some((re) => re.test(head));
}
