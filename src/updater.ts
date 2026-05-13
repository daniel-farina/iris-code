// `hip --update`: hit the GitHub releases API, find the latest release,
// download the platform tarball, verify sha256, and atomically replace
// the running binary (or the local source tree if running from source).
//
// Modeled after the Rust hip updater at mlx-code/src/updater.rs but
// pared down: we only support tarball downloads for now, no brew path.

import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { chmodSync, existsSync, mkdtempSync, renameSync, statSync } from 'node:fs';
import { writeFile } from 'node:fs/promises';
import { arch as nodeArch, platform as nodePlatform, tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO = 'daniel-farina/hippo-code';
const API_LATEST = `https://api.github.com/repos/${REPO}/releases/latest`;
const ACCENT = '\x1b[38;2;175;146;204m';
const RESET = '\x1b[0m';

interface GHRelease {
  tag_name: string;
  name?: string;
  assets: { name: string; browser_download_url: string }[];
}

/** Map node's platform+arch into the tarball naming convention used by
 *  the release workflow. Keep in sync with .github/workflows/release.yml. */
function targetTriple(): string {
  const plat = nodePlatform();
  const arch = nodeArch();
  if (plat === 'darwin' && arch === 'arm64') return 'mac-arm64';
  if (plat === 'darwin' && arch === 'x64') return 'mac-x64';
  if (plat === 'linux' && arch === 'x64') return 'linux-x64';
  if (plat === 'linux' && arch === 'arm64') return 'linux-arm64';
  throw new Error(`unsupported platform: ${plat}/${arch}`);
}

async function fetchJson<T>(url: string): Promise<T> {
  const res = await fetch(url, {
    headers: { accept: 'application/vnd.github+json', 'user-agent': 'hip-updater' },
  });
  if (!res.ok) throw new Error(`GET ${url}: HTTP ${res.status}`);
  return (await res.json()) as T;
}

async function fetchBuffer(url: string): Promise<Buffer> {
  const res = await fetch(url, { headers: { 'user-agent': 'hip-updater' } });
  if (!res.ok) throw new Error(`GET ${url}: HTTP ${res.status}`);
  const ab = await res.arrayBuffer();
  return Buffer.from(ab);
}

function sha256(buf: Buffer): string {
  return createHash('sha256').update(buf).digest('hex');
}

/** Find this script's install root (where bin/ + src/ live). When
 *  running from a source checkout this is the repo root; for a tarball
 *  install it's wherever the user extracted the tarball. */
function installRoot(): string {
  // updater.ts → src/updater.js (bundled) or src/updater.ts (dev).
  const here = dirname(fileURLToPath(import.meta.url));
  // We sit at src/ inside the install tree; root is one level up.
  return join(here, '..');
}

export async function runUpdate(currentVersion: string): Promise<void> {
  process.stderr.write(`${ACCENT}hip${RESET} update: checking latest release...\n`);
  const rel = await fetchJson<GHRelease>(API_LATEST);
  const latest = rel.tag_name.replace(/^v/, '');
  if (latest === currentVersion) {
    process.stderr.write(`already on latest (${currentVersion}). Nothing to do.\n`);
    return;
  }
  process.stderr.write(`current: ${currentVersion}\nlatest:  ${latest}\n`);

  const triple = targetTriple();
  const tarballName = `hip-${latest}-${triple}.tar.gz`;
  const shaName = `${tarballName}.sha256`;
  const tarAsset = rel.assets.find((a) => a.name === tarballName);
  const shaAsset = rel.assets.find((a) => a.name === shaName);
  if (!tarAsset) throw new Error(`no asset ${tarballName} in release ${latest}`);

  process.stderr.write(`downloading ${tarballName}...\n`);
  const tarBuf = await fetchBuffer(tarAsset.browser_download_url);
  if (shaAsset) {
    const expected = (await (await fetch(shaAsset.browser_download_url)).text()).split(/\s+/)[0];
    const actual = sha256(tarBuf);
    if (expected && expected !== actual) {
      throw new Error(`sha256 mismatch: expected ${expected}, got ${actual}`);
    }
    process.stderr.write(`sha256 ok\n`);
  } else {
    process.stderr.write(`(no .sha256 published; skipping integrity check)\n`);
  }

  const tmp = mkdtempSync(join(tmpdir(), 'hip-update-'));
  const tarPath = join(tmp, tarballName);
  await writeFile(tarPath, tarBuf);
  process.stderr.write(`extracting...\n`);
  // Extract into tmp/extracted/. tar -xzf is portable across mac+linux.
  const extracted = join(tmp, 'extracted');
  spawnSync('mkdir', ['-p', extracted]);
  const tarResult = spawnSync('tar', ['-xzf', tarPath, '-C', extracted], {
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  if (tarResult.status !== 0) {
    throw new Error(`tar extract failed: ${tarResult.stderr?.toString() ?? 'unknown'}`);
  }

  // Atomic swap. Find the new install root inside the tarball (usually
  // a single top-level dir like hip-0.0.2/). Move bin/, src/, assets/,
  // package.json over the existing install.
  const root = installRoot();
  const entries = spawnSync('ls', [extracted]).stdout.toString().trim().split('\n');
  const inner = entries.length === 1 ? join(extracted, entries[0]!) : extracted;
  if (!existsSync(inner)) throw new Error(`expected extracted dir at ${inner}`);
  for (const name of ['bin', 'src', 'assets', 'package.json', 'tsconfig.json']) {
    const from = join(inner, name);
    const to = join(root, name);
    if (!existsSync(from)) continue;
    // Backup the previous version next to the install (one revision).
    const backup = `${to}.prev`;
    if (existsSync(to)) {
      if (existsSync(backup)) spawnSync('rm', ['-rf', backup]);
      renameSync(to, backup);
    }
    renameSync(from, to);
  }
  // Make sure bin/hip is executable.
  const binHip = join(root, 'bin', 'hip');
  if (existsSync(binHip)) {
    chmodSync(binHip, 0o755);
  }
  process.stderr.write(
    `${ACCENT}hip${RESET} updated to ${latest}.\n` +
      `previous version backed up alongside the install (look for *.prev).\n` +
      `restart hip to use the new version.\n`,
  );
}

/** Quick sanity helper - print where we'd install to without doing the
 *  download. Useful for debugging "where did `hip` actually go?" */
export function printInstallInfo(): void {
  const root = installRoot();
  let writable = false;
  try {
    const s = statSync(root);
    writable = (s.mode & 0o200) !== 0;
  } catch {
    /* ignore */
  }
  process.stderr.write(
    `install root: ${root}\nwritable:     ${writable ? 'yes' : 'no'}\ntarget:       ${targetTriple()}\n`,
  );
}
