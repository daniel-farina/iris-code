// Smoke tests for the task manager. Exercises create/list/next/update
// transitions, single-in_progress invariant, and subscription firing.

import { afterEach, describe, expect, it } from 'vitest';
import { formatTaskList, taskManager } from '../src/task_manager.js';

afterEach(() => {
  taskManager.clear();
});

describe('task_manager', () => {
  it('creates tasks with monotonic ids and pending status', () => {
    const a = taskManager.create('Add login', 'Wire up the form');
    const b = taskManager.create('Add tests', 'Cover login flow');
    expect(a.id).toBe('1');
    expect(b.id).toBe('2');
    expect(a.status).toBe('pending');
    expect(b.status).toBe('pending');
  });

  it('next() claims the first pending task and flips it to in_progress', () => {
    taskManager.create('A', 'first');
    taskManager.create('B', 'second');
    const n = taskManager.next();
    expect(n?.id).toBe('1');
    expect(n?.status).toBe('in_progress');
    // Second call advances - but only after first is no longer pending.
    // Re-claiming returns the same in_progress task only if we start fresh:
    // here, next() picks the next pending after demoting current.
    const n2 = taskManager.next();
    expect(n2?.id).toBe('2');
    expect(n2?.status).toBe('in_progress');
    // The previous in_progress got demoted back to pending.
    expect(taskManager.get('1')?.status).toBe('pending');
  });

  it('only one task is in_progress at a time', () => {
    taskManager.create('A', 'a');
    taskManager.create('B', 'b');
    taskManager.start('1');
    taskManager.start('2');
    expect(taskManager.get('1')?.status).toBe('pending');
    expect(taskManager.get('2')?.status).toBe('in_progress');
    expect(taskManager.active()?.id).toBe('2');
  });

  it('update sets status, completed timestamps, and summary', () => {
    taskManager.create('A', 'a');
    taskManager.start('1');
    const t = taskManager.update('1', { status: 'completed', summary: 'shipped' });
    expect(t.status).toBe('completed');
    expect(t.summary).toBe('shipped');
    expect(t.completedAt).toBeGreaterThan(0);
  });

  it('next() returns undefined when all tasks are done', () => {
    taskManager.create('A', 'a');
    taskManager.start('1');
    taskManager.update('1', { status: 'completed' });
    expect(taskManager.next()).toBeUndefined();
  });

  it('subscribe fires created/started/completed events', () => {
    const events: string[] = [];
    const unsub = taskManager.subscribe((e) => events.push(e.kind));
    taskManager.create('A', 'a');
    taskManager.start('1');
    taskManager.update('1', { status: 'completed' });
    unsub();
    expect(events).toContain('created');
    expect(events).toContain('started');
    expect(events).toContain('completed');
  });

  it('formatTaskList renders glyphs + ids', () => {
    taskManager.create('First', 'a');
    taskManager.create('Second', 'b');
    taskManager.start('1');
    const out = formatTaskList(taskManager.list());
    expect(out).toMatch(/id=1/);
    expect(out).toMatch(/id=2/);
    expect(out).toMatch(/First/);
    expect(out).toMatch(/Second/);
  });

  it('counts() aggregates by status', () => {
    taskManager.create('A', 'a');
    taskManager.create('B', 'b');
    taskManager.create('C', 'c');
    taskManager.start('1');
    taskManager.update('1', { status: 'completed' });
    const c = taskManager.counts();
    expect(c.total).toBe(3);
    expect(c.completed).toBe(1);
    expect(c.pending).toBe(2);
    expect(c.inProgress).toBe(0);
  });

  it('clear wipes all tasks and resets id counter', () => {
    taskManager.create('A', 'a');
    taskManager.create('B', 'b');
    taskManager.clear();
    expect(taskManager.list().length).toBe(0);
    const next = taskManager.create('C', 'c');
    expect(next.id).toBe('1');
  });

  it('task_update tool accepts both "1" and "T1" id forms', async () => {
    const { taskUpdateTool } = await import('../src/tools/tasks.js');
    taskManager.create('A', 'a');
    // Bare numeric id - canonical form
    const r1 = await taskUpdateTool.run({ id: '1', status: 'in_progress' });
    expect(r1).toContain('in_progress');
    // T-prefixed (display form copied from panel) should also work
    const r2 = await taskUpdateTool.run({ id: 'T1', status: 'completed' });
    expect(r2).toContain('completed');
    expect(taskManager.get('1')?.status).toBe('completed');
  });
});
