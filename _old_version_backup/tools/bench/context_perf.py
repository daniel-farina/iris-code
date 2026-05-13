#!/usr/bin/env python3
"""Synthetic-context perf bench for the local MTPLX server.

Sends prompts of increasing size (20K, 40K, 60K, 80K... up to a cap) and
records TTFT / decode-rate / total time per request to a JSONL file.

Usage:
    python3 tools/bench/context_perf.py [--start 20000] [--stop 80000] [--step 20000]
                                        [--out ~/.mlx-code/logs/context-perf.jsonl]
                                        [--session bench-context]

Run directly against MTPLX (port 8088) — does not go through mlx-code, so
results reflect the model server's prefill+decode without client overhead.
"""
import argparse, json, os, random, time, urllib.request, sys

URL = "http://127.0.0.1:8088/v1/chat/completions"
MODEL = "mtplx-qwen36-27b-optimized-speed"
WORDS = (
    "function class import export const if else return while for try catch "
    "throw new this self await async let var interface extends implements "
    "type enum namespace module factor request handler middleware route "
    "render geometry shader buffer texture attribute uniform vertex fragment "
).split()


def build_prompt_for_token_target(target_tokens: int) -> str:
    # Empirically Qwen3 tokenizer ~3.5-4 chars/token for English text.
    # Padding loop builds a prompt that lands close to the target.
    random.seed(0x600D)
    target_chars = int(target_tokens * 4)
    parts = ["Read the following reference dump and at the very end answer 'OK' "
             "with NO commentary.\n\n=== reference dump ===\n"]
    while sum(len(p) for p in parts) < target_chars:
        parts.append(" ".join(random.choices(WORDS, k=200)) + "\n")
    parts.append("\n=== end reference dump ===\nReply with one short paragraph (~50 words) summarising the dump genre.")
    return "".join(parts)


def post(messages, session_id):
    body = {
        "model": MODEL,
        "messages": messages,
        "max_tokens": 80,
        "stream": True,
        "temperature": 0.1,
        "enable_thinking": False,
        "chat_template_kwargs": {"enable_thinking": False},
    }
    req = urllib.request.Request(
        URL,
        data=json.dumps(body).encode(),
        headers={
            "Content-Type": "application/json",
            "x-mtplx-session-id": session_id,
        },
    )
    t0 = time.time()
    ttft = None
    completion_tokens = 0
    prompt_tokens = None
    last_text = ""
    with urllib.request.urlopen(req, timeout=900) as resp:
        for raw in resp:
            line = raw.decode("utf-8", errors="ignore").strip()
            if not line.startswith("data:"):
                continue
            payload = line[5:].strip()
            if payload == "[DONE]" or not payload:
                continue
            try:
                obj = json.loads(payload)
            except Exception:
                continue
            for ch in obj.get("choices") or []:
                d = ch.get("delta") or {}
                if "content" in d and d["content"]:
                    if ttft is None:
                        ttft = time.time() - t0
                    completion_tokens += 1
                    last_text += d["content"]
            if obj.get("usage"):
                prompt_tokens = (obj["usage"] or {}).get("prompt_tokens")
                if (obj["usage"] or {}).get("completion_tokens") is not None:
                    completion_tokens = obj["usage"]["completion_tokens"]
    total = time.time() - t0
    return {
        "ttft_s": ttft,
        "total_s": total,
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "reply": last_text.strip()[:40],
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--start", type=int, default=20000)
    ap.add_argument("--stop", type=int, default=80000)
    ap.add_argument("--step", type=int, default=20000)
    ap.add_argument("--out", default=os.path.expanduser("~/.mlx-code/logs/context-perf.jsonl"))
    ap.add_argument("--session", default=f"bench-context-{int(time.time())}")
    ap.add_argument("--no-summary", action="store_true", help="skip the final summary table")
    args = ap.parse_args()
    os.makedirs(os.path.dirname(args.out), exist_ok=True)
    sizes = list(range(args.start, args.stop + 1, args.step))
    started = time.strftime("%Y-%m-%d %H:%M:%S")
    print(f"# context_perf.py {started}")
    print(f"#   url={URL}")
    print(f"#   model={MODEL}")
    print(f"#   session={args.session}")
    print(f"#   sizes={sizes}")
    print(f"#   logging to {args.out}")
    print(f"# {'target':>8} {'prompt_tok':>10} {'ttft_s':>8} {'total_s':>8} {'prefill_t/s':>11} {'decode_t/s':>10} {'ttft_x':>7} {'reply'}")
    rows = []
    first_ttft = None
    for sz in sizes:
        prompt = build_prompt_for_token_target(sz)
        msgs = [{"role": "user", "content": prompt}]
        try:
            r = post(msgs, session_id=args.session)
            ct = r.get("completion_tokens") or 0
            ttft = r.get("ttft_s")
            tot = r.get("total_s") or 0.0
            pt = r.get("prompt_tokens") or 0
            decode_s = (tot - ttft) if (ttft is not None and tot > ttft) else 0.0
            decode_tps = ct / max(0.001, decode_s) if decode_s > 0.05 else 0.0
            prefill_tps = pt / ttft if (ttft and pt) else 0.0
            row = {
                "ts_unix": int(time.time()),
                "target_tokens": sz,
                **r,
                "decode_tok_per_s": decode_tps,
                "prefill_tok_per_s": prefill_tps,
                "session_id": args.session,
            }
            with open(args.out, "a") as f:
                f.write(json.dumps(row) + "\n")
            ttft_disp = "-" if ttft is None else f"{ttft:.2f}"
            pt_disp = "?" if not pt else str(pt)
            if first_ttft is None and ttft is not None:
                first_ttft = ttft
            ttft_ratio = f"{ttft/first_ttft:.2f}x" if (first_ttft and ttft is not None) else "-"
            print(f"  {sz:>8} {pt_disp:>10} {ttft_disp:>8} {tot:>8.2f} {prefill_tps:>11.1f} {decode_tps:>10.1f} {ttft_ratio:>7} {r['reply']!r}")
            rows.append(row)
        except Exception as e:
            print(f"  {sz:>8} ERROR: {e}", file=sys.stderr)
            with open(args.out, "a") as f:
                f.write(json.dumps({"ts_unix": int(time.time()), "target_tokens": sz, "error": str(e), "session_id": args.session}) + "\n")

    if not args.no_summary and rows:
        print()
        print("# === summary ===")
        # Aggregate stats.
        ttfts = [r["ttft_s"] for r in rows if r.get("ttft_s")]
        prefills = [r["prefill_tok_per_s"] for r in rows if r.get("prefill_tok_per_s", 0) > 0]
        decodes = [r["decode_tok_per_s"] for r in rows if r.get("decode_tok_per_s", 0) > 0]
        if ttfts:
            print(f"#   ttft       min={min(ttfts):.2f}s  med={sorted(ttfts)[len(ttfts)//2]:.2f}s  max={max(ttfts):.2f}s")
        if prefills:
            print(f"#   prefill    min={min(prefills):.0f}t/s  med={sorted(prefills)[len(prefills)//2]:.0f}t/s  max={max(prefills):.0f}t/s")
        if decodes:
            print(f"#   decode     min={min(decodes):.1f}t/s  med={sorted(decodes)[len(decodes)//2]:.1f}t/s  max={max(decodes):.1f}t/s")
        # Scaling: linear vs superlinear.
        if len(rows) >= 2:
            first = rows[0]
            last = rows[-1]
            tt_ratio = (last["ttft_s"] / first["ttft_s"]) if first.get("ttft_s") and last.get("ttft_s") else None
            sz_ratio = last["target_tokens"] / first["target_tokens"] if first["target_tokens"] else None
            if tt_ratio and sz_ratio:
                shape = "linear (good)" if abs(tt_ratio - sz_ratio) < 0.5 * sz_ratio else \
                        ("superlinear (worse than linear)" if tt_ratio > sz_ratio else "sublinear (better than linear)")
                print(f"#   scaling    {first['target_tokens']}->{last['target_tokens']} tokens ({sz_ratio:.1f}x) "
                      f"-> ttft {tt_ratio:.2f}x  [{shape}]")
        wall = sum(r.get("total_s", 0) for r in rows)
        print(f"#   wall-clock total: {wall:.1f}s across {len(rows)} run(s)")


if __name__ == "__main__":
    main()
