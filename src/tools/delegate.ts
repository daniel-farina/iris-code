// `delegate` tool: spawn a focused sub-agent to edit one or more
// files. The sub-agent gets its own fresh MTPLX session, a restricted
// tool pool (read + edit + multi_edit + search + grep + glob + list +
// tree + diff), and optional pre-loaded file content. Returns a
// one-paragraph summary back to the parent.
//
// Why: the parent's conv stays small. A typical full-file read +
// multi_edit + assistant chatter eats 3-5K tokens of parent prefix.
// Delegating pushes all of that into a sub-session whose own prefix
// is small AND short-lived. Parent only sees the delegate call + its
// summary (~500 tok). The MTPLX session bank with MAX_ENTRIES=32 has
// plenty of room for short-lived sub-sessions; they cold-prefill
// once then evict, which is what we want.
//
// The sub-agent does NOT have access to delegate, task_*, bg_*, or
// browser_check - that prevents recursion and prevents subagents
// from polluting the parent's session-scoped state (task list,
// running background commands).

import { runSubagent } from '../subagent.js';
import { type Tool, argString } from './types.js';
import { diffTool } from './diff.js';
import { editTool, multiEditTool } from './edit.js';
import { globTool } from './glob.js';
import { grepTool } from './grep.js';
import { listTool } from './list.js';
import { readTool } from './read.js';
import { searchTool } from './search.js';
import { treeTool } from './tree.js';

/** Configuration injected by the CLI/TUI before the parent agent
 *  starts running. Without this, delegate fails fast with a clear
 *  message - we don't want to silently no-op. */
export interface DelegateContext {
  baseUrl: string;
  model: string;
  parentSessionId: string;
  sampling: {
    temperature: number;
    top_p: number;
    top_k: number;
    max_tokens: number;
  };
}

let context: DelegateContext | null = null;

/** Called by the CLI/TUI at session bootstrap (before any user
 *  prompt fires) to thread the parent's connection params through to
 *  the delegate tool. Setting again replaces the previous context -
 *  matches what happens on /new or /resume. */
export function setDelegateContext(ctx: DelegateContext): void {
  context = ctx;
}

/** Returns the tool pool a sub-agent is allowed to use. Excludes
 *  delegate (no recursion), task_* (parent's task plan is
 *  authoritative), bg_* (parent owns background lifecycle),
 *  browser_check (UI side-effects), and bash (subagents do bounded
 *  edit work, not project-level scripting). */
function subagentToolPool(): Tool[] {
  return [
    readTool,
    searchTool,
    grepTool,
    globTool,
    listTool,
    treeTool,
    diffTool,
    editTool,
    multiEditTool,
  ];
}

export const delegateTool: Tool = {
  name: 'delegate',
  spec: {
    type: 'function',
    function: {
      name: 'delegate',
      description:
        'Spawn a focused sub-agent for bounded edit work in a fresh session. Sub-agent has only read/edit/multi_edit/search/grep/glob/list/tree/diff (no bash, task_*, bg_*, browser_check, delegate). Returns a one-paragraph summary + files modified. Use when parent conv >10K tokens and task is well-scoped; do NOT use for trivial single edits.',
      parameters: {
        type: 'object',
        properties: {
          task: {
            type: 'string',
            description: 'specific task with file paths + transformation',
          },
          files: {
            type: 'array',
            items: { type: 'string' },
            description: 'files to pre-load (cap ~6KB each)',
          },
          max_rounds: { type: 'integer', description: 'cap on sub-agent rounds (default 8)' },
        },
        required: ['task'],
      },
    },
  },
  async run(args) {
    if (!context) {
      throw new Error(
        'delegate: not initialized. This tool requires the CLI/TUI bootstrap to call setDelegateContext() first.',
      );
    }
    const task = argString(args, 'task');
    if (!task) throw new Error('delegate: missing `task`');
    const filesRaw = args['files'];
    let files: string[] | undefined;
    if (filesRaw !== undefined) {
      // qwen3 may stringify arrays - mirror multi_edit's defensive parse.
      let parsed: unknown = filesRaw;
      if (typeof parsed === 'string') {
        try {
          parsed = JSON.parse(parsed);
        } catch {
          // Treat a single string as one path
          parsed = [filesRaw];
        }
      }
      if (Array.isArray(parsed)) {
        files = parsed.filter((x): x is string => typeof x === 'string');
      }
    }
    const maxRounds =
      typeof args['max_rounds'] === 'number' && Number.isFinite(args['max_rounds'])
        ? Math.max(1, Math.min(20, Math.floor(args['max_rounds'] as number)))
        : undefined;

    const result = await runSubagent({
      task,
      files,
      tools: subagentToolPool(),
      baseUrl: context.baseUrl,
      model: context.model,
      sampling: context.sampling,
      parentSessionId: context.parentSessionId,
      ...(maxRounds !== undefined ? { maxRounds } : {}),
    });

    const filesLine =
      result.filesEdited.length > 0
        ? `\nfiles edited: ${result.filesEdited.join(', ')}`
        : '\n(no files modified)';
    return (
      `[delegate done · ${result.rounds} round${result.rounds === 1 ? '' : 's'} · ` +
      `~${result.parentSavedApproxTokens} tok kept out of parent conv]${filesLine}\n\n` +
      `summary: ${result.summary}`
    );
  },
};
