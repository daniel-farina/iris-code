// Per-session running counters for the TUI status line + /stats command.
// Reset on /new. Updated by each round of the agent loop.

export interface PerToolStats {
  calls: number;
  errors: number;
  totalMs: number;
}

export interface UsageStats {
  rounds: number;
  toolCalls: number;
  promptTokens: number;
  completionTokens: number;
  totalMs: number;
  /** Per-tool breakdown: count, errors, accumulated ms. */
  perTool: Record<string, PerToolStats>;
}

export function emptyStats(): UsageStats {
  return {
    rounds: 0,
    toolCalls: 0,
    promptTokens: 0,
    completionTokens: 0,
    totalMs: 0,
    perTool: {},
  };
}

export function formatStats(s: UsageStats): string {
  const ttok = s.promptTokens + s.completionTokens;
  return [
    `rounds=${s.rounds}`,
    `tool_calls=${s.toolCalls}`,
    `tokens=${ttok}` + (ttok > 0 ? ` (in=${s.promptTokens} out=${s.completionTokens})` : ''),
    `ms=${s.totalMs.toFixed(0)}`,
  ].join(' · ');
}

export function formatPerTool(s: UsageStats): string {
  const entries = Object.entries(s.perTool);
  if (entries.length === 0) return '[no tool calls this session]';
  // Sort by call count descending so the most-used tool floats to top.
  entries.sort((a, b) => b[1].calls - a[1].calls);
  const rows = entries.map(([name, st]) => {
    const avg = st.calls > 0 ? (st.totalMs / st.calls).toFixed(0) : '0';
    const err = st.errors > 0 ? ` err=${st.errors}` : '';
    return `  ${name.padEnd(10)} calls=${String(st.calls).padStart(3)}  avg=${avg.padStart(4)}ms  total=${st.totalMs.toFixed(0)}ms${err}`;
  });
  return `per-tool usage (top first):\n${rows.join('\n')}`;
}
