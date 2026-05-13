// In-place tool-result pruning. The model rarely re-reads old tool
// outputs - it uses them at the time and moves on. After a few rounds,
// keeping a 200-line `read` result in conv just burns prefix tokens.
//
// This is COMPLEMENTARY to compactConv():
//   - compactConv invokes the model to summarize and reseed - expensive,
//     loses fidelity, runs at a threshold.
//   - pruneOldToolResults is a deterministic in-place rewrite - free,
//     no model call, can run between every round.
//
// The pruned message keeps the same `role: tool` + `tool_call_id`
// structure so MTPLX still sees a well-formed conv (orphan-tool rules
// still apply). Only the content shrinks to a one-line stub.

import type { ChatMessage } from './schema.js';

/** Replace the content of tool-result messages that are both OLD (not
 *  in the last keepLastK tool results) and LARGE (over pruneOverChars)
 *  with a stub. Stub format: `[redacted: NNN chars of <tool> output;
 *  re-run if needed]`. Returns the number of messages pruned and the
 *  chars saved for telemetry. */
export interface PruneResult {
  pruned: number;
  charsSaved: number;
}

export function pruneOldToolResults(
  conv: ChatMessage[],
  opts: { keepLastK?: number; pruneOverChars?: number } = {},
): PruneResult {
  const keepLastK = opts.keepLastK ?? 3;
  const pruneOverChars = opts.pruneOverChars ?? 1500;

  // Find indices of all `role: tool` messages, in order.
  const toolIdx: number[] = [];
  for (let i = 0; i < conv.length; i++) {
    if (conv[i]?.role === 'tool') toolIdx.push(i);
  }
  if (toolIdx.length <= keepLastK) return { pruned: 0, charsSaved: 0 };

  // Everything except the trailing keepLastK is eligible for pruning.
  const cutoff = toolIdx.length - keepLastK;
  let pruned = 0;
  let charsSaved = 0;

  for (let k = 0; k < cutoff; k++) {
    const i = toolIdx[k];
    if (i === undefined) continue;
    const msg = conv[i];
    if (!msg || msg.role !== 'tool') continue;
    const content = typeof msg.content === 'string' ? msg.content : '';
    if (content.length <= pruneOverChars) continue;
    // Already pruned (idempotent re-runs are common - we call this
    // every round). Detect by stub marker.
    if (content.startsWith('[redacted:')) continue;
    // Neutral hint: don't say "call again" because some tools (bash)
    // have side effects. Model decides what to do with the gap.
    const stub = `[redacted: ${content.length} chars of ${msg.name ?? 'tool'} output from an earlier round]`;
    charsSaved += content.length - stub.length;
    pruned++;
    msg.content = stub;
  }

  return { pruned, charsSaved };
}
