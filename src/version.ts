// Single source of truth for hip's version string. Read from
// package.json at module load so the bundled binary always reflects
// what was actually shipped, not a hardcoded constant that can drift.
// The file is small and the read is one-shot at import time.

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

function resolveVersion(): string {
  // From src/version.ts → ../package.json
  const here = dirname(fileURLToPath(import.meta.url));
  const candidate = join(here, '..', 'package.json');
  try {
    const pkg = JSON.parse(readFileSync(candidate, 'utf8')) as { version?: string };
    if (typeof pkg.version === 'string' && pkg.version.length > 0) return pkg.version;
  } catch {
    /* fall through */
  }
  return 'unknown';
}

export const VERSION = resolveVersion();
