#!/usr/bin/env python3
"""
chat-monitor: live view of MTPLX prefix-cache behaviour for hip chats.

Polls http://127.0.0.1:8088/metrics every second, dedupes on the
combination of session_id + request_start, and prints a colorized
line whenever a new run completes. The point is to make cache hits
vs cold-prefills (and *why* they miss) visible in real time, so we
can stop guessing whether the kv-cache is being reused across hip
invocations.

Run:
    python3 ~/code-2/mlx-code/scripts/chat-monitor.py
or:
    python3 ~/code-2/mlx-code/scripts/chat-monitor.py --once
"""
from __future__ import annotations

import argparse
import json
import sys
import time
import urllib.error
import urllib.request
from collections import defaultdict, deque
from typing import Any

GREEN = "\033[32m"
YELLOW = "\033[33m"
RED = "\033[31m"
DIM = "\033[2m"
BOLD = "\033[1m"
CYAN = "\033[36m"
MAGENTA = "\033[35m"
RESET = "\033[0m"

DEFAULT_URL = "http://127.0.0.1:8088/metrics"


def fetch(url: str) -> dict[str, Any] | None:
    try:
        with urllib.request.urlopen(url, timeout=2) as r:
            return json.load(r)
    except (urllib.error.URLError, TimeoutError, json.JSONDecodeError):
        return None


def classify(latest: dict[str, Any]) -> tuple[str, str]:
    """Return (label, color) describing cache health for this run."""
    hit = latest.get("session_cache_hit")
    cached = latest.get("cached_tokens") or 0
    prompt_tok = latest.get("prompt_tokens") or 0
    if hit and cached > 0:
        # Fraction of prompt that hit the cache.
        ratio = cached / max(prompt_tok, 1)
        if ratio > 0.9:
            return f"WARM ({ratio*100:.0f}%)", GREEN
        if ratio > 0.5:
            return f"PARTIAL ({ratio*100:.0f}%)", YELLOW
        return f"WEAK ({ratio*100:.0f}%)", YELLOW
    return "COLD (0%)", RED


def fmt(latest: dict[str, Any]) -> str:
    sid = (latest.get("session_id") or "?")[:38]
    prompt_tok = latest.get("prompt_tokens") or 0
    cached = latest.get("cached_tokens") or 0
    new_prefill = latest.get("new_prefill_tokens") or 0
    ttft = latest.get("ttft_s") or 0
    decode_tps = latest.get("decode_tok_s") or 0
    completion = latest.get("completion_tokens") or 0
    restore = latest.get("session_restore_mode") or "?"
    miss_reason = latest.get("cache_miss_reason") or ""
    label, color = classify(latest)
    ttft_color = GREEN if ttft < 2 else (YELLOW if ttft < 8 else RED)
    parts = [
        f"{color}{BOLD}{label:<16}{RESET}",
        f"sess={CYAN}{sid:<40}{RESET}",
        f"prompt={prompt_tok:>6} tok",
        f"cached={cached:>6}",
        f"prefill={new_prefill:>6}",
        f"{ttft_color}ttft={ttft:>5.2f}s{RESET}",
        f"decode={decode_tps:>5.1f} tok/s",
        f"out={completion:>5}",
        f"{DIM}restore={restore}{RESET}",
    ]
    line = " | ".join(parts)
    if miss_reason and not latest.get("session_cache_hit"):
        line += f"  {DIM}miss={miss_reason}{RESET}"
    return line


def summarize_session(sid: str, runs: list[dict[str, Any]]) -> str:
    """Per-session trend: was the first run cold and follow-ups warm?"""
    if not runs:
        return ""
    ttfts = [r.get("ttft_s") or 0 for r in runs]
    hits = sum(1 for r in runs if r.get("session_cache_hit"))
    return (
        f"{DIM}↳ {sid[:38]}: {len(runs)} runs, {hits} warm, "
        f"ttft min/max {min(ttfts):.2f}/{max(ttfts):.2f}s{RESET}"
    )


def run(url: str, interval: float, once: bool) -> int:
    seen: deque[tuple[str, float]] = deque(maxlen=64)
    by_session: dict[str, list[dict[str, Any]]] = defaultdict(list)

    print(
        f"{BOLD}chat-monitor{RESET} {DIM}polling {url} every {interval}s. "
        f"green=cache warm · red=cold start · yellow=partial. Ctrl-C to exit.{RESET}"
    )
    print()

    while True:
        metrics = fetch(url)
        if metrics is None:
            if once:
                print(f"{RED}!{RESET} could not reach {url}")
                return 1
            time.sleep(interval)
            continue

        latest = metrics.get("latest") or {}
        sid = latest.get("session_id")
        # Fingerprint: session + ttft + prompt_tokens uniquely IDs a request.
        fp = (
            sid or "?",
            float(latest.get("ttft_s") or 0),
            int(latest.get("prompt_tokens") or 0),
            int(latest.get("completion_tokens") or 0),
        )
        if sid and fp not in seen:
            seen.append(fp)
            by_session[sid].append(latest)
            ts = time.strftime("%H:%M:%S")
            print(f"{DIM}[{ts}]{RESET} {fmt(latest)}")
            # Hint when a session has multiple runs but cache never warmed.
            sess_runs = by_session[sid]
            if len(sess_runs) >= 2 and sum(
                1 for r in sess_runs if r.get("session_cache_hit")
            ) == 0:
                print(
                    f"  {YELLOW}!{RESET} {len(sess_runs)} runs on this "
                    f"session and still cold - prefix is diverging "
                    f"between calls (different system prompt? "
                    f"different message history?)"
                )

        if once:
            return 0
        time.sleep(interval)


def main() -> int:
    ap = argparse.ArgumentParser(description="Live MTPLX cache monitor for hip")
    ap.add_argument("--url", default=DEFAULT_URL)
    ap.add_argument(
        "--interval",
        type=float,
        default=1.0,
        help="Poll interval seconds (default 1.0)",
    )
    ap.add_argument(
        "--once",
        action="store_true",
        help="Print one snapshot and exit",
    )
    args = ap.parse_args()
    try:
        return run(args.url, args.interval, args.once)
    except KeyboardInterrupt:
        print()
        return 0


if __name__ == "__main__":
    sys.exit(main())
