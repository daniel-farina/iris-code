// Task manager. Lets the model plan a multi-step request as a list of
// discrete tasks, then work through them one-by-one with a fresh
// context window per task. Modeled after claude-code's TaskCreate /
// TaskList / TaskUpdate / TaskGet tools, condensed into a single
// module-level singleton.
//
// Why context resets per task: MTPLX's paged-KV cache thrashes past
// ~10K tokens, and the sidecar already produces per-round one-line
// summaries we can fold back in. Treating each task as its own short
// session keeps the prefix cache warm AND prevents "I forgot what I
// was doing" drift mid-feature.
//
// The flow:
//
//   1. Model reads user request, calls task_create N times to plan.
//   2. Model calls task_next() to claim the first pending task. The
//      manager flips its status to in_progress and emits 'task-started'.
//   3. TUI subscribes to that event and triggers an automatic compact
//      between rounds: conv → [system, sidecar-summary-digest, task
//      description]. Sidecar lines survive across the reset.
//   4. Model works the task with bash/edit/etc., then calls
//      task_update(id, status=completed). Repeat from 2.
//
// Tasks live only for the current hip session - we don't persist them
// to disk. The session_store handles whole-session resume; tasks are
// part of that conv so they get serialized implicitly via the
// transcript history.

export type TaskStatus = 'pending' | 'in_progress' | 'completed' | 'deleted';

export interface Task {
  id: string;
  subject: string;
  description: string;
  /** Present-continuous form shown in the TUI spinner while
   *  in_progress (e.g. "Running tests"). Optional. */
  activeForm?: string;
  status: TaskStatus;
  createdAt: number;
  startedAt?: number;
  completedAt?: number;
  /** Last sidecar summary captured when this task was completed.
   *  Used as the "what got done" record when the model resumes from
   *  the task list mid-session. */
  summary?: string;
}

export type TaskEvent =
  | { kind: 'created'; task: Task }
  | { kind: 'updated'; task: Task; previousStatus: TaskStatus }
  | { kind: 'started'; task: Task }
  | { kind: 'completed'; task: Task }
  | { kind: 'cleared' };

type Listener = (ev: TaskEvent) => void;

class TaskManager {
  private tasks = new Map<string, Task>();
  private nextNum = 1;
  private listeners = new Set<Listener>();

  create(subject: string, description: string, activeForm?: string): Task {
    if (!subject.trim()) throw new Error('task_create: subject required');
    if (!description.trim()) throw new Error('task_create: description required');
    const id = String(this.nextNum++);
    const task: Task = {
      id,
      subject: subject.trim(),
      description: description.trim(),
      activeForm: activeForm?.trim() || undefined,
      status: 'pending',
      createdAt: Date.now(),
    };
    this.tasks.set(id, task);
    this.emit({ kind: 'created', task });
    return task;
  }

  list(includeDeleted = false): Task[] {
    return [...this.tasks.values()]
      .filter((t) => includeDeleted || t.status !== 'deleted')
      .sort((a, b) => Number(a.id) - Number(b.id));
  }

  get(id: string): Task | undefined {
    return this.tasks.get(id);
  }

  /** Mark a task in_progress. The TUI uses the 'started' event to
   *  trigger a between-task compact so the model starts the new task
   *  with a fresh narrow context. */
  start(id: string): Task {
    const t = this.tasks.get(id);
    if (!t) throw new Error(`task ${id} not found`);
    if (t.status === 'in_progress') return t;
    if (t.status === 'completed' || t.status === 'deleted') {
      throw new Error(`task ${id} is ${t.status}, cannot start`);
    }
    // Demote any other in_progress task back to pending - only one
    // task is active at a time. Mirrors the reference's "only one
    // in_progress" invariant.
    for (const other of this.tasks.values()) {
      if (other.id !== id && other.status === 'in_progress') {
        other.status = 'pending';
        this.emit({ kind: 'updated', task: other, previousStatus: 'in_progress' });
      }
    }
    const previousStatus = t.status;
    t.status = 'in_progress';
    t.startedAt = Date.now();
    this.emit({ kind: 'updated', task: t, previousStatus });
    this.emit({ kind: 'started', task: t });
    return t;
  }

  update(
    id: string,
    patch: { status?: TaskStatus; subject?: string; description?: string; activeForm?: string; summary?: string },
  ): Task {
    const t = this.tasks.get(id);
    if (!t) throw new Error(`task ${id} not found`);
    const previousStatus = t.status;
    if (patch.status === 'in_progress') return this.start(id);
    if (patch.subject !== undefined) t.subject = patch.subject.trim();
    if (patch.description !== undefined) t.description = patch.description.trim();
    if (patch.activeForm !== undefined) t.activeForm = patch.activeForm.trim() || undefined;
    if (patch.summary !== undefined) t.summary = patch.summary;
    if (patch.status !== undefined && patch.status !== t.status) {
      t.status = patch.status;
      if (patch.status === 'completed' && !t.completedAt) t.completedAt = Date.now();
    }
    this.emit({ kind: 'updated', task: t, previousStatus });
    if (patch.status === 'completed' && previousStatus !== 'completed') {
      this.emit({ kind: 'completed', task: t });
    }
    return t;
  }

  /** Find the next pending task and start it. Returns the started task
   *  or undefined if no pending tasks remain. */
  next(): Task | undefined {
    const pending = this.list().find((t) => t.status === 'pending');
    if (!pending) return undefined;
    return this.start(pending.id);
  }

  /** Drop everything. Called by /tasks-clear or at session reset. */
  clear(): void {
    this.tasks.clear();
    this.nextNum = 1;
    this.emit({ kind: 'cleared' });
  }

  /** Snapshot the full task list for persistence. Returns plain
   *  objects (not Map entries) so JSON.stringify works. */
  snapshot(): { tasks: Task[]; nextNum: number } {
    return {
      tasks: this.list(true).map((t) => ({ ...t })),
      nextNum: this.nextNum,
    };
  }

  /** Restore the task list from a snapshot (e.g. on session resume).
   *  Replaces any current state. Fires a single 'cleared' event so
   *  subscribers re-pull the full list. */
  rehydrate(snapshot: { tasks?: Task[]; nextNum?: number } | undefined): void {
    this.tasks.clear();
    this.nextNum = 1;
    if (!snapshot?.tasks?.length) {
      this.emit({ kind: 'cleared' });
      return;
    }
    for (const t of snapshot.tasks) {
      this.tasks.set(t.id, { ...t });
    }
    if (snapshot.nextNum && snapshot.nextNum > this.nextNum) {
      this.nextNum = snapshot.nextNum;
    } else {
      // Fall back: pick next id one past the max we saw.
      let maxId = 0;
      for (const t of snapshot.tasks) {
        const n = Number(t.id);
        if (Number.isFinite(n) && n > maxId) maxId = n;
      }
      this.nextNum = maxId + 1;
    }
    this.emit({ kind: 'cleared' });
  }

  /** Active in_progress task, if any. The TUI uses this for the
   *  spinner-word override ("Running tests..." instead of the hippo
   *  word rotator). */
  active(): Task | undefined {
    return [...this.tasks.values()].find((t) => t.status === 'in_progress');
  }

  /** Aggregate counts for the status bar. */
  counts(): { pending: number; inProgress: number; completed: number; total: number } {
    let pending = 0,
      inProgress = 0,
      completed = 0;
    for (const t of this.tasks.values()) {
      if (t.status === 'pending') pending++;
      else if (t.status === 'in_progress') inProgress++;
      else if (t.status === 'completed') completed++;
    }
    return { pending, inProgress, completed, total: pending + inProgress + completed };
  }

  subscribe(cb: Listener): () => void {
    this.listeners.add(cb);
    return () => this.listeners.delete(cb);
  }

  private emit(ev: TaskEvent): void {
    for (const cb of this.listeners) {
      try {
        cb(ev);
      } catch {
        /* listener bugs are not our problem */
      }
    }
  }
}

/** Single-line label for a task, used in status bars + transcript. */
export function statusGlyph(s: TaskStatus): string {
  switch (s) {
    case 'pending':
      return '○';
    case 'in_progress':
      return '◆';
    case 'completed':
      return '✓';
    case 'deleted':
      return '✗';
  }
}

/** Format the task list as a markdown checklist - what the model sees
 *  when it calls task_list, and what /tasks prints. The id is written
 *  as `id=N` so the model can copy it verbatim into task_update / task_get
 *  without picking up a display-only prefix. */
export function formatTaskList(tasks: Task[]): string {
  if (tasks.length === 0) return '(no tasks)';
  return tasks
    .map((t) => {
      const tag = t.activeForm && t.status === 'in_progress' ? `  (${t.activeForm})` : '';
      return `${statusGlyph(t.status)} id=${t.id}  ${t.subject}${tag}`;
    })
    .join('\n');
}

export const taskManager = new TaskManager();
