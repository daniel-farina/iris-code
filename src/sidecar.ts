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
  /** Hard cap on a single call. Default 3000ms. */
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
  const timeoutMs = cfg.timeoutMs ?? 3000;
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

/** Auto-detect a usable sidecar at startup. Probes Ollama's /api/tags.
 *  Returns the first model from the preferred list that's installed,
 *  or null if Ollama isn't reachable / no preferred model is present.
 *  Cheap: one HTTP call with a tight 500ms timeout. Failures silent. */
export async function autoDetectSidecar(
  baseUrl = 'http://localhost:11434',
  preferred: readonly string[] = [
    // Smaller / faster models first so we pick the cheapest available.
    'gemma3:1b',
    'llama3.2:1b',
    'gemma4:e2b',
    'gemma4:e4b',
    'qwen3:1.7b',
  ],
): Promise<SidecarConfig | null> {
  const ctrl = new AbortController();
  const t = setTimeout(() => ctrl.abort(), 500);
  try {
    const res = await fetch(`${baseUrl.replace(/\/$/, '')}/api/tags`, { signal: ctrl.signal });
    if (!res.ok) return null;
    const data = (await res.json()) as { models?: { name?: string }[] };
    const installed = new Set((data.models ?? []).map((m) => m.name ?? '').filter(Boolean));
    for (const want of preferred) {
      if (installed.has(want)) return { url: baseUrl, model: want };
    }
    return null;
  } catch {
    return null;
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
