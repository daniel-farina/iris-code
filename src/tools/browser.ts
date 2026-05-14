// `browser_check` tool: opens a URL in headless Chrome, captures
// console errors + uncaught exceptions for a short window, returns
// them. Lets the model verify that a JS/UI change actually loads
// without throwing - catches refactor-induced ReferenceErrors, bad
// imports, etc. that don't surface in `vite build`.
//
// Default url = http://localhost:5173 (Vite default). The model
// should call this AFTER finishing a UI-modifying change as the
// final "did it actually work" step.

import { browserCheck } from '../browser_check.js';
import { type Tool, argString, argU64 } from './types.js';

export const browserTool: Tool = {
  name: 'browser_check',
  spec: {
    type: 'function',
    function: {
      name: 'browser_check',
      description:
        "Open a URL in headless Chrome, load the page, then CLICK every visible button to also catch errors thrown by click handlers (e.g. 'selectedTrack is not defined' that only fires on click). Captures console.error + uncaught exceptions throughout. Use as the final step after any UI/JS change. Returns 'OK' or a list of errors with file:line.",
      parameters: {
        type: 'object',
        properties: {
          url: {
            type: 'string',
            description: 'page URL to load. Default http://localhost:5173',
          },
          wait_ms: {
            type: 'integer',
            description:
              'milliseconds to wait for the page to settle before the click sweep. Default 3000.',
          },
          interact: {
            type: 'boolean',
            description:
              'if true (default), click every visible button after load to catch handler errors. Set false to only check page-load errors.',
          },
        },
      },
    },
  },
  async run(args) {
    const url = argString(args, 'url') ?? 'http://localhost:5173';
    const waitMs = argU64(args, 'wait_ms') ?? 3000;
    const interact = args.interact !== false;
    const r = await browserCheck({ url, waitMs, interact });
    if (r.checkError) {
      return `[browser_check skipped] ${r.checkError}`;
    }
    if (r.ok) {
      return `OK - no console errors after ${(r.elapsedMs / 1000).toFixed(1)}s (${r.log.length} logs)`;
    }
    const lines = r.errors.map(
      (e) =>
        `  [${e.source}] ${e.text}${e.url ? ` (${e.url.split('/').pop() ?? e.url}:${e.line ?? '?'})` : ''}`,
    );
    return `${r.errors.length} error${r.errors.length === 1 ? '' : 's'} found after ${(r.elapsedMs / 1000).toFixed(1)}s:\n${lines.join('\n')}\n${r.log.length > 0 ? `\nlogs (${r.log.length}):\n${r.log.slice(0, 5).join('\n')}` : ''}`;
  },
};
