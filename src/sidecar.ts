// Small-model sidecar. Runs in PARALLEL with the main MTPLX stream
// to produce cheap per-round summaries. Powers two wins:
//
//   1. Running summary: after each agent round we fire-and-forget a
//      summarize call to the sidecar. The result is appended to a
//      `runningSummary: string[]` so compactConv can splice it in
//      INSTEAD of calling the main model to summarize - free compact.
//   2. Future intent / stuck-detection - same client, different prompt.
//
// Designed against Ollama's /api/chat with `think: false` (so reasoning
// models like Gemma 4 / Qwen 3.6 skip their <think> phase and answer
// immediately). 360ms warm on gemma4:e2b for one-sentence summaries.
// Works against any OpenAI-compatible endpoint that honors the flag.
//
// Errors are absorbed: a flaky sidecar must never break the main loop.

export interface SidecarConfig {
  /** Ollama-style base URL, e.g. http://localhost:11434 (NO trailing /v1). */
  url: string;
  /** Model id, e.g. gemma4:e2b. */
  model: string;
  /** Hard cap on a single call. Default 10000ms (covers Ollama cold-load
   *  on small models; warm calls land in 300-700ms). */
  timeoutMs?: number;
}

const DEFAULT_SUMMARIZE_SYSTEM =
  'You produce ONE-SENTENCE note-to-self summaries of a coding-assistant turn. Output ONLY the sentence, no preamble. Mention concrete file paths, function names, exports, and decisions when present. Skip filler like "the user requested" or "I created".';

/** Call the sidecar to produce a one-sentence summary of an exchange.
 *  Returns the trimmed sentence, or null on any error/timeout. Never
 *  throws - caller can call without try/catch. */
export async function summarizeExchange(
  cfg: SidecarConfig,
  exchangeText: string,
  signal?: AbortSignal,
): Promise<string | null> {
  const timeoutMs = cfg.timeoutMs ?? 10000;
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), timeoutMs);
  const composite = signal ? mergeSignals(signal, ctrl.signal) : ctrl.signal;
  try {
    const res = await fetch(`${cfg.url.replace(/\/$/, '')}/api/chat`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      signal: composite,
      body: JSON.stringify({
        model: cfg.model,
        messages: [
          { role: 'system', content: DEFAULT_SUMMARIZE_SYSTEM },
          { role: 'user', content: exchangeText.slice(0, 8000) },
        ],
        think: false,
        options: { num_predict: 80, temperature: 0 },
        stream: false,
      }),
    });
    if (!res.ok) return null;
    const data = (await res.json()) as { message?: { content?: string } };
    const text = data.message?.content?.trim();
    if (!text) return null;
    // Strip surrounding quotes/markdown. Gemma sometimes wraps.
    return text.replace(/^["'`]+|["'`]+$/g, '').trim();
  } catch {
    return null;
  } finally {
    clearTimeout(timer);
  }
}

/** Distinguishes "Ollama not installed/reachable" from "Ollama up but
 *  no preferred small model installed" - lets the UI offer a precise
 *  hint (install ollama vs `ollama pull <model>`). */
export type SidecarStatus =
  | { kind: 'ok'; config: SidecarConfig }
  | { kind: 'no-ollama'; reason: string }
  | { kind: 'no-model'; preferred: readonly string[]; installed: string[] };

/** Auto-detect a usable sidecar at startup. Probes Ollama's /api/tags.
 *  Returns the first model from the preferred list that's installed,
 *  or null if Ollama isn't reachable / no preferred model is present.
 *  Cheap: one HTTP call with a tight 500ms timeout. Failures silent. */
export const PREFERRED_SIDECAR_MODELS: readonly string[] = [
  // Order by quality at the per-summary level (based on live testing
  // 2026-05): Gemma 4 E4B beats E2B by a clear margin for one-line
  // summaries - it preserves exported function names, file paths,
  // and specific numbers verbatim. 2x the latency (~600ms vs ~300ms)
  // but still well within the 1-17s MTPLX postcommit overlap window.
  // Smaller tier comes after as a fallback when E4B isn't installed.
  'gemma4:e4b',
  'gemma4:e2b',
  'gemma3:1b',
  'llama3.2:1b',
  'qwen3:1.7b',
] as const;

export async function autoDetectSidecar(
  baseUrl = 'http://localhost:11434',
  preferred: readonly string[] = PREFERRED_SIDECAR_MODELS,
): Promise<SidecarConfig | null> {
  const r = await probeSidecar(baseUrl, preferred);
  return r.kind === 'ok' ? r.config : null;
}

/** Like autoDetectSidecar but returns a precise status so the UI can
 *  offer install hints ("brew install ollama" vs "ollama pull X"). */
export async function probeSidecar(
  baseUrl = 'http://localhost:11434',
  preferred: readonly string[] = PREFERRED_SIDECAR_MODELS,
): Promise<SidecarStatus> {
  const ctrl = new AbortController();
  const t = setTimeout(() => ctrl.abort(), 500);
  try {
    const res = await fetch(`${baseUrl.replace(/\/$/, '')}/api/tags`, { signal: ctrl.signal });
    if (!res.ok) return { kind: 'no-ollama', reason: `HTTP ${res.status}` };
    const data = (await res.json()) as { models?: { name?: string }[] };
    const installed = (data.models ?? []).map((m) => m.name ?? '').filter(Boolean);
    const installedSet = new Set(installed);
    for (const want of preferred) {
      if (installedSet.has(want)) return { kind: 'ok', config: { url: baseUrl, model: want } };
    }
    return { kind: 'no-model', preferred, installed };
  } catch (e) {
    return { kind: 'no-ollama', reason: (e as Error).message ?? 'unreachable' };
  } finally {
    clearTimeout(t);
  }
}

/** Wire two AbortSignals together: abort the composite when EITHER fires. */
function mergeSignals(a: AbortSignal, b: AbortSignal): AbortSignal {
  const ctrl = new AbortController();
  const onA = () => ctrl.abort();
  const onB = () => ctrl.abort();
  if (a.aborted || b.aborted) ctrl.abort();
  else {
    a.addEventListener('abort', onA, { once: true });
    b.addEventListener('abort', onB, { once: true });
  }
  return ctrl.signal;
}

/** Convenience: build the exchange text fed to the sidecar from the
 *  conv messages added in the latest round. We deliberately limit to
 *  the LAST 2-3 messages (user/assistant + tool results), since the
 *  prior conv is already covered by earlier summary lines. */
export function buildExchangeText(messages: { role: string; content: unknown }[]): string {
  return messages
    .map((m) => {
      const content = typeof m.content === 'string' ? m.content : JSON.stringify(m.content);
      return `<${m.role}>${content.slice(0, 2000)}</${m.role}>`;
    })
    .join('\n');
}
