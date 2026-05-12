//! Auto-pruning of older tool-result messages once a conversation grows past
//! a watermark. The model never asked the user to keep them; once a tool result
//! has been seen, summarized, and acted on a few turns ago, sending its full
//! content with every subsequent request is pure prefill cost.
//!
//! Strategy (mirrors what opencode does for local-model long-context runs):
//!
//! 1. Estimate total prompt tokens by char-count / `CHARS_PER_TOKEN`.
//! 2. If total < `trigger_tokens`, do nothing.
//! 3. Walk backward from the latest message, accumulating estimated tokens
//!    until we cross `recent_window_tokens`. Everything OLDER than that point
//!    is eligible for pruning.
//! 4. Among eligible messages, look at `role: "tool"` (tool-result) messages
//!    larger than `min_prunable_chars`. Compute total potential savings.
//! 5. If potential savings < `min_savings_tokens`, do nothing (avoid making a
//!    visible-but-not-useful change).
//! 6. Otherwise replace each large old tool-result `content` with a short stub
//!    naming the original size, so the model can see "I had a tool result here
//!    that's been pruned".
//!
//! User-facing controls: env `MLX_CODE_NO_AUTO_PRUNE=1` disables. Per-invocation
//! `--no-auto-prune` CLI flag wires to the same disable path.

use crate::schema::ChatMessage;

/// Average chars-per-token across English text and code. Used for cheap token
/// estimation without invoking the tokenizer. Slightly pessimistic so we trip
/// the trigger earlier rather than later.
const CHARS_PER_TOKEN: usize = 3;

#[derive(Debug, Clone, Copy)]
pub struct PruneOpts {
    /// Don't even consider pruning unless estimated total prompt tokens exceed
    /// this. Default 20K - well below qwen3's ~25K prefill-cliff.
    pub trigger_tokens: usize,
    /// Keep all messages within this many tokens of the end of the conversation
    /// intact. Default 40K.
    pub recent_window_tokens: usize,
    /// Only prune if total savings would be at least this large. Avoids the
    /// "we made the diff look busy without helping" case.
    pub min_savings_tokens: usize,
    /// Only prune a tool-result whose content is at least this many chars.
    /// Tiny outputs aren't worth a stub.
    pub min_prunable_chars: usize,
}

impl Default for PruneOpts {
    fn default() -> Self {
        Self {
            trigger_tokens: 20_000,
            recent_window_tokens: 40_000,
            min_savings_tokens: 10_000,
            min_prunable_chars: 200,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PruneReport {
    pub pruned_count: usize,
    pub savings_tokens: usize,
    pub total_before_tokens: usize,
    pub total_after_tokens: usize,
}

/// Estimate tokens in a string via char-count / `CHARS_PER_TOKEN`.
pub fn estimate_tokens(s: &str) -> usize {
    s.chars().count() / CHARS_PER_TOKEN
}

/// Estimate total tokens across every message's content (system, user,
/// assistant text, tool results all counted). Tool-calls structured
/// payloads aren't counted (they're typically small relative to content).
pub fn estimate_conv_tokens(conv: &[ChatMessage]) -> usize {
    conv.iter()
        .filter_map(|m| m.content.as_deref())
        .map(estimate_tokens)
        .sum()
}

/// Inspect the conversation and prune older tool-result messages when warranted.
/// Returns Some(report) if any pruning happened, None otherwise.
pub fn maybe_prune_tool_outputs(
    conv: &mut Vec<ChatMessage>,
    opts: PruneOpts,
) -> Option<PruneReport> {
    let total_before = estimate_conv_tokens(conv);
    if total_before < opts.trigger_tokens {
        return None;
    }

    // Walk backward from the end, accumulate tokens until we cross
    // `recent_window_tokens`. Messages BEFORE the resulting index are
    // older-than-the-window and eligible for pruning.
    let mut accumulated = 0usize;
    let mut window_start_idx = 0usize;
    let mut crossed = false;
    for (i, m) in conv.iter().enumerate().rev() {
        accumulated += m
            .content
            .as_deref()
            .map(estimate_tokens)
            .unwrap_or(0);
        if accumulated >= opts.recent_window_tokens {
            window_start_idx = i;
            crossed = true;
            break;
        }
    }
    if !crossed {
        // Whole conversation fits in the recent window; nothing to prune.
        return None;
    }

    // Eligible: role=="tool" AND content large enough AND older than the
    // window start. The message at `window_start_idx` is the one whose
    // tokens tipped accumulated past the threshold — most of its content
    // is in the OLD zone, so we include it (inclusive range). Messages
    // strictly newer than it are 100% within the recent window and
    // preserved.
    //
    // Keep system + user/assistant pairs even when they're old — those are
    // conversational state and harder to summarize safely.
    let eligible: Vec<usize> = (0..=window_start_idx)
        .filter(|&i| {
            conv[i].role == "tool"
                && conv[i]
                    .content
                    .as_deref()
                    .map(|c| c.chars().count() >= opts.min_prunable_chars)
                    .unwrap_or(false)
        })
        .collect();

    let potential_savings: usize = eligible
        .iter()
        .filter_map(|&i| conv[i].content.as_deref())
        .map(estimate_tokens)
        .sum();

    if potential_savings < opts.min_savings_tokens {
        return None;
    }

    // Apply pruning.
    for &i in &eligible {
        let content = conv[i].content.take().unwrap_or_default();
        let chars = content.chars().count();
        let toks = estimate_tokens(&content);
        // Leave a breadcrumb so the model knows this slot used to hold a tool
        // output and can decide to re-run the tool if it still needs the data.
        let stub = format!(
            "[tool output pruned for context window: {} chars (~{} tok). Re-run the tool if you need this content again.]",
            chars, toks
        );
        conv[i].content = Some(stub);
    }

    let report = PruneReport {
        pruned_count: eligible.len(),
        savings_tokens: potential_savings,
        total_before_tokens: total_before,
        total_after_tokens: total_before.saturating_sub(potential_savings),
    };
    Some(report)
}

/// Convenience: read PruneOpts from env. `MLX_CODE_NO_AUTO_PRUNE=1` returns None.
/// Override the trigger via `MLX_CODE_PRUNE_TRIGGER_TOKENS`.
pub fn opts_from_env(disabled_flag: bool) -> Option<PruneOpts> {
    if disabled_flag {
        return None;
    }
    if std::env::var("MLX_CODE_NO_AUTO_PRUNE")
        .ok()
        .map(|v| v != "0" && !v.is_empty())
        .unwrap_or(false)
    {
        return None;
    }
    let mut opts = PruneOpts::default();
    if let Ok(v) = std::env::var("MLX_CODE_PRUNE_TRIGGER_TOKENS") {
        if let Ok(n) = v.parse::<usize>() {
            opts.trigger_tokens = n;
        }
    }
    if let Ok(v) = std::env::var("MLX_CODE_PRUNE_RECENT_WINDOW_TOKENS") {
        if let Ok(n) = v.parse::<usize>() {
            opts.recent_window_tokens = n;
        }
    }
    Some(opts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::ChatMessage;

    fn make_tool_msg(id: &str, body_chars: usize) -> ChatMessage {
        ChatMessage::tool_result(id, "x".repeat(body_chars))
    }

    fn make_user_msg(chars: usize) -> ChatMessage {
        ChatMessage::user("u".repeat(chars))
    }

    #[test]
    fn below_trigger_returns_none() {
        let mut conv = vec![ChatMessage::user("hello"), ChatMessage::assistant_text("hi")];
        let opts = PruneOpts {
            trigger_tokens: 100,
            recent_window_tokens: 50,
            min_savings_tokens: 10,
            min_prunable_chars: 10,
        };
        assert!(maybe_prune_tool_outputs(&mut conv, opts).is_none());
    }

    #[test]
    fn prunes_old_large_tool_results() {
        // Layout:
        //   user (5 chars)
        //   tool_result OLD (3000 chars ~ 1000 toks)
        //   user (5 chars)
        //   tool_result NEW (300 chars - smaller, recent)
        //   user (5 chars)
        let mut conv = vec![
            make_user_msg(5),
            make_tool_msg("t1", 3000),
            make_user_msg(5),
            make_tool_msg("t2", 300),
            make_user_msg(5),
        ];
        // Force the trigger and pick a window small enough that t1 falls outside.
        let opts = PruneOpts {
            trigger_tokens: 500,
            recent_window_tokens: 200, // ~600 chars of recent context
            min_savings_tokens: 100,
            min_prunable_chars: 200,
        };
        let report = maybe_prune_tool_outputs(&mut conv, opts).expect("should have pruned");
        assert_eq!(report.pruned_count, 1);
        assert!(report.savings_tokens > 0);
        // First tool message should be a stub now.
        let stubbed = conv[1].content.as_deref().unwrap_or("");
        assert!(
            stubbed.contains("tool output pruned"),
            "expected stub, got: {stubbed}"
        );
        // Second tool message should be untouched.
        assert_eq!(conv[3].content.as_deref().unwrap().len(), 300);
    }

    #[test]
    fn savings_below_min_returns_none() {
        // Only a tiny tool result old enough to prune — savings won't clear the
        // min_savings threshold, so prune is skipped.
        let mut conv = vec![
            make_user_msg(5),
            make_tool_msg("t1", 250), // ~83 toks
            make_user_msg(5000),      // forces trigger
        ];
        let opts = PruneOpts {
            trigger_tokens: 500,
            recent_window_tokens: 1000,
            min_savings_tokens: 5000, // higher than t1 alone
            min_prunable_chars: 200,
        };
        assert!(maybe_prune_tool_outputs(&mut conv, opts).is_none());
        // Unchanged.
        assert_eq!(conv[1].content.as_deref().unwrap().len(), 250);
    }

    #[test]
    fn does_not_touch_user_or_assistant_messages() {
        let mut conv = vec![
            make_user_msg(3000), // old, big — but it's a USER msg, don't prune
            make_tool_msg("t1", 3000),
            make_user_msg(5),
        ];
        let opts = PruneOpts {
            trigger_tokens: 500,
            recent_window_tokens: 200,
            min_savings_tokens: 100,
            min_prunable_chars: 200,
        };
        let _ = maybe_prune_tool_outputs(&mut conv, opts);
        // The first message (user, 3000 chars) must remain intact.
        assert_eq!(conv[0].content.as_deref().unwrap().len(), 3000);
    }
}
