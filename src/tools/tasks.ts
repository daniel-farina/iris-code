// Task-system tools: task_create, task_list, task_get, task_update,
// task_next. Thin wrappers around src/task_manager.ts. Designed for
// the same shape as claude-code-main's TaskCreate/TaskUpdate/TaskList/
// TaskGet trio plus a `task_next` convenience that claims-and-starts
// the next pending task (so the model can chain through the plan
// without re-reading the list each time).

import { formatTaskList, taskManager, type TaskStatus } from '../task_manager.js';
import { argString, type Tool } from './types.js';

/** Normalize an incoming task id. The TUI panel renders ids as "T1",
 *  "T2", etc. (purely visual), but the manager stores plain numeric
 *  strings ("1", "2"). The model sometimes copy-pastes the display
 *  label into a tool call — strip the leading "T"/"t" so both forms
 *  work. Also trims whitespace. */
function normalizeId(raw: string | undefined): string | undefined {
  if (!raw) return undefined;
  const t = raw.trim();
  if (!t) return undefined;
  return /^t\d+$/i.test(t) ? t.slice(1) : t;
}

function fmtTask(t: ReturnType<typeof taskManager.get>): string {
  if (!t) return '(not found)';
  const lines = [`task ${t.id}  ${t.subject}  [${t.status}]`, `  ${t.description}`];
  if (t.activeForm) lines.push(`  active form: ${t.activeForm}`);
  if (t.startedAt) lines.push(`  started: ${new Date(t.startedAt).toISOString()}`);
  if (t.completedAt) lines.push(`  completed: ${new Date(t.completedAt).toISOString()}`);
  if (t.summary) lines.push(`  summary: ${t.summary}`);
  return lines.join('\n');
}

export const taskCreateTool: Tool = {
  name: 'task_create',
  spec: {
    type: 'function',
    function: {
      name: 'task_create',
      description:
        'Add a planned task to the session. Use for 3+ step requests so you can plan first, then work one task at a time. Call task_next to start the first.',
      parameters: {
        type: 'object',
        properties: {
          subject: { type: 'string', description: 'brief imperative title' },
          description: {
            type: 'string',
            description: 'detail enough to resume from this text alone after a context reset',
          },
          active_form: { type: 'string', description: 'present-continuous form for the spinner' },
        },
        required: ['subject', 'description'],
      },
    },
  },
  async run(args) {
    const subject = argString(args, 'subject');
    const description = argString(args, 'description');
    const activeForm = argString(args, 'active_form');
    if (!subject) throw new Error('task_create: missing subject');
    if (!description) throw new Error('task_create: missing description');
    const t = taskManager.create(subject, description, activeForm);
    return `created task id="${t.id}": ${t.subject}`;
  },
};

export const taskListTool: Tool = {
  name: 'task_list',
  spec: {
    type: 'function',
    function: {
      name: 'task_list',
      description: 'List all tasks (status glyph + id + subject).',
      parameters: { type: 'object', properties: {}, required: [] },
    },
  },
  async run() {
    const list = taskManager.list();
    const counts = taskManager.counts();
    const header =
      `tasks: ${counts.total} total · ${counts.pending} pending · ` +
      `${counts.inProgress} in_progress · ${counts.completed} completed`;
    return `${header}\n${formatTaskList(list)}`;
  },
};

export const taskGetTool: Tool = {
  name: 'task_get',
  spec: {
    type: 'function',
    function: {
      name: 'task_get',
      description: 'Read full task details (description, timestamps, summary).',
      parameters: {
        type: 'object',
        properties: {
          id: { type: 'string', description: 'bare numeric task id (e.g. "1")' },
        },
        required: ['id'],
      },
    },
  },
  async run(args) {
    const id = normalizeId(argString(args, 'id'));
    if (!id) throw new Error('task_get: missing id');
    return fmtTask(taskManager.get(id));
  },
};

const VALID_STATUSES: TaskStatus[] = ['pending', 'in_progress', 'completed', 'deleted'];

export const taskUpdateTool: Tool = {
  name: 'task_update',
  spec: {
    type: 'function',
    function: {
      name: 'task_update',
      description:
        'Update a task. Mark `in_progress` BEFORE work, `completed` AS SOON AS done. Setting `in_progress` triggers a between-task context reset on the next round. Only one task in_progress at a time.',
      parameters: {
        type: 'object',
        properties: {
          id: { type: 'string', description: 'bare numeric task id' },
          status: { type: 'string', enum: VALID_STATUSES },
          subject: { type: 'string' },
          description: { type: 'string' },
          active_form: { type: 'string' },
          summary: { type: 'string', description: 'one-line summary for completed transitions' },
        },
        required: ['id'],
      },
    },
  },
  async run(args) {
    const id = normalizeId(argString(args, 'id'));
    if (!id) throw new Error('task_update: missing id');
    const status = argString(args, 'status') as TaskStatus | undefined;
    if (status && !VALID_STATUSES.includes(status)) {
      throw new Error(`task_update: invalid status "${status}"; must be one of ${VALID_STATUSES.join(', ')}`);
    }
    const t = taskManager.update(id, {
      status,
      subject: argString(args, 'subject'),
      description: argString(args, 'description'),
      activeForm: argString(args, 'active_form'),
      summary: argString(args, 'summary'),
    });
    return `updated id="${t.id}": status=${t.status} · ${t.subject}`;
  },
};

export const taskNextTool: Tool = {
  name: 'task_next',
  spec: {
    type: 'function',
    function: {
      name: 'task_next',
      description:
        'Claim the next pending task and mark it in_progress. Triggers a between-task context reset on the next round.',
      parameters: { type: 'object', properties: {}, required: [] },
    },
  },
  async run() {
    const next = taskManager.next();
    if (!next) {
      const counts = taskManager.counts();
      return `all tasks done (${counts.completed} completed, ${counts.total} total)`;
    }
    return [
      `now working on task id="${next.id}": ${next.subject}`,
      `${next.description}`,
      `[context will reset on the next round - sidecar summary lines preserved]`,
    ].join('\n');
  },
};
