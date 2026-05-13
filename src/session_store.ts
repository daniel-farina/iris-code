// Session persistence. Each completed --print or REPL session writes a
// JSONL record to ~/.hip/sessions.jsonl. --continue-last and --resume
// reload by session id. Mirrors hip's session_store.rs in spirit (same
// path conventions adapted from MLX_CODE_HOME) but simpler - just the
// conversation array, no fancy compaction.

import { existsSync } from 'node:fs';
import { appendFile, mkdir, readFile, writeFile } from 'node:fs/promises';
import { homedir } from 'node:os';
import { join } from 'node:path';
import type { ChatMessage } from './schema.js';

// Read HIP_HOME lazily so tests that change it between cases get the
// current value (module-cached consts were sticking to the first test's
// tmpdir).
function homeDirPath(): string {
  return process.env['HIP_HOME'] ?? join(homedir(), '.hip');
}
function sessionsLogPath(): string {
  return join(homeDirPath(), 'sessions.jsonl');
}

export interface SessionRecord {
  session_id: string;
  ts_unix: number;
  cwd: string;
  /** First user message; used as the picker label. */
  first_user: string;
  /** Full conversation (system + user/assistant/tool messages). */
  conv: ChatMessage[];
}

async function ensureDir(): Promise<void> {
  const dir = homeDirPath();
  if (!existsSync(dir)) await mkdir(dir, { recursive: true });
}

export async function appendSession(rec: SessionRecord): Promise<void> {
  await ensureDir();
  await appendFile(sessionsLogPath(), JSON.stringify(rec) + '\n', 'utf8');
}

export async function readAllSessions(): Promise<SessionRecord[]> {
  const p = sessionsLogPath();
  if (!existsSync(p)) return [];
  const body = await readFile(p, 'utf8');
  const out: SessionRecord[] = [];
  for (const line of body.split('\n')) {
    if (!line.trim()) continue;
    try {
      const rec = JSON.parse(line) as SessionRecord;
      sanitizeMalformedToolCalls(rec);
      out.push(rec);
    } catch {
      // Skip malformed lines.
    }
  }
  return out;
}

/** A session persisted before the agent.ts sanitizer fix may contain
 *  assistant messages with malformed tool_call.arguments. Resuming
 *  such a session sends invalid JSON to MTPLX → 422. Rewrite any
 *  unparseable args to the same stub shape the live sanitizer uses,
 *  so a recovered session can keep going. Idempotent. */
function sanitizeMalformedToolCalls(rec: SessionRecord): void {
  for (const msg of rec.conv) {
    if (msg.role !== 'assistant' || !msg.tool_calls) continue;
    for (const call of msg.tool_calls) {
      if (!call.function?.arguments) continue;
      try {
        JSON.parse(call.function.arguments);
      } catch {
        call.function.arguments = JSON.stringify({
          _malformed: true,
          _hint: 'arguments were truncated in a previous run; sanitized on load',
          _original_size_bytes: call.function.arguments.length,
        });
      }
    }
  }
}

export async function findSession(sessionId: string): Promise<SessionRecord | undefined> {
  const all = await readAllSessions();
  // Latest record wins if there are duplicates.
  for (let i = all.length - 1; i >= 0; i--) {
    if (all[i]!.session_id === sessionId) return all[i];
  }
  return undefined;
}

export async function lastSession(): Promise<SessionRecord | undefined> {
  const all = await readAllSessions();
  return all[all.length - 1];
}

/** Most recent session whose cwd matches the given path. Used by
 *  --continue-last so the user resumes THIS project's last conv, not
 *  some unrelated dir's. Falls back to undefined if nothing matches. */
export async function lastSessionForCwd(cwd: string): Promise<SessionRecord | undefined> {
  const all = await readAllSessions();
  for (let i = all.length - 1; i >= 0; i--) {
    if (all[i]?.cwd === cwd) return all[i];
  }
  return undefined;
}

/** Replace the conversation for an existing session id (rewrites the
 * whole sessions.jsonl with the updated record). Cheap for normal sizes;
 * if the log grows huge we'd switch to one-file-per-session. */
export async function updateSession(rec: SessionRecord): Promise<void> {
  await ensureDir();
  const all = await readAllSessions();
  const idx = all.findIndex((r) => r.session_id === rec.session_id);
  if (idx >= 0) all[idx] = rec;
  else all.push(rec);
  const body = all.map((r) => JSON.stringify(r)).join('\n') + '\n';
  await writeFile(sessionsLogPath(), body, 'utf8');
}

export function homeDir(): string {
  return homeDirPath();
}
