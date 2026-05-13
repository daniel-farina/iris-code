// In-loop conversation compaction.
//
// Asks the model to write a terse summary of the current conv, then
// rebuilds the conv as [system, user(summary), assistant(ack)]. Used
// by:
//   - the TUI's /compact slash command and the auto-trigger that
//     fires when conv crosses AUTO_COMPACT_TOKEN_THRESHOLD
//   - the print-mode agent loop (this module) which calls compactConv()
//     mid-loop when the prefix grows past the cliff
//
// Compaction is what keeps long sessions fast: MTPLX's KV cache thrashes
// past ~10K-15K tokens, so trimming the prefix before every "big" round
// lets the cache stay warm and TTFT stay sub-second.

import type { MtplxClient, SamplingOpts } from './client.js';
import { type ChatMessage, systemMessage, userMessage } from './schema.js';
import { DEFAULT_SYSTEM_PROMPT } from './system_prompt.js';

export interface CompactResult {
  summary: string;
  newConv: ChatMessage[];
  msgsBefore: number;
  msgsAfter: number;
  /** Approximate token count (chars/4) before/after for telemetry. */
  tokensBefore: number;
  tokensAfter: number;
}

export const COMPACT_SUMMARIZE_PROMPT = [
  'Write a terse note-to-self that will REPLACE this conversation, so future-you can keep working without re-reading anything. Use this exact structure:',
  '',
  '## Original goal',
  '<one sentence — what the user originally asked for>',
  '',
  '## Files touched',
  '<bulleted list. one line per file. include path + status + KEY exports/structure.>',
  '- `src/foo.js` (created): exports `createFoo(opts)` returning `{a, b, update(dt)}`',
  '- `src/bar.js` (edited): now imports `createFoo`; updateLoop calls foo.update(dt)',
  '',
  '## Decisions / constraints',
  '<bullet list of any non-obvious choices or constraints — e.g. "using Catmull-Rom curve, not bezier", "files >50 lines split via append">',
  '',
  '## Done so far',
  '<bullet list — concrete results, NOT process>',
  '',
  '## Next action',
  '<one sentence — the single next concrete step to keep moving toward the goal>',
  '',
  'Rules: terse > complete. Concrete > abstract. Mention exact file paths and function/export names so the resumed model can `edit` or `read` precisely. Skip pleasantries.',
].join('\n');

export async function compactConv(opts: {
  client: MtplxClient;
  conv: ChatMessage[];
  sampling: SamplingOpts;
  systemPrompt?: string;
  signal?: AbortSignal;
}): Promise<CompactResult | null> {
  const { client, conv, sampling, systemPrompt, signal } = opts;
  if (conv.length <= 3) return null;

  const msgsBefore = conv.length;
  const tokensBefore = approxTokens(conv);

  const summarizeConv: ChatMessage[] = [
    ...conv,
    { role: 'user', content: COMPACT_SUMMARIZE_PROMPT },
  ];
  const res = await client.stream(
    summarizeConv,
    undefined,
    { ...sampling, max_tokens: 2000 },
    signal,
  );
  const summary = res.content.trim();
  if (!summary) return null;

  const newConv: ChatMessage[] = [
    systemMessage(systemPrompt ?? DEFAULT_SYSTEM_PROMPT),
    userMessage(`[Compacted session summary]\n${summary}`),
    { role: 'assistant', content: 'OK, continuing from the summary.' },
  ];
  return {
    summary,
    newConv,
    msgsBefore,
    msgsAfter: newConv.length,
    tokensBefore,
    tokensAfter: approxTokens(newConv),
  };
}

/** Cheap token estimate: JSON-stringify length / 4. Same heuristic the
 *  TUI uses; good enough to compare orders of magnitude. */
export function approxTokens(conv: ChatMessage[]): number {
  return Math.floor(JSON.stringify(conv).length / 4);
}
