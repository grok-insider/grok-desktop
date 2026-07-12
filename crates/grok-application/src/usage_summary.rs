//! Read-only aggregates of official conversation-turn usage.

use std::sync::Arc;

use grok_domain::{ProjectId, ThreadId, UnixMillis};

use crate::{ApplicationError, Clock, ConversationTurnStore, StoreError};

/// One day in milliseconds.
const DAY_MS: u64 = 86_400_000;
/// Rolling week window.
const LAST_7_DAYS_MS: u64 = 7 * DAY_MS;
/// Rolling month-ish window used by product copy (“last 30 days”).
const LAST_30_DAYS_MS: u64 = 30 * DAY_MS;

/// Which conversation turns participate in a summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsageScope {
    /// Every completed turn in the local workspace.
    Workspace,
    /// Completed turns owned by one project.
    Project(ProjectId),
    /// Completed turns owned by one conversation thread.
    Thread(ThreadId),
}

/// Rolling or unbounded time filter for a summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageWindow {
    /// Turns with `created_at >= now - 7 days`.
    Last7Days,
    /// Turns with `created_at >= now - 30 days`.
    Last30Days,
    /// No lower bound on `created_at`.
    AllTime,
}

/// Bounded aggregate of provider-reported usage for completed turns only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageSummary {
    /// Sum of input tokens across matching completed turns.
    pub input_tokens: u64,
    /// Sum of output tokens across matching completed turns.
    pub output_tokens: u64,
    /// Sum of xAI cost ticks (1 USD = 10_000_000_000 ticks).
    pub cost_in_usd_ticks: u64,
    /// Number of completed turns included.
    pub turn_count: u64,
    /// Scope used for the query.
    pub scope: UsageScope,
    /// Window used for the query.
    pub window: UsageWindow,
    /// Clock sample that defined the window lower bound.
    pub as_of: UnixMillis,
}

/// Input for one read-only usage summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetUsageSummary {
    /// Workspace, project, or thread filter.
    pub scope: UsageScope,
    /// Rolling window or all-time.
    pub window: UsageWindow,
}

/// Aggregates durable conversation-turn usage without provider network access.
pub struct UsageSummaryService {
    store: Arc<dyn ConversationTurnStore>,
    clock: Arc<dyn Clock>,
}

impl UsageSummaryService {
    /// Creates the service from the conversation store and clock.
    #[must_use]
    pub fn new(store: Arc<dyn ConversationTurnStore>, clock: Arc<dyn Clock>) -> Self {
        Self { store, clock }
    }

    /// Returns official completed-turn usage for the requested scope and window.
    ///
    /// # Errors
    ///
    /// Returns [`ApplicationError`] when the scope identifiers are invalid in the
    /// store layer or persistence fails.
    pub async fn summarize(&self, input: GetUsageSummary) -> Result<UsageSummary, ApplicationError> {
        let as_of = self.clock.now();
        let summary = self
            .store
            .summarize_usage(input.scope, input.window, as_of)
            .await
            .map_err(map_store)?;
        Ok(summary)
    }
}

/// Inclusive lower bound on `created_at`, or `None` for all-time.
#[must_use]
pub fn window_lower_bound(window: UsageWindow, as_of: UnixMillis) -> Option<UnixMillis> {
    match window {
        UsageWindow::AllTime => None,
        UsageWindow::Last7Days => Some(as_of.saturating_sub(LAST_7_DAYS_MS)),
        UsageWindow::Last30Days => Some(as_of.saturating_sub(LAST_30_DAYS_MS)),
    }
}

fn map_store(error: StoreError) -> ApplicationError {
    match error {
        StoreError::NotFound => ApplicationError::NotFound,
        StoreError::Conflict => ApplicationError::Conflict,
        StoreError::Unavailable(message) => ApplicationError::Unavailable(message),
        StoreError::Internal(message) => ApplicationError::Storage(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_bounds_are_rolling_from_as_of() {
        let as_of = 1_700_000_000_000_u64;
        assert_eq!(window_lower_bound(UsageWindow::AllTime, as_of), None);
        assert_eq!(
            window_lower_bound(UsageWindow::Last7Days, as_of),
            Some(as_of - LAST_7_DAYS_MS)
        );
        assert_eq!(
            window_lower_bound(UsageWindow::Last30Days, as_of),
            Some(as_of - LAST_30_DAYS_MS)
        );
        assert_eq!(window_lower_bound(UsageWindow::Last7Days, 0), Some(0));
    }
}
