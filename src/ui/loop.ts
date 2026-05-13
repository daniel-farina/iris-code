// /loop slash-command implementation. Modeled on Claude Code's /loop skill.
// Parses the same input grammar:
//
//   /loop 5m <prompt>             leading-token interval (^\d+[smhd]$)
//   /loop check the deploy every 20m   trailing "every Nu" clause
//   /loop <prompt>                no interval -> dynamic mode (not yet implemented)
//
// Each scheduled loop is an in-memory setInterval entry tagged with a
// short id. Lives only in this REPL session (no disk persistence) - same
// scoping as Claude Code's CronCreate session-only mode.

export interface LoopJob {
  id: string;
  intervalMs: number;
  cadence: string; // human-readable "every 5 minutes"
  prompt: string;
  handle: ReturnType<typeof setInterval>;
  fires: number;
  createdAt: number;
}

const jobs = new Map<string, LoopJob>();

/** Parse the `/loop` arg string. Returns `null` if it should fall back to
 * dynamic mode (no interval). On parse error returns a string with the
 * error to display. */
export function parseLoopInput(
  raw: string,
): { intervalMs: number; cadence: string; prompt: string } | { error: string } | null {
  const trimmed = raw.trim();
  if (!trimmed) return { error: 'usage: /loop [interval] <prompt>' };

  // Rule 1: leading token like "5m", "2h", "30s", "1d".
  const leading = trimmed.match(/^(\d+)([smhd])\s+(.+)$/s);
  if (leading) {
    const n = Number(leading[1]);
    const unit = leading[2];
    const prompt = leading[3]?.trim() ?? '';
    if (!prompt) return { error: 'usage: /loop [interval] <prompt>' };
    return makeInterval(n, unit!, prompt);
  }
  // Rule 2: trailing "every N<unit>" or "every N <unit-word>".
  const trailing = trimmed.match(
    /^(.+?)\s+every\s+(\d+)\s*(s|sec|secs|second|seconds|m|min|mins|minute|minutes|h|hr|hrs|hour|hours|d|day|days)\s*$/i,
  );
  if (trailing) {
    const prompt = (trailing[1] ?? '').trim();
    const n = Number(trailing[2]);
    const word = (trailing[3] ?? '').toLowerCase();
    const unit = word.startsWith('s')
      ? 's'
      : word.startsWith('m')
        ? 'm'
        : word.startsWith('h')
          ? 'h'
          : 'd';
    if (!prompt) return { error: 'usage: /loop [interval] <prompt>' };
    return makeInterval(n, unit, prompt);
  }
  // Rule 3: no interval. Dynamic mode - not implemented in v0.0.x. Hint
  // the user to retry with an interval until we wire monitor signals.
  return null;
}

function makeInterval(
  n: number,
  unit: string,
  prompt: string,
): { intervalMs: number; cadence: string; prompt: string } | { error: string } {
  const seconds = unit === 's' ? n : unit === 'm' ? n * 60 : unit === 'h' ? n * 3600 : n * 86400;
  if (!Number.isFinite(seconds) || seconds <= 0) return { error: `invalid interval: ${n}${unit}` };
  if (seconds < 5) return { error: 'minimum interval is 5s' };
  const cadenceMap: Record<string, string> = {
    s: 'seconds',
    m: 'minutes',
    h: 'hours',
    d: 'days',
  };
  const cadenceUnit = cadenceMap[unit] ?? unit;
  const cadence = `every ${n} ${cadenceUnit}`;
  return { intervalMs: seconds * 1000, cadence, prompt };
}

/** Schedule a loop. `dispatch` is called with the prompt every time the
 * timer fires. Returns the assigned id (8-char hex). */
export function scheduleLoop(
  intervalMs: number,
  cadence: string,
  prompt: string,
  dispatch: (prompt: string) => Promise<void> | void,
): LoopJob {
  const id = Math.floor(Math.random() * 0xffffffff)
    .toString(16)
    .padStart(8, '0');
  const job: LoopJob = {
    id,
    intervalMs,
    cadence,
    prompt,
    fires: 0,
    createdAt: Date.now(),
    handle: setInterval(() => {
      job.fires++;
      void dispatch(prompt);
    }, intervalMs),
  };
  jobs.set(id, job);
  return job;
}

export function listLoops(): LoopJob[] {
  return [...jobs.values()];
}

export function stopLoop(id: string): boolean {
  const job = jobs.get(id);
  if (!job) return false;
  clearInterval(job.handle);
  jobs.delete(id);
  return true;
}

export function stopAllLoops(): number {
  const count = jobs.size;
  for (const job of jobs.values()) clearInterval(job.handle);
  jobs.clear();
  return count;
}
