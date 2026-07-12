//! Runtime selection for the credential rail used by newly created chat turns.

use std::sync::atomic::{AtomicU8, Ordering};

use grok_domain::ChatRail;

/// Process-local selection shared by chat discovery and conversation creation.
///
/// A turn persists the selected rail in its lineage before provider dispatch;
/// changing this value therefore affects only new turns.
pub struct ChatRailSelection(AtomicU8);

impl ChatRailSelection {
    /// Creates a selection with one explicit initial rail.
    #[must_use]
    pub fn new(rail: ChatRail) -> Self {
        Self(AtomicU8::new(encode(rail)))
    }

    /// Returns the rail currently selected for new chat work.
    #[must_use]
    pub fn current(&self) -> ChatRail {
        decode(self.0.load(Ordering::Acquire))
    }

    /// Selects the rail used by subsequent chat discovery and new turns.
    pub fn set(&self, rail: ChatRail) {
        self.0.store(encode(rail), Ordering::Release);
    }
}

fn encode(rail: ChatRail) -> u8 {
    match rail {
        ChatRail::XaiApiKey => 0,
        ChatRail::SuperGrokApi => 1,
    }
}

fn decode(value: u8) -> ChatRail {
    match value {
        1 => ChatRail::SuperGrokApi,
        _ => ChatRail::XaiApiKey,
    }
}
