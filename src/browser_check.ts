// Headless-Chrome console-error checker. Launches Chrome with
// --headless=new --remote-debugging-port=0 (lets OS pick a free
// port), parses the assigned port from stderr, attaches via the
// DevTools Protocol over WebSocket, navigates to the URL, captures
// console.error + Runtime.exceptionThrown events for a short window,
// then returns them.
//
// Designed as a verification step after writing UI/game code: the
// model can call it as a tool, and hip can run it automatically at
// the end of --print to surface any "but did it actually load?"
// errors. Without this hip would happily commit a JS file that
// throws ReferenceError on load - the model would never know.
//
// Bun's built-in WebSocket + spawn makes this dep-free.

import { type ChildProcess, spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const CHROME_PATHS = [
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  '/Applications/Chromium.app/Contents/MacOS/Chromium',
  '/usr/bin/google-chrome',
  '/usr/bin/chromium-browser',
  '/usr/bin/chromium',
];

export interface BrowserError {
  source: 'console.error' | 'exception';
  text: string;
  url?: string;
  line?: number;
}

export interface BrowserCheckResult {
  ok: boolean;
  errors: BrowserError[];
  /** Plain stuff that landed in console.log/info - useful for debugging. */
  log: string[];
  /** Set if the check itself failed (Chrome not found, port parse fail, etc). */
  checkError?: string;
  /** Wall-clock time for the whole check. */
  elapsedMs: number;
}

function findChrome(): string | null {
  for (const p of CHROME_PATHS) if (existsSync(p)) return p;
  return null;
}

/** Run a browser-side check of the given URL. Spawns headless Chrome,
 *  navigates, captures console + exceptions for `waitMs`, kills Chrome,
 *  returns the result. When `interact: true` (default), after page load
 *  it simulates a click on every visible button to surface errors that
 *  fire from click handlers (which load-only checks miss).
 *  Never throws - returns {checkError} on failure. */
export async function browserCheck(opts: {
  url: string;
  waitMs?: number;
  interact?: boolean;
}): Promise<BrowserCheckResult> {
  const t0 = performance.now();
  const waitMs = opts.waitMs ?? 3000;
  const interact = opts.interact ?? true;
  const chromePath = findChrome();
  if (!chromePath) {
    return {
      ok: false,
      errors: [],
      log: [],
      checkError: 'Chrome/Chromium not found at standard paths',
      elapsedMs: performance.now() - t0,
    };
  }

  const userDataDir = await mkdtemp(join(tmpdir(), 'hip-browser-check-'));
  let chrome: ChildProcess | undefined;
  try {
    chrome = spawn(
      chromePath,
      [
        '--headless=new',
        '--remote-debugging-port=0',
        '--no-startup-window',
        '--no-first-run',
        '--no-default-browser-check',
        `--user-data-dir=${userDataDir}`,
        // Software-rasterized WebGL via SwiftShader. Without this,
        // headless Chrome can't make a WebGL context and Three.js
        // apps throw at `new WebGLRenderer()` - hiding the real
        // bug the model needs to see further down the page (e.g.
        // ReferenceError in main.js).
        '--use-gl=swiftshader',
        '--enable-unsafe-swiftshader',
      ],
      { stdio: ['ignore', 'pipe', 'pipe'] },
    );
    // Parse the WS endpoint from stderr. Chrome prints:
    //   DevTools listening on ws://127.0.0.1:NNNN/devtools/browser/UUID
    const wsUrl = await readWsEndpoint(chrome, 5000);
    if (!wsUrl) {
      return {
        ok: false,
        errors: [],
        log: [],
        checkError: 'timed out waiting for Chrome DevTools port',
        elapsedMs: performance.now() - t0,
      };
    }
    // Hit /json to find or create a page target.
    const restUrl = wsUrl.replace(/^ws:\/\//, 'http://').replace(/\/devtools\/browser\/.+$/, '');
    const result = await collectErrors(restUrl, opts.url, waitMs, interact);
    return { ...result, elapsedMs: performance.now() - t0 };
  } catch (e) {
    return {
      ok: false,
      errors: [],
      log: [],
      checkError: `browser_check failed: ${(e as Error).message ?? String(e)}`,
      elapsedMs: performance.now() - t0,
    };
  } finally {
    try {
      chrome?.kill('SIGTERM');
    } catch {}
    rm(userDataDir, { recursive: true, force: true }).catch(() => {});
  }
}

function readWsEndpoint(chrome: ChildProcess, timeoutMs: number): Promise<string | null> {
  return new Promise((resolve) => {
    let buf = '';
    let resolved = false;
    const finish = (v: string | null) => {
      if (!resolved) {
        resolved = true;
        resolve(v);
      }
    };
    chrome.stderr?.on('data', (chunk: Buffer | string) => {
      buf += chunk.toString();
      const m = buf.match(/DevTools listening on (ws:\/\/[^\s]+)/);
      if (m?.[1]) finish(m[1]);
    });
    chrome.on('exit', () => finish(null));
    setTimeout(() => finish(null), timeoutMs);
  });
}

interface TargetInfo {
  id: string;
  type: string;
  webSocketDebuggerUrl?: string;
  url?: string;
}

async function collectErrors(
  restUrl: string,
  pageUrl: string,
  waitMs: number,
  interact: boolean,
): Promise<Omit<BrowserCheckResult, 'elapsedMs'>> {
  // Create a new page target so we don't interfere with anything else.
  const createRes = await fetch(`${restUrl}/json/new?${encodeURIComponent(pageUrl)}`, {
    method: 'PUT',
  });
  if (!createRes.ok) {
    return {
      ok: false,
      errors: [],
      log: [],
      checkError: `failed to create page target: HTTP ${createRes.status}`,
    };
  }
  const target = (await createRes.json()) as TargetInfo;
  if (!target.webSocketDebuggerUrl) {
    return {
      ok: false,
      errors: [],
      log: [],
      checkError: 'page target has no webSocketDebuggerUrl',
    };
  }

  const errors: BrowserError[] = [];
  const logs: string[] = [];

  const ws = new WebSocket(target.webSocketDebuggerUrl);
  let msgId = 0;

  await new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('WS open timeout')), 5000);
    ws.addEventListener('open', () => {
      clearTimeout(timer);
      resolve();
    });
    ws.addEventListener('error', () => {
      clearTimeout(timer);
      reject(new Error('WS error'));
    });
  });

  ws.addEventListener('message', (ev: MessageEvent) => {
    const data = typeof ev.data === 'string' ? ev.data : '';
    if (!data) return;
    try {
      const msg = JSON.parse(data) as {
        method?: string;
        params?: {
          type?: string;
          args?: { value?: unknown; description?: unknown }[];
          exceptionDetails?: {
            text?: string;
            url?: string;
            lineNumber?: number;
            exception?: { description?: string };
          };
          stackTrace?: { callFrames?: { url?: string; lineNumber?: number }[] };
        };
      };
      if (msg.method === 'Runtime.consoleAPICalled') {
        const type = msg.params?.type ?? 'log';
        const args = msg.params?.args ?? [];
        const text = args
          .map((a) => (a.value !== undefined ? String(a.value) : String(a.description ?? '')))
          .join(' ');
        if (type === 'error') {
          errors.push({ source: 'console.error', text });
        } else {
          logs.push(`[${type}] ${text}`);
        }
      } else if (msg.method === 'Runtime.exceptionThrown') {
        const ex = msg.params?.exceptionDetails;
        const text =
          ex?.exception?.description ?? ex?.text ?? 'uncaught exception (no description)';
        errors.push({
          source: 'exception',
          text,
          url: ex?.url,
          line: ex?.lineNumber,
        });
      }
    } catch {}
  });

  // Enable the domains we care about.
  ws.send(JSON.stringify({ id: ++msgId, method: 'Runtime.enable' }));
  ws.send(JSON.stringify({ id: ++msgId, method: 'Console.enable' }));
  ws.send(JSON.stringify({ id: ++msgId, method: 'Page.enable' }));

  // Wait for page to settle.
  await new Promise<void>((resolve) => setTimeout(resolve, waitMs));

  // Interaction sweep: click every visible <button>. Catches errors
  // thrown by click handlers (like `selectedTrack is not defined`)
  // that load-only checks miss. Each click is wrapped so an error
  // surfaces via Runtime.exceptionThrown but doesn't stop the sweep.
  if (interact) {
    const clickScript = `
      (async () => {
        const buttons = Array.from(document.querySelectorAll('button'));
        for (const b of buttons) {
          if (b.offsetParent === null) continue; // not visible
          try { b.click(); } catch (e) { console.error('[interact-click]', b.id || b.textContent?.trim()?.slice(0, 40) || '<button>', e?.message ?? e); }
          await new Promise(r => setTimeout(r, 50));
        }
      })();
    `;
    ws.send(
      JSON.stringify({
        id: ++msgId,
        method: 'Runtime.evaluate',
        params: { expression: clickScript, awaitPromise: true, returnByValue: true },
      }),
    );
    // Listen a bit longer for click-triggered errors.
    await new Promise<void>((resolve) => setTimeout(resolve, 1500));
  }

  try {
    ws.close();
  } catch {}
  // Close the page target so Chrome doesn't keep it.
  fetch(`${restUrl}/json/close/${target.id}`).catch(() => {});

  return { ok: errors.length === 0, errors, log: logs };
}

/** Format the result for printing on stderr (used by --browser-check). */
export function formatBrowserCheck(r: BrowserCheckResult): string {
  if (r.checkError) {
    return `[hip] browser-check skipped: ${r.checkError}`;
  }
  if (r.ok) {
    return `[hip] browser-check: OK (${r.elapsedMs.toFixed(0)}ms, ${r.log.length} logs, 0 errors)`;
  }
  const lines = r.errors
    .slice(0, 8)
    .map((e) => `  - ${e.source}: ${e.text}${e.url ? ` (${e.url}:${e.line ?? '?'})` : ''}`);
  return `[hip] browser-check: ${r.errors.length} error${r.errors.length === 1 ? '' : 's'}\n${lines.join('\n')}`;
}
