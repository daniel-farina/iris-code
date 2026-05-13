// Bash tool execution tests. The bash tool is the riskiest in the
// registry (arbitrary shell), so the timeout-kill path matters - if it
// regressed, a runaway command could hang the agent loop forever.

import { describe, it, expect } from 'vitest';
import { bashTool } from '../src/tools/bash.js';

describe('bashTool', () => {
  it('runs a simple command and reports exit 0', async () => {
    const out = await bashTool.run({ command: 'echo hello' });
    expect(out).toContain('hello');
    expect(out).toContain('[exit 0]');
  });

  it('captures both stdout and stderr', async () => {
    const out = await bashTool.run({
      command: 'echo to_out; echo to_err 1>&2',
    });
    expect(out).toContain('to_out');
    expect(out).toContain('to_err');
  });

  it('reports nonzero exit code', async () => {
    const out = await bashTool.run({ command: 'sh -c "exit 7"' });
    expect(out).toContain('[exit 7]');
  });

  it('refuses missing command', async () => {
    await expect(bashTool.run({})).rejects.toThrow(/missing command/);
  });

  it('kills process after timeout_s', async () => {
    const start = Date.now();
    const out = await bashTool.run({ command: 'sleep 5', timeout_s: 1 });
    const elapsed = Date.now() - start;
    // Should kill at ~1s, not the full 5s sleep.
    expect(elapsed).toBeLessThan(2000);
    expect(out).toContain('[killed: timeout after 1s]');
  });

  it('accepts timeout_s as a string (MTPLX qwen XML quirk)', async () => {
    // argU64 helper coerces numeric strings; verify the path works for
    // the bash tool too (not just read/etc).
    const out = await bashTool.run({ command: 'echo ok', timeout_s: '5' });
    expect(out).toContain('ok');
    expect(out).toContain('[exit 0]');
  });

  it('caps timeout_s at 600', async () => {
    // Pass an absurd timeout; the tool should silently clamp to 600 but
    // still let `echo` run instantly. We can only check that this
    // doesn't somehow break (it can't actually wait 600+ seconds).
    const out = await bashTool.run({ command: 'echo ok', timeout_s: 99999 });
    expect(out).toContain('[exit 0]');
  });

  it('truncates output above 16KB so a runaway command does not blow context', async () => {
    // Emit ~20KB - generate 20480 'x' chars via yes/head.
    const out = await bashTool.run({
      command: 'yes x | head -c 20480',
    });
    expect(out).toContain('[... truncated');
    expect(out).toContain('bytes ...]');
    // Cap is 16KB; with truncation header + status, total should stay under ~17KB.
    expect(out.length).toBeLessThan(18000);
    expect(out).toContain('[exit 0]');
  });
});
