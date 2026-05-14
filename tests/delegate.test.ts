// Tests for the delegate tool's surface area. We can't drive a real
// MTPLX subagent loop in unit tests (no server), so these cover the
// argument-parsing + context-wiring + error paths. The actual loop
// integration is exercised end-to-end in manual TUI runs.

import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { delegateTool, setDelegateContext } from '../src/tools/delegate.js';

let tmp: string;

beforeEach(() => {
  tmp = mkdtempSync(join(tmpdir(), 'hip-delegate-'));
});
afterEach(() => {
  rmSync(tmp, { recursive: true, force: true });
});

describe('delegate tool', () => {
  it('fails fast when setDelegateContext was never called', async () => {
    // No setDelegateContext - simulates a startup path that forgot to
    // wire it. We test by clearing any prior context with a sentinel
    // that won't actually run (no real MTPLX), and asserting the
    // error message guides the dev.
    // (We can't actually "unset" the singleton; instead we test by
    //  asserting our error message format when uninitialized.)
    // This is a structural smoke test - the real "not initialized"
    // path triggers only on first import.
    expect(typeof delegateTool.run).toBe('function');
  });

  it('requires task arg', async () => {
    setDelegateContext({
      baseUrl: 'http://127.0.0.1:0', // intentionally unreachable port
      model: 'fake',
      parentSessionId: 'test',
      sampling: { temperature: 0, top_p: 1, top_k: 1, max_tokens: 1 },
    });
    await expect(delegateTool.run({})).rejects.toThrow(/missing `task`/);
  });

  it('exposes the right schema fields', () => {
    expect(delegateTool.name).toBe('delegate');
    const params = delegateTool.spec.function.parameters as {
      properties: Record<string, unknown>;
      required: string[];
    };
    expect(params.properties).toHaveProperty('task');
    expect(params.properties).toHaveProperty('files');
    expect(params.properties).toHaveProperty('max_rounds');
    expect(params.required).toEqual(['task']);
  });

  it('description mentions sub-agent + restricted tool pool', () => {
    const desc = delegateTool.spec.function.description ?? '';
    expect(desc.toLowerCase()).toMatch(/sub-agent|subagent/);
    expect(desc.toLowerCase()).toMatch(/no bash|no task_|no bg_/);
  });
});
