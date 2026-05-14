// Smoke tests for the background-task manager. Exercises the happy
// path (start → output → completion) and the kill path. Avoids
// flakiness by using short shell commands and polling the public API
// rather than the internal child handle.

import { describe, expect, it } from 'vitest';
import { bgTasks } from '../src/background_tasks.js';

async function waitFor<T>(
  pred: () => T | undefined,
  timeoutMs = 5000,
  intervalMs = 25,
): Promise<T> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    const v = pred();
    if (v !== undefined && v !== false) return v;
    await new Promise((r) => setTimeout(r, intervalMs));
  }
  throw new Error(`waitFor: timed out after ${timeoutMs}ms`);
}

describe('background_tasks', () => {
  it('starts a command, captures output, and marks it completed', async () => {
    const info = bgTasks.start('echo hello-bg-task && sleep 0.05');
    expect(info.id).toMatch(/^[0-9a-f]{6}$/);
    expect(info.status).toBe('running');
    expect(info.outputPath).toMatch(/hip-bg-/);

    const final = await waitFor(() => {
      const t = bgTasks.get(info.id);
      return t && t.status !== 'running' ? t : undefined;
    });
    expect(final.status).toBe('completed');
    expect(final.exitCode).toBe(0);

    const out = bgTasks.output(info.id);
    expect(out.found).toBe(true);
    expect(out.output).toContain('hello-bg-task');
  });

  it('drainCompletions returns finished tasks exactly once', async () => {
    const info = bgTasks.start('true');
    await waitFor(() => {
      const t = bgTasks.get(info.id);
      return t && t.status !== 'running' ? t : undefined;
    });
    const drained = bgTasks.drainCompletions();
    const ours = drained.find((d) => d.id === info.id);
    expect(ours).toBeDefined();
    // Second drain should not include this task again.
    const second = bgTasks.drainCompletions();
    expect(second.find((d) => d.id === info.id)).toBeUndefined();
  });

  it('stops a long-running task', async () => {
    const info = bgTasks.start('sleep 30');
    expect(bgTasks.get(info.id)?.status).toBe('running');
    const stopResult = bgTasks.stop(info.id);
    expect(stopResult.ok).toBe(true);
    const final = await waitFor(() => {
      const t = bgTasks.get(info.id);
      return t && t.status !== 'running' ? t : undefined;
    });
    // Either 'killed' (set synchronously by stop) or 'failed' (exit
    // code != 0 from the kill). Both are acceptable terminal states.
    expect(['killed', 'failed']).toContain(final.status);
  });

  it('list() returns tasks newest-first', async () => {
    const a = bgTasks.start('true');
    // Tiny gap so timestamps differ.
    await new Promise((r) => setTimeout(r, 5));
    const b = bgTasks.start('true');
    const ids = bgTasks.list().map((t) => t.id);
    const ai = ids.indexOf(a.id);
    const bi = ids.indexOf(b.id);
    expect(bi).toBeGreaterThanOrEqual(0);
    expect(ai).toBeGreaterThan(bi); // b is newer, so it appears first
  });

  it('subscribe fires on state changes', async () => {
    let count = 0;
    const unsub = bgTasks.subscribe(() => {
      count++;
    });
    bgTasks.start('true');
    await waitFor(() => count > 0);
    expect(count).toBeGreaterThan(0);
    unsub();
  });
});
