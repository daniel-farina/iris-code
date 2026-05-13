// `hip --setup`: interactive one-time configuration flow. Detects the
// running MTPLX, shows what differs from hippo-code's recommended
// settings, and offers to restart MTPLX with those settings layered
// into its env. Mirrors how `gh auth setup` or `claude /login` walk a
// user through a one-shot config change.

import { createInterface } from 'node:readline/promises';
import {
  RECOMMENDED_ENV,
  diffConfig,
  formatDiff,
  getMtplxStatus,
  startMtplx,
  stopMtplx,
  waitForMtplx,
} from './mtplx_manager.js';

const ACCENT = '\x1b[38;2;175;146;204m';
const GOOD = '\x1b[38;5;65m';
const WARN = '\x1b[38;5;178m';
const DIM = '\x1b[2m';
const RESET = '\x1b[0m';

export async function runSetup(baseUrl: string): Promise<void> {
  process.stdout.write(`\n${ACCENT}hip --setup${RESET}  ${DIM}MTPLX config check${RESET}\n\n`);

  const status = await getMtplxStatus(baseUrl);
  if (!status.running) {
    process.stdout.write(
      `${WARN}MTPLX is not running at ${baseUrl}.${RESET}\n` +
        `Start MTPLX first (it should listen on ${baseUrl}), then re-run \`hip --setup\`.\n`,
    );
    return;
  }

  const diff = diffConfig(status.env, RECOMMENDED_ENV);
  process.stdout.write(`${formatDiff(status, diff)}\n\n`);

  if (diff.missing.length === 0) {
    process.stdout.write(
      `${GOOD}MTPLX is already configured for hippo-code. Nothing to do.${RESET}\n`,
    );
    return;
  }

  // Build the new env we'd launch with: keep MTPLX's existing env vars
  // (MLX_CODE_*, MTPLX_*) untouched and overlay our recommended ones.
  const desiredEnv: Record<string, string> = { ...(status.env ?? {}), ...RECOMMENDED_ENV };
  const cmdline = status.cmdline ?? [];
  if (cmdline.length === 0) {
    process.stdout.write(
      `${WARN}Could not read MTPLX's command line. Restart it manually with the env vars above.${RESET}\n`,
    );
    return;
  }

  process.stdout.write(
    `${DIM}Restart plan:${RESET}\n` +
      `  1. SIGTERM pid ${status.pid}\n` +
      `  2. Wait for port ${urlPort(baseUrl)} to free\n` +
      `  3. Relaunch with the same argv + the recommended env vars\n` +
      `  4. Wait for /v1/models to respond again\n` +
      `${DIM}Logs from the new MTPLX will append to /tmp/mtplx-hip-launch.log${RESET}\n\n`,
  );

  const rl = createInterface({ input: process.stdin, output: process.stdout });
  let answer = '';
  try {
    answer = (await rl.question(`Apply + restart MTPLX now? [Y/n] `)).trim().toLowerCase();
  } finally {
    rl.close();
  }
  if (answer && !['y', 'yes', ''].includes(answer)) {
    process.stdout.write(`${DIM}cancelled${RESET}\n`);
    return;
  }

  process.stdout.write(`\n${DIM}stopping MTPLX (pid ${status.pid})...${RESET}\n`);
  await stopMtplx(status.pid ?? 0, urlPort(baseUrl));
  process.stdout.write(`${DIM}relaunching with new env...${RESET}\n`);
  const newPid = startMtplx(cmdline, desiredEnv);
  process.stdout.write(`${DIM}waiting for MTPLX to boot (pid ${newPid})...${RESET}\n`);
  try {
    await waitForMtplx(baseUrl, 90_000);
  } catch (e) {
    process.stdout.write(
      `${WARN}MTPLX didn't come up cleanly: ${(e as Error).message}\n` +
        `Check /tmp/mtplx-hip-launch.log for clues.${RESET}\n`,
    );
    return;
  }

  // Re-probe and confirm.
  const after = await getMtplxStatus(baseUrl);
  const afterDiff = diffConfig(after.env, RECOMMENDED_ENV);
  if (afterDiff.missing.length === 0) {
    process.stdout.write(
      `\n${GOOD}MTPLX restarted with hippo-code's recommended settings.${RESET}\n` +
        `${DIM}new pid: ${after.pid ?? '?'}${RESET}\n`,
    );
  } else {
    process.stdout.write(
      `\n${WARN}MTPLX restarted, but some settings still don't match. ` +
        `It may be reading env from a parent script that overrides ours.${RESET}\n` +
        `${formatDiff(after, afterDiff)}\n`,
    );
  }
}

function urlPort(baseUrl: string): string {
  const m = baseUrl.match(/:(\d+)(?:\/|$)/);
  return m?.[1] ?? '8088';
}
