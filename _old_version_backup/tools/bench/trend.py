#!/usr/bin/env python3
"""Print a perf trend over ~/.mlx-code/logs/runs.jsonl.

Shows per-mode success rate, decode-rate distribution (p10/p50/p90),
context-bench scaling (if context-perf.jsonl exists), and any error rows.
"""
import json, os, statistics, time
from collections import defaultdict

RUNS = os.path.expanduser('~/.mlx-code/logs/runs.jsonl')
CTX = os.path.expanduser('~/.mlx-code/logs/context-perf.jsonl')
SMOKE = os.path.expanduser('~/.mlx-code/logs/smoke.jsonl')


def loadj(path):
    out = []
    if not os.path.exists(path): return out
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line: continue
            try: out.append(json.loads(line))
            except: pass
    return out


def section(title): print(f"\n\033[1m── {title} ──\033[0m")


runs = loadj(RUNS)
ctx = loadj(CTX)
sm = loadj(SMOKE)

section('runs.jsonl summary')
by_mode = defaultdict(list)
for r in runs:
    by_mode[r.get('mode', '?')].append(r)

print(f"{'mode':<14}  {'count':>5}  {'success%':>8}  {'tok/s p10/p50/p90':>22}  {'ttft_s p50':>10}")
for m, rs in sorted(by_mode.items()):
    n = len(rs)
    s = sum(1 for r in rs if r.get('success'))
    tps_vals = [r.get('decode_tok_per_s') for r in rs if r.get('decode_tok_per_s') is not None and 0 < r.get('decode_tok_per_s', 0) < 200]
    ttft_vals = [r.get('ttft_ms', 0)/1000.0 for r in rs if r.get('ttft_ms') is not None]
    if tps_vals:
        tps_vals.sort()
        p10 = tps_vals[len(tps_vals)//10]
        p50 = statistics.median(tps_vals)
        p90 = tps_vals[max(0, int(len(tps_vals)*0.9)-1)]
        tps_disp = f"{p10:>5.1f}/{p50:>5.1f}/{p90:>5.1f}"
    else:
        tps_disp = '-'
    ttft_disp = f"{statistics.median(ttft_vals):.1f}" if ttft_vals else '-'
    print(f"{m:<14}  {n:>5}  {100*s/n if n else 0:>7.0f}%  {tps_disp:>22}  {ttft_disp:>10}")

if ctx:
    # Group by session so each bench run is visible as its own column.
    sessions = sorted({r.get('session_id', '') for r in ctx if r.get('session_id') and not r.get('error')})
    last_sessions = sessions[-3:] if len(sessions) > 3 else sessions
    section(f'context-perf.jsonl scaling (last {len(last_sessions)} session(s))')
    if last_sessions:
        print(f"{'target':>7}  {'prompt':>7}  {'ttft_s':>7}  {'prefill_t/s':>11}  {'decode_t/s':>10}  session")
        for sid in last_sessions:
            seen = set()
            for r in ctx:
                if r.get('error'): continue
                if r.get('session_id') != sid: continue
                target = r.get('target_tokens')
                if target in seen: continue
                seen.add(target)
                ttft = r.get('ttft_s')
                ttft_disp = '-' if ttft is None else f"{ttft:.1f}"
                short_sid = sid if len(sid) <= 22 else sid[:19] + '...'
                print(f"{target:>7}  {r.get('prompt_tokens', 0):>7}  {ttft_disp:>7}  {r.get('prefill_tok_per_s', 0):>10.0f}  {r.get('decode_tok_per_s', 0):>9.1f}  {short_sid}")
            print()

if sm:
    section('smoke history')
    total_pass = sum(r.get('pass', 0) for r in sm)
    total_fail = sum(r.get('fail', 0) for r in sm)
    print(f"{len(sm)} smoke runs, total pass={total_pass}, fail={total_fail}")
    if total_fail:
        for r in sm:
            for res in r.get('results', []):
                if res.get('status') == 'fail':
                    t = time.strftime('%H:%M:%S', time.localtime(r['ts_unix']))
                    print(f"  {t}  FAIL  {res.get('file')}  -- {res.get('issues','')}")

def sparkline(values):
    """Tiny inline sparkline for a numeric series. Returns Unicode block chars."""
    if not values: return ''
    blocks = ' ▁▂▃▄▅▆▇█'
    lo = min(values); hi = max(values)
    if hi == lo:
        return blocks[4] * len(values)
    return ''.join(blocks[min(8, max(0, int((v - lo) / (hi - lo) * 8)))] for v in values)


def fmt_age(seconds):
    if seconds < 60: return f"{int(seconds)}s ago"
    if seconds < 3600: return f"{int(seconds/60)}m ago"
    if seconds < 86400: return f"{int(seconds/3600)}h ago"
    return f"{int(seconds/86400)}d ago"


# Recent agent runs: last N rows, with a per-row tok/s mini-bar.
recent = [r for r in runs if r.get('decode_tok_per_s') is not None and 0 < r.get('decode_tok_per_s', 0) < 200]
recent_n = min(20, len(recent))
if recent_n:
    section(f'last {recent_n} runs (tok/s sparkline)')
    tail = recent[-recent_n:]
    tps_series = [float(r.get('decode_tok_per_s') or 0) for r in tail]
    print(f"  {sparkline(tps_series)}  range={min(tps_series):.0f}-{max(tps_series):.0f} t/s")
    now = time.time()
    print()
    print(f"  {'when':>10}  {'mode':<14}  {'ms':>7}  {'tps':>5}  prompt")
    for r in tail[-10:]:  # detailed table for the last 10 only
        ts = r.get('ts_unix', 0)
        age = fmt_age(now - ts) if ts else '?'
        snippet = (r.get('prompt_first_120_chars') or '')[:50]
        tps = r.get('decode_tok_per_s')
        tps_disp = f"{tps:.0f}" if tps else '-'
        print(f"  {age:>10}  {r.get('mode', '?'):<14}  {r.get('total_ms', 0):>7}  {tps_disp:>5}  \"{snippet}\"")


errs = [r for r in runs if not r.get('success')]
if errs:
    section('errors')
    for r in errs:
        t = time.strftime('%H:%M:%S', time.localtime(r['ts_unix']))
        print(f"  {t}  {r.get('mode'):<14}  {r.get('error')}")
else:
    section('errors')
    print("(none)")

print()
