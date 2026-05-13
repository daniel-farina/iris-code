// hip's lightweight MTPLX lifecycle manager.
//
// Detects an already-running MTPLX server, reads its env + cmdline, and
// diffs against the settings hippo-code recommends. Used by `hip --setup`
// (interactive: show diff, restart with our env layered in) and by the
// startup banner check that fires every launch (non-interactive: warns
// if the running config diverges).
//
// We intentionally do NOT manage MTPLX state long-term - hip just nudges
// the user once and stays out of the way. Restarting is a one-shot via
// SIGTERM + spawn-detached so MTPLX outlives hip.

import { spawn, spawnSync } from 'node:child_process';

const ACCENT = '\x1b[38;2;175;146;204m';
const DIM = '\x1b[2m';
const WARN = '\x1b[38;5;178m';
const RESET = '\x1b[0m';

/** Settings hippo-code wants MTPLX to run with. Add new keys here as
 *  we discover new tunables that matter. */
export const RECOMMENDED_ENV: Readonly<Record<string, string>> = Object.freeze({
  // Bumps MTPLX's session-bank entry cap from 8 → 32. Without this,
  // hip's long-prefix sessions get evicted past ~16K tokens (see the
  // POLICY_MISMATCH investigation; engine_session.py:772 reads this).
  MTPLX_SESSION_BANK_MAX_ENTRIES: '32',
});

export interface MtplxStatus {
  running: boolean;
  baseUrl: string;
  pid?: number;
  cmdline?: string[];
  /** Only MTPLX_* / MLX_* env vars; we don't snoop everything. */
  env?: Record<string, string>;
  /** Parsed from /v1/models, if available. */
  versionHint?: string;
  reachable?: boolean;
}

/** Probe MTPLX at the given baseUrl. Combines an HTTP health check with
 *  ps/lsof-based process inspection. Returns whatever could be gathered;
 *  caller decides what to do about missing fields. */
export async function getMtplxStatus(baseUrl: string): Promise<MtplxStatus> {
  const status: MtplxStatus = { running: false, baseUrl };

  // 1. HTTP probe.
  try {
    const ctl = new AbortController();
    const t = setTimeout(() => ctl.abort(), 1500);
    const res = await fetch(`${baseUrl.replace(/\/$/, '')}/models`, { signal: ctl.signal });
    clearTimeout(t);
    if (res.ok) {
      status.reachable = true;
      try {
        const body = (await res.json()) as { data?: { id?: string }[] };
        status.versionHint = body.data?.[0]?.id;
      } catch {
        /* best-effort */
      }
    }
  } catch {
    status.reachable = false;
  }

  // 2. PID from lsof on the port.
  const portMatch = baseUrl.match(/:(\d+)(?:\/|$)/);
  const port = portMatch?.[1] ?? '8088';
  const pid = findPidOnPort(port);
  if (pid) {
    status.pid = pid;
    status.running = true;
    status.cmdline = readProcessCmdline(pid);
    status.env = readProcessMtplxEnv(pid);
  } else {
    status.running = status.reachable === true; // reachable but no pid (rare)
  }
  return status;
}

function findPidOnPort(port: string): number | undefined {
  // -nP avoids DNS + service-name lookups (fast), -i :PORT filters.
  const r = spawnSync('lsof', ['-nP', '-iTCP:' + port, '-sTCP:LISTEN', '-t'], {
    encoding: 'utf8',
  });
  if (r.status !== 0) return undefined;
  const pid = Number(r.stdout.trim().split('\n')[0]);
  return Number.isFinite(pid) && pid > 0 ? pid : undefined;
}

function readProcessCmdline(pid: number): string[] | undefined {
  // ps -p PID -o command= prints the full argv as a single line; split
  // on whitespace is good enough for our use (paths with spaces are
  // unlikely in MTPLX's argv).
  const r = spawnSync('ps', ['-p', String(pid), '-o', 'command='], { encoding: 'utf8' });
  if (r.status !== 0) return undefined;
  const line = r.stdout.trim();
  if (!line) return undefined;
  return line.split(/\s+/);
}

function readProcessMtplxEnv(pid: number): Record<string, string> {
  // `ps eww` includes env after the command. macOS prints `KEY=VAL`
  // tokens space-separated. We pull just MTPLX_* and MLX_* (and
  // LIBRARY_PATH style, ignored for now).
  const r = spawnSync('ps', ['eww', String(pid)], { encoding: 'utf8' });
  const env: Record<string, string> = {};
  if (r.status !== 0) return env;
  for (const tok of r.stdout.split(/\s+/)) {
    if (!/^MTPLX_|^MLX_/.test(tok)) continue;
    const eq = tok.indexOf('=');
    if (eq < 0) continue;
    env[tok.slice(0, eq)] = tok.slice(eq + 1);
  }
  return env;
}

export interface ConfigDiff {
  /** Keys whose current value differs from recommended (or is missing). */
  missing: { key: string; current: string | undefined; recommended: string }[];
  /** Keys that match recommended already. */
  matching: { key: string; value: string }[];
}

export function diffConfig(
  current: Record<string, string> | undefined,
  recommended: Readonly<Record<string, string>>,
): ConfigDiff {
  const missing: ConfigDiff['missing'] = [];
  const matching: ConfigDiff['matching'] = [];
  const cur = current ?? {};
  for (const [key, want] of Object.entries(recommended)) {
    const have = cur[key];
    if (have === want) matching.push({ key, value: have });
    else missing.push({ key, current: have, recommended: want });
  }
  return { missing, matching };
}

/** Render a human-readable diff. Used by both --setup and the startup
 *  drift warning. */
export function formatDiff(status: MtplxStatus, diff: ConfigDiff): string {
  const lines: string[] = [];
  if (!status.running) {
    lines.push(`${WARN}MTPLX not running at ${status.baseUrl}${RESET}`);
    return lines.join('\n');
  }
  lines.push(
    `${ACCENT}MTPLX${RESET} running on ${status.baseUrl} (pid ${status.pid ?? '?'}${
      status.versionHint ? `, model ${status.versionHint}` : ''
    })`,
  );
  if (diff.matching.length > 0) {
    lines.push(`${DIM}  matching recommended:${RESET}`);
    for (const m of diff.matching) lines.push(`    ${m.key}=${m.value}`);
  }
  if (diff.missing.length > 0) {
    lines.push(`${WARN}  needs update:${RESET}`);
    for (const m of diff.missing) {
      const curStr = m.current === undefined ? '(unset)' : m.current;
      lines.push(`    ${m.key}: ${curStr} → ${m.recommended}`);
    }
  }
  return lines.join('\n');
}

/** Quick non-blocking drift check called on every hip launch. Returns
 *  a one-line warning or empty string if everything's fine. Never
 *  prompts. */
export async function quickDriftCheck(baseUrl: string): Promise<string> {
  let status: MtplxStatus;
  try {
    status = await getMtplxStatus(baseUrl);
  } catch {
    return '';
  }
  if (!status.running) return '';
  const diff = diffConfig(status.env, RECOMMENDED_ENV);
  if (diff.missing.length === 0) return '';
  const missingKeys = diff.missing.map((m) => m.key).join(', ');
  return (
    `${DIM}[hip] MTPLX config drift detected (${missingKeys}). ` +
    `Run \`hip --setup\` to apply hippo-code's recommended settings.${RESET}`
  );
}

/** Send SIGTERM, wait for the port to free, then return. */
export async function stopMtplx(pid: number, port: string, timeoutMs = 10000): Promise<void> {
  try {
    process.kill(pid, 'SIGTERM');
  } catch {
    // Already gone.
    return;
  }
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (!findPidOnPort(port)) return;
    await sleep(200);
  }
  // Last resort: SIGKILL.
  try {
    process.kill(pid, 'SIGKILL');
  } catch {
    /* ignore */
  }
  // Wait once more.
  await sleep(500);
}

/** Spawn MTPLX detached with the given cmdline + env. Returns the new
 *  pid, or throws if spawn fails. */
export function startMtplx(
  cmdline: string[],
  env: Record<string, string>,
  logFile = '/tmp/mtplx-hip-launch.log',
): number {
  const [bin, ...args] = cmdline;
  if (!bin) throw new Error('startMtplx: cmdline must have at least one element');
  // Append log header so the user can tell apart launches.
  const fs = require('node:fs') as typeof import('node:fs');
  fs.appendFileSync(
    logFile,
    `\n=== hip-launched MTPLX at ${new Date().toISOString()} ===\nargv: ${cmdline.join(' ')}\n`,
  );
  const out = fs.openSync(logFile, 'a');
  const child = spawn(bin, args, {
    env: { ...process.env, ...env },
    detached: true,
    stdio: ['ignore', out, out],
  });
  child.unref();
  if (!child.pid) throw new Error('startMtplx: spawn returned no pid');
  return child.pid;
}

/** Block until MTPLX answers on baseUrl, or throw on timeout. */
export async function waitForMtplx(baseUrl: string, timeoutMs = 60000): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    try {
      const ctl = new AbortController();
      const t = setTimeout(() => ctl.abort(), 1000);
      const res = await fetch(`${baseUrl.replace(/\/$/, '')}/models`, { signal: ctl.signal });
      clearTimeout(t);
      if (res.ok) return;
    } catch {
      /* still booting */
    }
    await sleep(500);
  }
  throw new Error(`MTPLX did not come up within ${timeoutMs}ms at ${baseUrl}`);
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}
