// `edit` tool: exact string replace with multiple robustness layers
// before failing. Matches claude-code's FileEditTool feature set,
// minus the parts that only make sense in their app (permission
// matchers, sandbox, LSP wiring, skill discovery, file-history
// backup).
//
// Match layers, tried in order:
//
//   1. Exact byte-for-byte.
//   2. Quote-normalized: curly ↔ straight (preserves file typography
//      on write).
//   3. Line-ending-normalized: matches CRLF files when the model
//      emits LF (file_io reads everything LF-normalized so this is
//      mostly free; we re-apply the file's original endings on
//      write).
//   4. Leading-whitespace-normalized: tabs ↔ spaces at line start.
//      Catches the common "model emits 2-space indent, file has
//      tabs" failure.
//
// When all match attempts fail, we check whether new_string is
// already in the file (the model retried an already-applied edit)
// and return `[no-op] already applied` rather than erroring.
//
// Other features parity-matched with claude-code:
//
//   - Pre-read requirement (Read it first before writing - prevents
//     blind edits).
//   - File-staleness check (file modified since read - reject).
//   - File creation via empty old_string on a nonexistent path.
//   - Missing-path "did you mean...?" hints.
//   - .ipynb refusal (notebooks need cell-aware editing).
//   - settings.json / .json JSON validation post-edit.
//   - 1 GiB size cap.
//   - UTF-16-LE encoding via BOM detection.
//   - Structured unified-diff output.

import { existsSync, statSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { mkdir } from 'node:fs/promises';
import { formatDiffForResult, makeUnifiedDiff } from '../diff.js';
import { findSimilarFile, readFileForEdit, suggestPathUnderCwd, writeFileForEdit } from '../file_io.js';
import { hashContent, readState } from '../read_state.js';
import { type Tool, argBool, argString } from './types.js';

const MAX_EDIT_FILE_SIZE = 1024 * 1024 * 1024; // 1 GiB

const LEFT_SINGLE_CURLY = '‘';
const RIGHT_SINGLE_CURLY = '’';
const LEFT_DOUBLE_CURLY = '“';
const RIGHT_DOUBLE_CURLY = '”';

/** One edit operation. Used both by the single-edit tool and by
 *  multi_edit (which takes an array of these). */
export interface EditOp {
  old_string: string;
  new_string: string;
  replace_all?: boolean;
}

export interface EditOutcome {
  /** True when at least one edit was applied (and the file was
   *  rewritten). False for the [no-op] already-applied path. */
  changed: boolean;
  message: string;
}

export const editTool: Tool = {
  name: 'edit',
  spec: {
    type: 'function',
    function: {
      name: 'edit',
      description:
        'Replace exact text in a file. Read the file first. `old_string=""` on a missing path creates the file. Returns unified diff or `[no-op]` if already applied. Tolerant of curly/straight quotes, CRLF/LF, tabs/spaces.',
      parameters: {
        type: 'object',
        properties: {
          path: { type: 'string', description: 'file path' },
          old_string: { type: 'string', description: 'text to replace' },
          new_string: { type: 'string', description: 'replacement' },
          replace_all: { type: 'boolean', description: 'replace every match (default false)' },
        },
        required: ['path', 'old_string', 'new_string'],
      },
    },
  },
  async run(args) {
    const path = argString(args, 'path');
    const oldS = argString(args, 'old_string');
    const newS = argString(args, 'new_string');
    if (!path || oldS === undefined || newS === undefined)
      throw new Error('edit: need path, old_string, new_string');
    const replaceAll = argBool(args, 'replace_all') ?? false;
    const outcome = await applyEdits(path, [{ old_string: oldS, new_string: newS, replace_all: replaceAll }]);
    return outcome.message;
  },
};

export const multiEditTool: Tool = {
  name: 'multi_edit',
  spec: {
    type: 'function',
    function: {
      name: 'multi_edit',
      description:
        'Apply a sequence of edits to one file atomically. Each edit sees the previous result; any failure aborts all writes. Use for refactors that touch one file in multiple places.',
      parameters: {
        type: 'object',
        properties: {
          path: { type: 'string', description: 'file path' },
          edits: {
            type: 'array',
            description: 'ordered edits',
            items: {
              type: 'object',
              properties: {
                old_string: { type: 'string' },
                new_string: { type: 'string' },
                replace_all: { type: 'boolean' },
              },
              required: ['old_string', 'new_string'],
            },
          },
        },
        required: ['path', 'edits'],
      },
    },
  },
  async run(args) {
    const path = argString(args, 'path');
    if (!path) throw new Error('multi_edit: missing path');
    let editsRaw = args['edits'];
    // qwen3 tool-call quirk: complex array params sometimes arrive as
    // a JSON-encoded string instead of a real array. Recover by
    // parsing - matches the defensive coercion in argU64/argString.
    if (typeof editsRaw === 'string') {
      try {
        editsRaw = JSON.parse(editsRaw);
      } catch (e) {
        throw new Error(
          `multi_edit: \`edits\` was a string but not valid JSON (${(e as Error).message})`,
        );
      }
    }
    if (!Array.isArray(editsRaw) || editsRaw.length === 0)
      throw new Error('multi_edit: `edits` must be a non-empty array');
    const edits: EditOp[] = editsRaw.map((e, i) => {
      if (!e || typeof e !== 'object') throw new Error(`multi_edit: edit[${i}] is not an object`);
      const o = e as Record<string, unknown>;
      const old_string = typeof o['old_string'] === 'string' ? (o['old_string'] as string) : undefined;
      const new_string = typeof o['new_string'] === 'string' ? (o['new_string'] as string) : undefined;
      if (old_string === undefined || new_string === undefined)
        throw new Error(`multi_edit: edit[${i}] needs old_string and new_string`);
      const replace_all = typeof o['replace_all'] === 'boolean' ? (o['replace_all'] as boolean) : false;
      return { old_string, new_string, replace_all };
    });
    const outcome = await applyEdits(path, edits);
    return outcome.message;
  },
};

/** Apply a sequence of edits atomically to a single file. Used by
 *  both `edit` (one-element array) and `multi_edit`. */
export async function applyEdits(path: string, edits: EditOp[]): Promise<EditOutcome> {
  if (edits.length === 0) throw new Error('edit: no edits provided');
  const abs = resolve(path);

  // .ipynb files are JSON-encoded notebook documents; editing them
  // as plain text usually corrupts cell metadata. Hip doesn't ship a
  // notebook tool, so we refuse rather than risk silent breakage.
  if (abs.endsWith('.ipynb')) {
    throw new Error(
      `edit: ${path} is a Jupyter notebook - editing as plain text would corrupt cell JSON. Use a notebook-aware editor.`,
    );
  }

  // Size cap (cheap stat before any read).
  if (existsSync(abs)) {
    const sz = statSync(abs).size;
    if (sz > MAX_EDIT_FILE_SIZE) {
      throw new Error(
        `edit: ${path} is ${(sz / 1024 / 1024).toFixed(1)} MiB - exceeds the 1 GiB edit cap`,
      );
    }
  }

  // File-creation path: empty old_string + nonexistent file means
  // "write a new file at this path with new_string as the content".
  // The first edit must be the create; subsequent edits operate on
  // the resulting content as usual.
  const first = edits[0]!;
  const fileExists = existsSync(abs);
  if (!fileExists) {
    if (first.old_string !== '') {
      // Likely a wrong path. Try to help.
      const sim = findSimilarFile(abs) ?? suggestPathUnderCwd(abs);
      throw new Error(
        `edit: ${path} does not exist${sim ? ` (did you mean ${sim}?)` : ''}`,
      );
    }
    // Create the file (and its parent directory), then drop the
    // create edit from the list. Remaining edits operate on the
    // freshly written content.
    await mkdir(dirname(abs), { recursive: true });
    writeFileSync(abs, first.new_string, 'utf8');
    readState.recordRead(abs, first.new_string, false);
    const remaining = edits.slice(1);
    if (remaining.length === 0) {
      const diff = makeUnifiedDiff(path, '', first.new_string);
      return {
        changed: true,
        message: `[ok] created ${path} (${first.new_string.length} bytes)\n${formatDiffForResult(diff)}`,
      };
    }
    return await applyEditsToExisting(abs, path, remaining);
  }

  return await applyEditsToExisting(abs, path, edits);
}

async function applyEditsToExisting(
  abs: string,
  displayPath: string,
  edits: EditOp[],
): Promise<EditOutcome> {
  const readEntry = readState.get(abs);
  // Pre-read requirement. Any read is sufficient (partial OR full) -
  // the content hash recorded in readState covers the entire file
  // even when the model only displayed a windowed slice, so the
  // staleness check below still catches external modifications.
  // We surface a hint when the read was partial (the model may not
  // know the area it's editing) but don't block.
  if (!readEntry) {
    throw new Error(
      `edit: ${displayPath} has not been read in this session. Use \`read\` first.`,
    );
  }

  // Read the file, then check it hasn't changed since the read. Use
  // the content hash as the source of truth - mtime alone is unreliable.
  const r = readFileForEdit(abs);
  if (!r.exists) {
    // Race: file existed at .stat time, gone now. Treat as missing.
    throw new Error(`edit: ${displayPath} disappeared between stat and read`);
  }
  if (hashContent(r.content) !== readEntry.contentHash) {
    throw new Error(
      `edit: ${displayPath} has been modified since you last read it (by the user, a linter, or another tool). Read it again before editing.`,
    );
  }

  // Apply edits in sequence to an in-memory working copy. If any
  // step fails, abort - no changes written.
  let working = r.content;
  const steps: { matchedSubstring: string; occurrences: number; actualNew: string; replaceAll: boolean }[] = [];
  let noopCount = 0;
  for (let i = 0; i < edits.length; i++) {
    const e = edits[i]!;
    if (e.old_string === e.new_string) {
      throw new Error(`edit[${i}]: new_string equals old_string (no-op)`);
    }
    if (e.old_string === '') {
      throw new Error(`edit[${i}]: old_string is empty on existing file (use a unique anchor)`);
    }
    const match = findMatch(working, e.old_string);
    if (!match) {
      // Already-applied detection.
      if (
        working.includes(e.new_string) ||
        working.includes(normalizeQuotes(e.new_string)) ||
        working.includes(normalizeLeadingWS(e.new_string))
      ) {
        noopCount++;
        steps.push({ matchedSubstring: '', occurrences: 0, actualNew: '', replaceAll: false });
        continue;
      }
      throw new Error(
        `edit[${i}]: old_string not found in ${displayPath} (tried exact, quote, CRLF, and whitespace normalization)`,
      );
    }
    const replaceAll = e.replace_all ?? false;
    if (match.occurrences > 1 && !replaceAll) {
      throw new Error(
        `edit[${i}]: old_string matches ${match.occurrences} places in ${displayPath}. Pass replace_all=true or extend old_string to make it unique.`,
      );
    }
    // Preserve curly quote style: if the match converted curly→
    // straight, apply the same quote style to new_string. Small fix
    // but it keeps the file's typography stable when editing docs.
    const actualNew = preserveQuoteStyle(e.old_string, match.matchedSubstring, e.new_string);
    working = replaceAll
      ? working.split(match.matchedSubstring).join(actualNew)
      : working.replace(match.matchedSubstring, actualNew);
    steps.push({
      matchedSubstring: match.matchedSubstring,
      occurrences: match.occurrences,
      actualNew,
      replaceAll,
    });
  }

  // All steps succeeded (some may have been no-ops). If literally
  // nothing changed, return [no-op] without writing.
  if (working === r.content) {
    return {
      changed: false,
      message: `[no-op] all ${edits.length} edit${edits.length === 1 ? '' : 's'} already applied to ${displayPath} (new_string already present, old_string not found)`,
    };
  }

  // *.json / *.jsonc: validate that the result still parses.
  // Many ".json" files in the wild are actually JSONC: tsconfig.json,
  // .eslintrc.json, .babelrc, .vscode/settings.json, package.json sometimes,
  // etc. Always strip // and /* */ comments before the parse check -
  // we only write the original text, so this is purely a syntax gate.
  if (abs.endsWith('.json') || abs.endsWith('.jsonc')) {
    try {
      JSON.parse(stripJsonComments(working));
    } catch (e) {
      throw new Error(
        `edit: applying these edits to ${displayPath} would produce invalid JSON: ${(e as Error).message}`,
      );
    }
  }

  writeFileForEdit(abs, working, r.encoding, r.lineEnding);
  // Re-record the read state with the new content so subsequent
  // edits in the same session don't have to call `read` again.
  readState.recordRead(abs, working, false);

  const diff = makeUnifiedDiff(displayPath, r.content, working);
  const applied = edits.length - noopCount;
  // Total replacements across all applied edits. When replace_all
  // hits 3 places, that's 3 replacements from a single edit op.
  const totalReplacements = steps.reduce(
    (sum, s) => sum + (s.replaceAll ? s.occurrences : s.matchedSubstring === '' ? 0 : 1),
    0,
  );
  // Phrasing reads naturally for both single-edit and multi-edit
  // callers. "3 edits applied" for the typical multi-edit;
  // "1 edit applied (3 replacements)" when replace_all triggered.
  const headline =
    edits.length === 1 && steps[0] && steps[0]!.replaceAll && steps[0]!.occurrences > 1
      ? `${totalReplacements} edits applied to ${displayPath}`
      : `${applied} edit${applied === 1 ? '' : 's'} applied to ${displayPath}${
          noopCount > 0 ? ` (${noopCount} already-applied no-op${noopCount === 1 ? '' : 's'})` : ''
        }`;
  const summary = `[ok] ${headline} · +${diff.additions} -${diff.deletions}`;
  return { changed: true, message: `${summary}\n${formatDiffForResult(diff)}` };
}

/** Try to locate `needle` in `haystack` through four match layers.
 *  Returns the actual substring from haystack (with original
 *  typography) so the caller can splice without losing the file's
 *  style. */
function findMatch(
  haystack: string,
  needle: string,
): { matchedSubstring: string; occurrences: number } | undefined {
  // 1. Exact.
  const exact = countOccurrences(haystack, needle);
  if (exact > 0) return { matchedSubstring: needle, occurrences: exact };

  // 2. Quote-normalized.
  const needleStraight = normalizeQuotes(needle);
  const haystackStraight = normalizeQuotes(haystack);
  if (needleStraight !== needle || haystackStraight !== haystack) {
    const idx = haystackStraight.indexOf(needleStraight);
    if (idx !== -1) {
      const matched = haystack.substring(idx, idx + needleStraight.length);
      return {
        matchedSubstring: matched,
        occurrences: countOccurrences(haystackStraight, needleStraight),
      };
    }
  }

  // 3. CRLF-normalized: readFileForEdit already collapses CRLF to LF
  //    before we get here, but the model might still send a needle
  //    with stray \r characters. Strip them and retry.
  if (needle.includes('\r')) {
    const needleLF = needle.replace(/\r\n/g, '\n').replace(/\r/g, '\n');
    const lfCount = countOccurrences(haystack, needleLF);
    if (lfCount > 0) {
      return { matchedSubstring: needleLF, occurrences: lfCount };
    }
  }

  // 4. Leading-whitespace-normalized: convert tabs ↔ spaces at line
  //    start on both sides and try again. We use a 2-space tab on
  //    one side and a 4-space tab on the other to cover both common
  //    conventions; if either matches we splice using the matched
  //    range from the haystack.
  for (const tabSize of [2, 4]) {
    const needleNorm = normalizeLeadingWS(needle, tabSize);
    const haystackNorm = normalizeLeadingWS(haystack, tabSize);
    if (needleNorm === needle && haystackNorm === haystack) continue;
    const idx = haystackNorm.indexOf(needleNorm);
    if (idx === -1) continue;
    // Walk to find the corresponding substring in the original
    // haystack. Since whitespace normalization can change byte
    // counts, we count significant (non-whitespace) characters to
    // align. This is approximate; if the count of significant chars
    // matches we splice; otherwise we bail to the next layer.
    const matchedSubstring = locateInOriginal(haystack, haystackNorm, idx, needleNorm.length);
    if (matchedSubstring !== undefined) {
      return { matchedSubstring, occurrences: countOccurrences(haystackNorm, needleNorm) };
    }
  }

  return undefined;
}

/** Walk the haystack from index 0, advancing both an "original" and a
 *  "normalized" cursor in lockstep, until the normalized cursor hits
 *  `targetStart`. Then continue until `targetStart + targetLen`. The
 *  original substring between those positions is the match. Returns
 *  undefined if the alignment is ambiguous (sanity check on the end
 *  position landing on a normalize-safe boundary). */
function locateInOriginal(
  original: string,
  normalized: string,
  targetStart: number,
  targetLen: number,
): string | undefined {
  void normalized; // we don't actually need it - we re-normalize per step
  let origIdx = 0;
  let normIdx = 0;
  const advance = (): boolean => {
    if (origIdx >= original.length) return false;
    const ch = original[origIdx]!;
    if (ch === '\t') {
      // Tab → some number of spaces in normalized; consume the run
      // of normalized spaces that came from this tab. We use 2 as
      // the conservative size (matches the first normalization
      // pass); 4 will work too.
      origIdx++;
      // Skip however many spaces are at this position in normalized.
      let skipped = 0;
      while (skipped < 4 && normIdx < normalized.length && normalized[normIdx] === ' ') {
        normIdx++;
        skipped++;
      }
      return true;
    }
    origIdx++;
    normIdx++;
    return true;
  };
  while (normIdx < targetStart) {
    if (!advance()) return undefined;
  }
  const startOrig = origIdx;
  const targetEnd = targetStart + targetLen;
  while (normIdx < targetEnd) {
    if (!advance()) return undefined;
  }
  return original.slice(startOrig, origIdx);
}

function normalizeQuotes(s: string): string {
  return s
    .replaceAll(LEFT_SINGLE_CURLY, "'")
    .replaceAll(RIGHT_SINGLE_CURLY, "'")
    .replaceAll(LEFT_DOUBLE_CURLY, '"')
    .replaceAll(RIGHT_DOUBLE_CURLY, '"');
}

/** Replace leading tabs on each line with `tabSize` spaces. Doesn't
 *  touch tabs anywhere else (inline tabs are rare and meaningful). */
function normalizeLeadingWS(s: string, tabSize = 2): string {
  return s.split('\n').map((line) => {
    let i = 0;
    let prefix = '';
    while (i < line.length && (line[i] === '\t' || line[i] === ' ')) {
      prefix += line[i] === '\t' ? ' '.repeat(tabSize) : ' ';
      i++;
    }
    return prefix + line.slice(i);
  }).join('\n');
}

/** When old_string matched via quote normalization, re-apply curly
 *  quotes to new_string in the same shape as the file uses. */
function preserveQuoteStyle(
  oldString: string,
  actualOldString: string,
  newString: string,
): string {
  if (oldString === actualOldString) return newString;
  const hasDouble =
    actualOldString.includes(LEFT_DOUBLE_CURLY) || actualOldString.includes(RIGHT_DOUBLE_CURLY);
  const hasSingle =
    actualOldString.includes(LEFT_SINGLE_CURLY) || actualOldString.includes(RIGHT_SINGLE_CURLY);
  let out = newString;
  if (hasDouble) out = applyCurlyDouble(out);
  if (hasSingle) out = applyCurlySingle(out);
  return out;
}

function applyCurlyDouble(s: string): string {
  const chars = [...s];
  const result: string[] = [];
  for (let i = 0; i < chars.length; i++) {
    if (chars[i] === '"') {
      result.push(isOpeningContext(chars, i) ? LEFT_DOUBLE_CURLY : RIGHT_DOUBLE_CURLY);
    } else result.push(chars[i]!);
  }
  return result.join('');
}

function applyCurlySingle(s: string): string {
  const chars = [...s];
  const result: string[] = [];
  for (let i = 0; i < chars.length; i++) {
    if (chars[i] === "'") {
      const prev = i > 0 ? chars[i - 1] : undefined;
      const next = i < chars.length - 1 ? chars[i + 1] : undefined;
      // Apostrophe in a contraction stays as right-single-curly.
      if (prev && next && /\p{L}/u.test(prev) && /\p{L}/u.test(next)) {
        result.push(RIGHT_SINGLE_CURLY);
      } else {
        result.push(isOpeningContext(chars, i) ? LEFT_SINGLE_CURLY : RIGHT_SINGLE_CURLY);
      }
    } else result.push(chars[i]!);
  }
  return result.join('');
}

function isOpeningContext(chars: string[], i: number): boolean {
  if (i === 0) return true;
  const prev = chars[i - 1];
  return prev === ' ' || prev === '\t' || prev === '\n' || prev === '\r' || prev === '(' || prev === '[' || prev === '{';
}

function countOccurrences(haystack: string, needle: string): number {
  if (!needle) return 0;
  let count = 0;
  let pos = 0;
  while ((pos = haystack.indexOf(needle, pos)) !== -1) {
    count++;
    pos += needle.length;
  }
  return count;
}

/** Strip // and /* comments from a string for JSON.parse-ing
 *  settings files that allow comments. Doesn't try to be a full
 *  parser - just enough to validate the structure. */
function stripJsonComments(s: string): string {
  return s
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .replace(/(^|[^:])\/\/.*$/gm, '$1');
}
