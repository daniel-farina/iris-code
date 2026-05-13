# PR #31 split - disposition record

PR #31 (https://github.com/youssofal/MTPLX/pull/31) bundled four SessionBank
fixes on stale lineage. We split it. This doc records what happened to each
piece so we can resurrect any DROPped change later if it turns out we need it.

Date: 2026-05-09
Upstream HEAD at decision time: `1fd87d732ea55def3267e80c32f09b761b34c866 Split sustained prefill chunk size by layout`

## Summary

| Commit | Subject | Decision | New PR | Resurrect-if |
|---|---|---|---|---|
| 1319e48 | 16K cliff fix | KEEP-MERGE | #39 | n/a - already landed |
| 4169e09 | entry-cap env | KEEP-MERGE | #40 | n/a once merged; if dropped, see archived diff |
| 110bb9d + 43bd51f | postcommit prefix reuse + miss-reason | MODIFY (Tier 1.2 part 1 + miss-reason only; orchestration split dropped) | #41 | upstream `ModelWorkScheduler` regresses to multi-thread or dedicates a separate executor whose foreground gens contend with idle postcommit on `state.lock` |
| 37c8cd7 | storage prefix-match | DROP | none | upstream removes `_postcommit_next_turn_prefix_ids` (`d4315b7`); the sentinel-marker approach can be replaced by 37c8cd7's `<\|im_start\|>` truncation as a fallback |

## 1319e48 - 16K cliff fix

Already landed as #39 (https://github.com/youssofal/MTPLX/pull/39) by another agent. No further action needed. Source diff archived below for reference.

```diff
commit 1319e482e89e1af33f48ddfb53f8e662f72cc1b5
Author: Daniel Farina <m3d.webdev@gmail.com>
Date:   Fri May 8 12:50:53 2026 -0400

    perf: fix SessionBank cache miss above 16K-token threshold

    [...]

diff --git a/mtplx/session_bank.py b/mtplx/session_bank.py
[full diff archived in /tmp/1319e48.diff at investigation time;
 see the upstream PR #39 for the merged form]
```

## 4169e09 - env-overridable SessionBank.max_entries

**What it did:** Adds `_bank_entries_from_env` helper and an `MTPLX_SESSION_BANK_MAX_ENTRIES` env var that overrides `SessionBank.max_entries` when constructing the singleton bank in `EngineSessionManager.__init__`. Default 8 unchanged. Plus 12 unit tests in `tests/test_session_bank_env_caps.py`.

**Upstream now:**

`mtplx/engine_session.py` (origin/main) has the byte-cap helper `_bank_bytes_from_env` from `87af6be` (PR #18), guarded by `9d92dac`. The construction site at `EngineSessionManager.__init__` reads:

```python
self.bank = bank or SessionBank(
    max_bytes=_bank_bytes_from_env("MTPLX_SESSION_BANK_MAX_BYTES", DEFAULT_MAX_BYTES),
    per_session_max_bytes=_bank_bytes_from_env("MTPLX_SESSION_BANK_PER_SESSION_BYTES", DEFAULT_PER_SESSION_MAX_BYTES),
    idle_ttl_s=idle_ttl_s,
)
```

There is NO `max_entries` env override - that axis is hard-coded to 8 (from `SessionBank.__init__`). Upstream's `_bank_bytes_from_env` is bytes-only (parses K/M/G/T suffixes), and `9d92dac`'s guards are about byte values too.

**Decision:** KEEP-MERGE.

**Reason:** Entry count is a different axis from byte cap. At ~2 GB per entry on Qwen3.6-27B paged KV with long agent prefixes, 8 entries binds the bank to ~16 GB even when `MTPLX_SESSION_BANK_MAX_BYTES` is set higher, evicting longest-matched prefixes per session. Upstream does not cover this.

**If we need this back:** merge our PR (link below). If for any reason we have to start from scratch, the original commit drops cleanly on top of upstream `_bank_bytes_from_env` - just add `_bank_entries_from_env` next to the existing helper and wire it into `SessionBank(max_entries=...)`.

```diff
[full git show 4169e09 archived in /tmp/4169e09.diff]
```

(See `4169e09` in the source branch `perf/postcommit-off-generation-executor` for the resurrection patch.)

## 110bb9d + 43bd51f - postcommit prefix reuse + cache_miss_reason observability

**What 110bb9d did:** Two-part fix.

1. **Tier 2.1 / Option B (the load-bearing piece):** `_store_retokenized_history_snapshot` passes `session_bank=state.sessions.bank` plus `template_hash`, `draft_head_identity`, `policy_fingerprint` to `restore_or_prefill_prompt_state`. This lets the postcommit re-prefill reuse the previous turn's bank entry as a prefix and only forward-AR the new suffix. Postcommit cost collapses from ~27 s (full 18K-token re-prefill) to ~1 s (suffix forward) on consecutive tool-calling turns.

2. **Option A (small dose):** the `_schedule_idle_postcommit_snapshot` orchestration loop runs on a separate `postcommit_executor` while the actual MLX-touching snapshot call is submitted to `generation_executor` via `Future.result()`. This keeps the 250 ms inter-attempt sleeps off the foreground executor.

Plus 5 unit tests in `tests/test_postcommit_executor_split.py`.

**What 43bd51f did:** One-line addition: `cache_miss_reason` field copied from `prompt_state` into the postcommit result dict. Plus a one-line test assertion. Depends on part 1 of 110bb9d (without `session_bank` plumbing, `prompt_state.cache_miss_reason` is not populated).

**Upstream now:**

Upstream's `mtplx/server/openai.py` has had massive postcommit churn since the source branch:

- `d4315b7` "Fix SessionBank postcommit prefix reachability" (210 lines in openai.py): introduced `_postcommit_next_turn_prefix_ids` (sentinel-message-based encoding), `_generation_final_postcommit_compatibility`, `_render_messages_for_postcommit`.
- `31baa26` "Keep streamed tool-call preambles in SessionBank history": preserves natural-language preambles before tool calls when building `assistant_content`.
- `3a545a6` "Wait briefly for prior session postcommit": adds bounded same-session postcommit wait via `EngineSession.set_pending_postcommit` / `wait_for_pending_postcommit`.
- `349fdf7` "Add batch-ready SessionBank model scheduler": replaces dual-executor model with a SINGLE-OWNER-THREAD `ModelWorkScheduler` that handles both foreground and idle postcommit. `_submit_foreground_model_work` and `_submit_idle_postcommit_model_work` both target this scheduler. The scheduler picks idle work only when no foreground is queued, but a started idle task DOES occupy the owner thread until completion.

Critical: the current postcommit call site is:

```python
prompt_state = restore_or_prefill_prompt_state(
    state.runtime,
    history_ids,
    mtp_hidden_variant="post_norm",
    mtp_history_policy="committed",
)
```

`session_bank` is NOT passed. So upstream's postcommit still pays the full re-prefill cost, just like the pre-110bb9d state. **Tier 1.2 / Option B is NOT covered upstream.**

For Option A (orchestration off generation_executor): upstream's single-owner-thread scheduler effectively means the 250 ms sleep concern is moot in the typical case. The orchestration loop's `state.lock.acquire(blocking=False)` is between the same owner thread holding it (foreground gen) - in upstream, foreground generation runs through `_submit_foreground_model_work` ON THE OWNER THREAD, which is the same thread the postcommit task runs on. So `state.lock` is never contended across threads while an idle postcommit is the active task. The orchestration loop will succeed on first iteration. The 30-second deadline never triggers in steady state. **Option A is structurally not needed under upstream's scheduler model.**

For 43bd51f: upstream's postcommit result dict does NOT include `cache_miss_reason`. The `last_miss_reason` field exists on `SessionBank` but is not propagated through `restore_or_prefill_prompt_state` -> `_store_retokenized_history_snapshot` result -> log line. Observability gap remains, BUT it depends on part 1 (passing `session_bank`) to be meaningful.

**Decision:** MODIFY.

**Reason:**
- Carve out Tier 1.2 / Option B (the prefix-reuse piece) and the 43bd51f miss-reason field. These are still needed and the diff is clean.
- DROP Option A (orchestration loop on `postcommit_executor`). It does not fit upstream's single-owner-thread scheduler. Replicating the two-stage executor split would either require resurrecting a separate `postcommit_executor` (regressing the scheduler unification) or adding compat shims for a configuration that doesn't exist.
- DROP the `tests/test_postcommit_executor_split.py` test file. Most of its tests assert behavior of the dual-executor split that we are not bringing across. We will replicate the ONE relevant test (`test_retokenized_snapshot_passes_session_bank_for_prefix_reuse`) inside the new PR but adapted to upstream signatures, AND add a small assertion that `cache_miss_reason` is propagated.

**If we need this back (full version, including Option A):** the source commits live on `perf/postcommit-off-generation-executor` (4169e09's parent branch). To resurrect Option A on top of a future upstream that re-introduces a dedicated `postcommit_executor`: re-apply 110bb9d's `_run_store_on_generation_executor` shim and the `executor = postcommit_executor if use_two_stage else state.generation_executor` selector. Original full diff archived below.

```diff
[full git show 110bb9d archived in /tmp/110bb9d.diff]
[full git show 43bd51f archived in /tmp/43bd51f.diff]
```

## 37c8cd7 - storage prefix-match

**What it did:** Introduced `_encode_history_for_storage` helper that appends a sentinel user message after the assistant turn, encodes the result with the chat template, then truncates before the LAST `<|im_start|>` token so the stored prefix is a strict prefix of any next-request encoding. Wired into both `_store_retokenized_history_snapshot` and `_history_ids_for_postcommit`.

The motivation: Qwen3 chat template injects `<think>\n\n</think>\n\n` before tool_call markup when the assistant is the trailing message and `enable_thinking=False`. Storage's synthetic history makes the assistant trailing; lookup-time the assistant has tool_result+user_msg following it, so no injection. Storage tokens != lookup tokens -> `prefix_divergence_at_token` miss every turn.

**Upstream now:**

`d4315b7` "Fix SessionBank postcommit prefix reachability" introduced `_postcommit_next_turn_prefix_ids` which solves the SAME problem with a different mechanism:

1. Appends a sentinel `__MTPLX_POSTCOMMIT_SENTINEL_4f02c7d2__` message (role=`tool` if assistant has tool_calls, else role=`user`).
2. Renders via `apply_chat_template`.
3. `_sentinel_next_turn_start` searches the rendered string for a role-marker (`<|im_start|>{role}\n` for chatml, with fallbacks for other templates) immediately before the sentinel content position.
4. Encodes only the prefix text up to that role marker.

Plus a fallback: `_history_ids_for_postcommit` calls `_postcommit_next_turn_prefix_ids` first, falls back to `_encode_messages` of `messages + assistant` if the sentinel approach fails.

Upstream's approach is strictly more general:
- Handles tool_call case explicitly (role=`tool` sentinel).
- Works on the rendered text, not on raw token ids - more robust to tokenizer variations.
- Cross-template (chatml, llama-2, alpaca, generic `Role:` prefix).

**Decision:** DROP.

**Reason:** Upstream `d4315b7` covers the same need (storage prefix == lookup prefix) with a more robust, more general mechanism. Reapplying 37c8cd7 on top would either fight `_postcommit_next_turn_prefix_ids` (two helpers doing the same job, the second one wrapping the first) or replace it (regressing the role-aware sentinel). Neither is worth it.

**If we need this back:** if upstream ever removes `_postcommit_next_turn_prefix_ids` and the live workload regresses to `prefix_divergence_at_token`, drop in 37c8cd7's `_encode_history_for_storage` helper as a fallback - it works for chatml only but is simpler to reason about. Original full diff archived below.

```diff
[full git show 37c8cd7 archived in /tmp/37c8cd7.diff]
```

## Test results

All tests run in `/Users/dan/code-2/forks/MTPLX/.venv/bin/python -m pytest`.

### PR #40 (entry-cap env)

```
$ pytest tests/test_session_bank_env_caps.py tests/test_engine_session_env.py
============================== 35 passed in 0.08s ==============================
```

19 new tests in `test_session_bank_env_caps.py` plus 16 existing in `test_engine_session_env.py`.

### PR #41 (postcommit prefix reuse + miss-reason)

```
$ pytest tests/test_postcommit_tools_plumbing.py tests/test_idle_postcommit_subagent.py \
        tests/test_postcommit_wait.py tests/test_postcommit_wait_integration.py \
        tests/test_postcommit_prefix_reuse.py
35 passed in 0.34s

$ pytest tests/test_server_openai.py tests/test_openai_bridge.py tests/test_model_scheduler.py
64 passed in 0.40s
```

3 new tests in `test_postcommit_prefix_reuse.py` plus 32 existing postcommit tests; 64 server/scheduler tests confirm no regressions in the surrounding code.

### 37c8cd7 (DROPped)

No tests run; upstream `_postcommit_next_turn_prefix_ids` is exercised by `test_postcommit_tools_plumbing.py` (already passing under PR #41 verification).


