//! Pending-message queue. The chat loop processes one message per turn; if
//! the user has more than one message ready (e.g. they have a series of
//! follow-ups they want to run back-to-back), they can stage them in the
//! queue between turns via `/queue add <msg>` and the loop will dequeue +
//! send them automatically as the next prompts. The queue chains: turn N
//! finishes → if queue non-empty, dequeue head as turn N+1's prompt without
//! prompting the user → repeat until empty → fall back to readline.
//!
//! Phase 2 (future) will move this to a crossterm raw-mode reader that
//! captures input *during* streaming and supports up-arrow editing of
//! queued items in place. This module's data shape is built to extend.

use std::collections::VecDeque;
use std::sync::Mutex;

/// Shared queue of pending user messages. Held in run_chat's local scope
/// (no need for Arc since there's no second thread reading from it yet).
pub struct PendingQueue {
    items: Mutex<VecDeque<String>>,
}

impl PendingQueue {
    pub fn new() -> Self {
        Self {
            items: Mutex::new(VecDeque::new()),
        }
    }

    pub fn push(&self, msg: impl Into<String>) {
        self.items.lock().unwrap().push_back(msg.into());
    }

    /// Dequeue the head and return it. Returns None if empty.
    pub fn pop_front(&self) -> Option<String> {
        self.items.lock().unwrap().pop_front()
    }

    pub fn len(&self) -> usize {
        self.items.lock().unwrap().len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.items.lock().unwrap().is_empty()
    }

    /// Snapshot for display purposes (e.g. `/queue` command). Does not
    /// dequeue.
    pub fn peek_all(&self) -> Vec<String> {
        self.items.lock().unwrap().iter().cloned().collect()
    }

    pub fn clear(&self) {
        self.items.lock().unwrap().clear();
    }
}
