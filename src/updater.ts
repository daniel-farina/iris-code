// `hip --update`: hit the GitHub releases API, find the latest release,
// and run the bundled install.sh against that version so the on-disk
// install at ~/.local/hippo-code/<VERSION>/ + the ~/.local/bin/hip
// symlink match what a fresh installer-curl would produce. After
// install, mirror `hip --setup`'s drift check so we leave the user
// with a one-line answer to "is MTPLX configured correctly now?".
//
// We deliberately reuse install.sh (fetched from raw.githubusercontent
// at the release tag) rather than replicating its tarball/sha256/
// symlink dance here. That keeps installer logic in ONE place: the
// shell script. install.sh already knows about HIPPO_INSTALL_DIR,
// PREFIX, bun-presence checks, sha256 verification, etc., and we'd
// just be drifting if we re-implemented it in TS.

import { spawn, spawnSync } from 'node:child_process';
import { existsSync, statSync, writeFileSync } from 'node:fs';
import { arch as nodeArch, platform as nodePlatform, tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  RECOMMENDED_ENV,
  diffConfig,
  formatDiff,
  getMtplxStatus,
} from './mtplx_manager.js';

const REPO = 'daniel-farina/hippo-code';
const API_LATEST = `https://api.github.com/repos/${REPO}/releases/latest`;
const ACCENT = '\x1b[38;2;175;146;204m';
const DIM = '\x1b[2m';
const RESET = '\x1b[0m';

interface GHRelease {
  tag_name: string;
  name?: string;
  assets: { name: string; browser_download_url: string }[];
}

/** Map node's platform+arch into the tarball naming convention used by
 *  the release workflow. Keep in sync with .github/workflows/release.yml
 *  and install.sh. */
function targetTriple(): string {
  const plat = nodePlatform();
  const arch = nodeArch();
  // MLX (MTPLX's runtime) is Apple-Silicon-only, so we only ship for
  // darwin-arm64. Other platforms get a clear error.
  if (plat === 'darwin' && arch === 'arm64') return 'darwin-arm64';
  throw new Error(
    `unsupported platform: ${plat}/${arch}. hip only ships for darwin-arm64 ` +
      `(MLX/MTPLX is Apple-Silicon-only). Run from source instead: ` +
      `'git clone https://github.com/daniel-farina/hippo-code && cd hippo-code && bun install && ./bin/hip'.`,
  );
}

async function fetchJson<T>(url: string): Promise<T> {
  const res = await fetch(url, {
    headers: { accept: 'application/vnd.github+json', 'user-agent': 'hip-updater' },
  });
  if (!res.ok) throw new Error(`GET ${url}: HTTP ${res.status}`);
  return (await res.json()) as T;
}

async function fetchText(url: string): Promise<string> {
  const res = await fetch(url, { headers: { 'user-agent': 'hip-updater' } });
  if (!res.ok) throw new Error(`GET ${url}: HTTP ${res.status}`);
  return await res.text();
}

/** Find this script's install root (where bin/ + src/ live). When
 *  running from a source checkout this is the repo root; for a tarball
 *  install it's wherever the user extracted the tarball
 *  (~/.local/hippo-code/<VERSION>/). */
function installRoot(): string {
  // updater.ts → src/updater.js (bundled) or src/updater.ts (dev).
  const here = dirname(fileURLToPath(import.meta.url));
  // We sit at src/ inside the install tree; root is one level up.
  return join(here, '..');
}

/** Pull install.sh from the release tag's raw.githubusercontent. We
 *  pin to the release tag (not main) so a hip --update at v0.5.0 runs
 *  install.sh as it shipped with v0.5.0, never a future install.sh
 *  that might have incompatible expectations. */
async function fetchInstallScript(versionTag: string): Promise<string> {
  // Tag form is "vX.Y.Z" from GH; raw URL wants the tag verbatim.
  const url = `https://raw.githubusercontent.com/${REPO}/${versionTag}/install.sh`;
  return await fetchText(url);
}

/** Run a shell script through `sh` with HIPPO_VERSION pinned to the
 *  target tag, streaming its stdout/stderr to ours. */
function runShellScript(script: string, env: Record<string, string>): Promise<number> {
  return new Promise((resolve, reject) => {
    const dir = tmpdir();
    const scriptPath = join(dir, `hip-install-${Date.now()}.sh`);
    try {
      writeFileSync(scriptPath, script, { mode: 0o700 });
    } catch (e) {
      reject(e);
      return;
    }
    const child = spawn('sh', [scriptPath], {
      stdio: ['ignore', 'inherit', 'inherit'],
      env: { ...process.env, ...env },
    });
    child.on('error', reject);
    child.on('close', (code) => resolve(code ?? 1));
  });
}

export async function runUpdate(currentVersion: string): Promise<void> {
  // 1. Resolve latest from GitHub API. Plain fetch, no curl.
  process.stderr.write(`${ACCENT}[hip]${RESET} checking latest release on GitHub...\n`);
  let rel: GHRelease;
  try {
    rel = await fetchJson<GHRelease>(API_LATEST);
  } catch (e) {
    process.stderr.write(`[hip] failed to query GitHub: ${(e as Error).message}\n`);
    process.exit(1);
    return;
  }
  const latestTag = rel.tag_name; // e.g. "v0.5.0"
  const latest = latestTag.replace(/^v/, '');
  const current = currentVersion.replace(/^v/, '');

  if (latest === current) {
    process.stderr.write(`[hip] already on v${current} (latest)\n`);
    return;
  }

  process.stderr.write(`[hip] v${latest} is the latest (currently on v${current}); downloading...\n`);

  // 2. Sanity-check that the tarball exists in the release so we fail
  //    fast with a clear message instead of letting install.sh choke.
  const triple = targetTriple();
  const tarballName = `hip-${latest}-${triple}.tar.gz`;
  const hasAsset = rel.assets.some((a) => a.name === tarballName);
  if (!hasAsset) {
    process.stderr.write(
      `[hip] release ${latestTag} is missing asset ${tarballName}; cannot update.\n`,
    );
    process.exit(1);
    return;
  }

  // 3. Fetch install.sh from the release tag and run it. Pin HIPPO_VERSION
  //    so it doesn't re-query the API. Pass-through any user overrides
  //    they set on the shell (HIPPO_INSTALL_DIR, PREFIX).
  let script: string;
  try {
    script = await fetchInstallScript(latestTag);
  } catch (e) {
    process.stderr.write(`[hip] failed to fetch install.sh: ${(e as Error).message}\n`);
    process.exit(1);
    return;
  }
  const env: Record<string, string> = { HIPPO_VERSION: latestTag };
  if (process.env['HIPPO_INSTALL_DIR']) env['HIPPO_INSTALL_DIR'] = process.env['HIPPO_INSTALL_DIR'];
  if (process.env['PREFIX']) env['PREFIX'] = process.env['PREFIX'];

  let code: number;
  try {
    code = await runShellScript(script, env);
  } catch (e) {
    process.stderr.write(`[hip] install.sh failed to launch: ${(e as Error).message}\n`);
    process.exit(1);
    return;
  }
  if (code !== 0) {
    process.stderr.write(`[hip] install.sh exited with code ${code}\n`);
    process.exit(1);
    return;
  }

  process.stderr.write(
    `${ACCENT}[hip]${RESET} updated to v${latest}. ` +
      `Re-run \`hip\` to pick up the new binary.\n`,
  );

  // 4. Verify MTPLX config: mirror --setup's drift check so the user
  //    sees, in one shot, whether the model server they talk to is
  //    still configured the way hip wants. We DON'T apply changes
  //    here (--update is install-only; --setup-apply is the lever
  //    for restart).
  await verifyMtplx();
}

/** Mirror of --setup's first pass: probe MTPLX, diff env, print. No
 *  prompts, no restart. */
async function verifyMtplx(): Promise<void> {
  // Resolve the same default MTPLX URL hip uses elsewhere. We import
  // lazily so a stripped-down `hip --update` doesn't pay the config-
  // module load cost on the happy "already on latest" path.
  const { DEFAULT_MTPLX_URL } = await import('./config.js');
  const baseUrl = process.env['MLX_CODE_URL'] ?? DEFAULT_MTPLX_URL;

  process.stderr.write(`\n${ACCENT}[hip]${RESET} verifying MTPLX config at ${baseUrl}...\n`);
  let status;
  try {
    status = await getMtplxStatus(baseUrl);
  } catch (e) {
    process.stderr.write(`[hip] could not probe MTPLX: ${(e as Error).message}\n`);
    return;
  }
  if (!status.running) {
    process.stderr.write(
      `${DIM}[hip] MTPLX is not running at ${baseUrl}; skipping config diff. ` +
        `Start MTPLX and re-run \`hip --setup\` to verify.${RESET}\n`,
    );
    return;
  }
  const diff = diffConfig(status.env, RECOMMENDED_ENV);
  process.stderr.write(`${formatDiff(status, diff)}\n`);
  if (diff.missing.length === 0) {
    process.stderr.write(`${DIM}[hip] MTPLX is configured for hippo-code.${RESET}\n`);
  } else {
    process.stderr.write(
      `${DIM}[hip] run \`hip --setup\` to apply the recommended env.${RESET}\n`,
    );
  }
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
  let target = 'unknown';
  try {
    target = targetTriple();
  } catch (e) {
    target = `(unsupported: ${(e as Error).message.split('.')[0]})`;
  }
  process.stderr.write(
    `install root: ${root}\nwritable:     ${writable ? 'yes' : 'no'}\ntarget:       ${target}\n` +
      `bin symlink:  ${process.env['HOME'] ?? '~'}/.local/bin/hip\n`,
  );
  // Hint about whether the install root looks like a release install
  // tree vs a dev checkout.
  const looksLikeRelease =
    /\.local\/hippo-code\/[0-9]/.test(root) || existsSync(join(root, '.release-marker'));
  process.stderr.write(`mode:         ${looksLikeRelease ? 'release tree' : 'source / dev'}\n`);
}
