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

/** Partial-compaction strategy: split conv into HEAD (system+old) and
 *  TAIL (recent turns). Summarize the HEAD, keep the TAIL verbatim.
 *  Model still has fresh context for the immediate next step instead of
 *  ONLY a summary. Default keepTailMessages=6 covers roughly the last
 *  3 user/assistant pairs (or 2 tool-using rounds), bounded by
 *  keepTailMaxTokens so a single fat tool result can't blow the tail. */
export async function compactConv(opts: {
  client: MtplxClient;
  conv: ChatMessage[];
  sampling: SamplingOpts;
  systemPrompt?: string;
  signal?: AbortSignal;
  /** How many of the most recent messages to keep verbatim. Default 6. */
  keepTailMessages?: number;
  /** Hard cap on the kept tail's approximate tokens. If the last
   *  keepTailMessages exceed this, we shrink the tail from the front
   *  (oldest-of-tail) until it fits. Default 3000. */
  keepTailMaxTokens?: number;
}): Promise<CompactResult | null> {
  const { client, conv, sampling, systemPrompt, signal } = opts;
  const keepTail = opts.keepTailMessages ?? 6;
  const keepTailMaxTokens = opts.keepTailMaxTokens ?? 3000;
  if (conv.length <= 3) return null;

  const msgsBefore = conv.length;
  const tokensBefore = approxTokens(conv);

  // Figure out the cut: keep system + (up to keepTail trailing msgs).
  // Anything between gets summarized. If conv is so short there's
  // nothing to summarize after that cut, fall back to full compaction.
  const sysSlice = conv[0]?.role === 'system' ? conv.slice(0, 1) : [];
  let tailStart = Math.max(sysSlice.length, conv.length - keepTail);
  // Shrink the tail from the FRONT until it fits the token budget.
  // (Trimming from the back would lose the most-recent message, which
  // is exactly the one the next round needs to act on.)
  while (tailStart < conv.length - 1 && approxTokens(conv.slice(tailStart)) > keepTailMaxTokens) {
    tailStart++;
  }
  const headForSummary = conv.slice(sysSlice.length, tailStart);
  const tail = conv.slice(tailStart);
  // Don't bother summarizing if there's barely anything to summarize.
  const doPartial = headForSummary.length >= 3;

  let summary: string;
  if (doPartial) {
    const summarizeConv: ChatMessage[] = [
      ...sysSlice,
      ...headForSummary,
      { role: 'user', content: COMPACT_SUMMARIZE_PROMPT },
    ];
    const res = await client.stream(
      summarizeConv,
      undefined,
      { ...sampling, max_tokens: 2000 },
      signal,
    );
    summary = res.content.trim();
    if (!summary) return null;
  } else {
    // Fall back to full compaction across the whole conv.
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
    summary = res.content.trim();
    if (!summary) return null;
  }

  // New conv: [system, user(summary), assistant(ack), ...tail].
  // The tail keeps the model anchored to the immediate next step
  // rather than ONLY having a paragraph of summary.
  const newConv: ChatMessage[] = [
    systemMessage(systemPrompt ?? DEFAULT_SYSTEM_PROMPT),
    userMessage(`[Compacted session summary — older turns]\n${summary}`),
    { role: 'assistant', content: 'OK, continuing from the summary.' },
    ...(doPartial ? sanitizeTail(tail) : []),
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

/** The tail must remain a valid assistant-tool/user-tool-result chain or
 *  MTPLX rejects it. Drop a leading orphan tool message and any leading
 *  assistant-tool-calls without matching results. Cheap heuristic: trim
 *  until the first message is `user` or `assistant` without tool_calls. */
function sanitizeTail(tail: ChatMessage[]): ChatMessage[] {
  let i = 0;
  while (i < tail.length) {
    const m = tail[i];
    if (!m) break;
    // Orphan tool result (no preceding assistant tool_call in the new
    // post-summary conv).
    if (m.role === 'tool') {
      i++;
      continue;
    }
    // Assistant message with tool_calls is fine ONLY if all its
    // tool_call_ids have matching `tool` messages later in tail.
    if (m.role === 'assistant' && m.tool_calls && m.tool_calls.length > 0) {
      const ids = new Set(m.tool_calls.map((c) => c.id));
      let matched = 0;
      for (let j = i + 1; j < tail.length; j++) {
        const r = tail[j];
        if (r?.role === 'tool' && r.tool_call_id && ids.has(r.tool_call_id)) matched++;
      }
      if (matched < ids.size) {
        i++;
        continue;
      }
    }
    break;
  }
  return tail.slice(i);
}

/** Cheap token estimate: JSON-stringify length / 4. Same heuristic the
 *  TUI uses; good enough to compare orders of magnitude. */
export function approxTokens(conv: ChatMessage[]): number {
  return Math.floor(JSON.stringify(conv).length / 4);
}
