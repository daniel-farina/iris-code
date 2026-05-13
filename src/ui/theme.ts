// Hippo-purple color palette. Ported from /Users/dan/code-2/mlx-code's
// src/theme.rs (River theme). Keep the hex values in lockstep with the
// Rust source so the ANSI logo and the Ink-rendered surfaces blend.
//
//   af92cc = hippo body / accent
//   9e7ebd = hippo shadow / highlight
//   5c486d = deep shadow / dim-purple
//
// Ink accepts hex via the `color` and `backgroundColor` props on <Text>.

import { existsSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

export const HIPPO_PURPLE = '#af92cc'; // accent: input prompt, user marker, highlights
export const HIPPO_SHADOW = '#9e7ebd'; // highlight: secondary accents, headings
export const HIPPO_DEEP = '#5c486d'; // dim: thinking, persisted-conv hints
export const HIPPO_MOSS = '#5f8f5f'; // good: success messages
export const HIPPO_MUSTARD = '#d7af00'; // warn: yellow status / hints
export const HIPPO_SOFT_RED = '#d7875f'; // bad: errors, cancellations

/** Lazily load the bundled hippo logo. Returns the raw ANSI string (with
 *  embedded escape codes) - print it directly to stdout/stderr; Ink's
 *  <Text> would strip the embedded escapes. */
export async function loadLogo(): Promise<string | undefined> {
  // src/ui/theme.ts → assets/hippo-logo.txt (../../assets relative to this file).
  const here = dirname(fileURLToPath(import.meta.url));
  const path = join(here, '..', '..', 'assets', 'hippo-logo.txt');
  if (!existsSync(path)) return undefined;
  return await readFile(path, 'utf8');
}
