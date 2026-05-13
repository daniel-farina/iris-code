// Tool registry. The agent loop receives this list and dispatches each
// tool_call by matching `function.name`. Ordering doesn't matter for
// dispatch but the order is sent to the model in the tools array, so keep
// the most-used tools first (search/read/edit/bash) - the model latches
// onto whichever it sees first when ambiguous.

import { bashTool } from './bash.js';
import { diffTool } from './diff.js';
import { editTool } from './edit.js';
import { globTool } from './glob.js';
import { grepTool } from './grep.js';
import { listTool } from './list.js';
import { peekLogTool } from './peek_log.js';
import { readTool } from './read.js';
import { searchTool } from './search.js';
import { treeTool } from './tree.js';
import type { Tool } from './types.js';

export const REGISTRY: Tool[] = [
  readTool,
  searchTool,
  grepTool,
  globTool,
  editTool,
  bashTool,
  listTool,
  treeTool,
  diffTool,
  peekLogTool,
];

export function findTool(name: string): Tool | undefined {
  return REGISTRY.find((t) => t.name === name);
}

export function toolSpecs() {
  return REGISTRY.map((t) => t.spec);
}

export type { Tool };
