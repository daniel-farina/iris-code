// Tracks which files have been read in the current hip session and
// when. Edit refuses to write to a file that hasn't been read yet,
// matching claude-code's "Read it first before writing to it" rule.
// This prevents two failure modes:
//
//   1. The model edits a file it's never seen - guessing at content
//      that may not exist (whitespace mismatch, file already changed,
//      file doesn't even exist at the path the model picked).
//   2. The model edits a file that has been modified externally (by
//      a linter, by the user, by another process) between the read
//      and the edit - silently clobbering work.
//
// We store the timestamp at read time AND the content hash so the
// staleness check can fall back on content comparison when the mtime
// shifts without a real change (cloud sync, antivirus rescans).

import { createHash } from 'node:crypto';
import { resolve } from 'node:path';

export interface ReadEntry {
  /** Wall-clock ms when the read happened. */
  timestamp: number;
  /** SHA1 of the bytes we read. Used by the staleness check when
   *  mtime alone is unreliable. */
  contentHash: string;
  /** True when the read was a windowed slice (offset/limit/around) -
   *  edits to a partial read are rejected because the model can't
   *  see the whole file. */
  partial: boolean;
}

class ReadStateStore {
  private entries = new Map<string, ReadEntry>();

  /** Record that the given file was read. Path is canonicalized to
   *  the absolute form so windows/relative quirks don't break the
   *  lookup. */
  recordRead(path: string, content: string, partial: boolean): void {
    const abs = resolve(path);
    this.entries.set(abs, {
      timestamp: Date.now(),
      contentHash: hashContent(content),
      partial,
    });
  }

  /** Get the most recent read entry for the given path, or undefined
   *  if the file has not been read in this session. */
  get(path: string): ReadEntry | undefined {
    return this.entries.get(resolve(path));
  }

  /** Test helper: drop everything. */
  clear(): void {
    this.entries.clear();
  }
}

export function hashContent(content: string): string {
  return createHash('sha1').update(content).digest('hex');
}

export const readState = new ReadStateStore();
