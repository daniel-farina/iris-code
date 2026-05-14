// Subagent runner. Spawns a focused mini-loop with a fresh MTPLX
// session, a restricted tool pool, and an optional pre-read context
// of specific files. The parent agent's conv stays small - it sees
// only the delegate(...) tool call and its short summary result,
// not the subagent's tool history.
//
// Pattern modeled after claude-code-main's AgentTool/runAgent.ts,
// stripped down to the parts hip needs:
//
//   - Fresh session_id keeps MTPLX's per-session prefix tiny → cold
//     prefill on first round is cheap (3-5K tokens vs 18K in parent).
//   - Restricted tool pool (no delegate / no task_* / no bg_*) so
//     subagents can't recurse infinitely or pollute parent state.
//   - Pre-read files into the subagent's readState so the subagent
//     doesn't have to spend a round reading what the caller already
//     knows it needs.
//   - Caps maxRounds tightly (default 8) - subagents do focused work.
//   - Returns the final assistant message + the list of files the
//     subagent edited, so the caller can render a meaningful summary.

import { readFileForEdit } from './file_io.js';
import { type ChatMessage, systemMessage, userMessage } from './schema.js';
import { type SamplingOpts, MtplxClient } from './client.js';
import { readState } from './read_state.js';
import { type Tool } from './tools/index.js';
import { runLoop } from './agent.js';
import { withEnv } from './system_prompt.js';

const SUBAGENT_PROMPT = `You are a focused sub-agent. The parent agent has delegated a specific, bounded task to you. Do exactly that task, then stop.

Rules:
- The task description is in the first user message. Read it carefully.
- Files already pre-loaded into your read history are listed there too - you can edit them without calling \`read\` again.
- Use ONLY the tools you've been given. You do NOT have access to task_*, bg_*, browser_check, or delegate.
- Make the smallest correct change. Don't refactor adjacent code.
- When the task is done (or you hit a blocker), output ONE paragraph summarizing what you changed (file:line bullet points) and stop. The parent reads only your final message.
- Do NOT call tools after your summary - stop after the summary turn.`;

export interface SubagentOptions {
  /** What the subagent should do, in plain English. */
  task: string;
  /** Files to pre-read into the subagent's readState before it starts.
   *  The subagent sees them in its initial user message AND can call
   *  edit on them without a separate read first. */
  files?: string[];
  /** Subset of the parent's tool registry to expose. Default is the
   *  "edit pack": read, edit, multi_edit, search, grep, glob, list,
   *  tree, diff. No bash by default - bounded edit work, not project-
   *  level scripting. */
  tools: Tool[];
  /** Parent's MTPLX base URL + model so the subagent talks to the
   *  same server. */
  baseUrl: string;
  model: string;
  /** Cap on rounds. Subagents should converge fast - if it blows past
   *  this, the task was too big. Default 8. */
  maxRounds?: number;
  sampling: SamplingOpts;
  /** Abort signal threaded from the parent (so Ctrl-C in the TUI
   *  cancels the subagent too). */
  signal?: AbortSignal;
  /** Optional parent session id - used only to build a memorable
   *  sub-session id for debugging. */
  parentSessionId?: string;
}

export interface SubagentResult {
  summary: string;
  filesEdited: string[];
  rounds: number;
  /** Approximate tokens the parent's conv saved by NOT seeing the
   *  subagent's tool history. Computed as (subagent conv size) -
   *  (final summary size). Surfaced in the delegate tool's output
   *  so the user can see what delegation paid for. */
  parentSavedApproxTokens: number;
}

/** Run a sub-agent loop. Returns the final assistant text + list of
 *  files it modified. Never throws on subagent-internal errors; if
 *  the subagent fails to converge, returns a summary explaining that. */
export async function runSubagent(opts: SubagentOptions): Promise<SubagentResult> {
  const subSessionId = `sub-${opts.parentSessionId?.slice(-12) ?? 'na'}-${rand6()}`;
  const client = new MtplxClient({
    baseUrl: opts.baseUrl,
    model: opts.model,
    sessionId: subSessionId,
  });

  // Pre-read requested files. Records each in readState so the
  // subagent's edit pre-read check passes without a tool round, AND
  // includes a snippet of each in the initial user message so the
  // model sees the content directly.
  const preReadBlocks: string[] = [];
  const filesPreRead: string[] = [];
  for (const f of opts.files ?? []) {
    try {
      const r = readFileForEdit(f);
      if (!r.exists) {
        preReadBlocks.push(`[pre-read ${f}] FILE DOES NOT EXIST`);
        continue;
      }
      readState.recordRead(f, r.content, false);
      filesPreRead.push(f);
      // Show the model up to ~6000 chars per file. Anything bigger
      // would defeat the purpose of delegating (the prompt would be
      // as big as the parent's full read).
      const snippet =
        r.content.length > 6000
          ? r.content.slice(0, 6000) + `\n[... ${r.content.length - 6000} bytes truncated; call \`read\` if you need the tail]`
          : r.content;
      preReadBlocks.push(`[pre-read ${f}]\n${snippet}\n[end ${f}]`);
    } catch (e) {
      preReadBlocks.push(`[pre-read ${f}] ERROR: ${(e as Error).message}`);
    }
  }

  const taskBlock = `<task>${opts.task}</task>`;
  const preReadSection =
    preReadBlocks.length > 0
      ? `\n\n${preReadBlocks.join('\n\n')}`
      : '';
  const conv: ChatMessage[] = [
    systemMessage(withEnv(SUBAGENT_PROMPT)),
    userMessage(`${taskBlock}${preReadSection}\n\nWork the task. When done, give a one-paragraph summary and stop.`),
  ];

  const filesEdited = new Set<string>();
  let rounds = 0;

  try {
    await runLoop({
      client,
      conv,
      sampling: opts.sampling,
      maxRounds: opts.maxRounds ?? 8,
      availableTools: opts.tools,
      signal: opts.signal,
      // Subagents don't need postcommit polling - they're small + cold;
      // the wait is wasted time. Set to 0 to disable.
      postcommitDelayMs: 0,
      // No mid-loop auto-compact - subagents are bounded by maxRounds
      // and shouldn't grow large enough to need it.
      autoCompactThresholdTokens: 0,
      events: {
        onToolStart: (name, args) => {
          // Track file edits so we can report them back to the parent.
          if ((name === 'edit' || name === 'multi_edit') && typeof args['path'] === 'string') {
            filesEdited.add(args['path'] as string);
          }
        },
        onRound: () => {
          rounds++;
        },
      },
    });
  } catch (e) {
    if ((e as Error).name !== 'AbortError') {
      return {
        summary: `[subagent failed] ${(e as Error).message}`,
        filesEdited: [...filesEdited],
        rounds,
        parentSavedApproxTokens: 0,
      };
    }
  }

  // Extract the final assistant message as the summary.
  const lastAssistant = [...conv]
    .reverse()
    .find((m) => m.role === 'assistant' && typeof m.content === 'string' && (m.content as string).trim().length > 0);
  const summary =
    lastAssistant && typeof lastAssistant.content === 'string'
      ? (lastAssistant.content as string).trim()
      : `(subagent ran ${rounds} rounds with no final summary; check files: ${[...filesEdited].join(', ') || 'none'})`;

  // Rough savings: every tool result + assistant chatter in the
  // subagent's conv stays inside the subagent. Parent sees only the
  // final summary. Approximate via JSON.stringify length / 4.
  const subagentConvSize = Math.floor(JSON.stringify(conv).length / 4);
  const summarySize = Math.floor(summary.length / 4);
  const parentSavedApproxTokens = Math.max(0, subagentConvSize - summarySize);

  return {
    summary,
    filesEdited: [...filesEdited],
    rounds,
    parentSavedApproxTokens,
  };
}

function rand6(): string {
  return Math.floor(Math.random() * 0xffffff)
    .toString(16)
    .padStart(6, '0');
}
