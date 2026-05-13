// Render a tool call as a short, scannable one-liner instead of dumping
// raw JSON. e.g. `read src/main.js` instead of `read({"path":"…"})`.
// args is already a JSON string (capped upstream).

export function compactToolLabel(name: string, args: string): string {
  let obj: Record<string, unknown> = {};
  try {
    obj = JSON.parse(args);
  } catch {
    return `${name} ${args}`;
  }
  const s = (k: string): string | undefined =>
    typeof obj[k] === 'string' ? (obj[k] as string) : undefined;
  switch (name) {
    case 'read':
    case 'peek':
    case 'list':
    case 'tree':
    case 'glob':
    case 'diff':
      return `${name} ${s('path') ?? s('pattern') ?? ''}`.trim();
    case 'grep':
    case 'search': {
      const pat = s('pattern') ?? s('query') ?? '';
      const p = s('path') ?? '';
      return `${name} "${pat}"${p ? ` in ${p}` : ''}`;
    }
    case 'edit':
      return `edit ${s('path') ?? ''}`;
    case 'bash':
      return `bash $ ${(s('command') ?? '').slice(0, 80)}`;
    case 'peek_log': {
      const failed = s('failed');
      const n = typeof obj['n'] === 'number' ? `n=${obj['n']}` : '';
      const f = failed ? `failed=${failed}` : '';
      return `peek_log ${[f, n].filter(Boolean).join(' ')}`.trim();
    }
    default:
      return `${name} ${args.slice(0, 80)}`;
  }
}
