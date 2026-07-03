use thiserror::Error;

use crate::{EffectId, RunId, UnixMillis};

/// Broad side-effect class used for policy and recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectKind {
    /// Host or project filesystem mutation.
    FileWrite,
    /// External process invocation.
    ProcessExecution,
    /// Network mutation such as a send or publish.
    ExternalMutation,
    /// Desktop input action.
    ComputerInput,
}

/// Whether an operation can be safely retried with the same idempotency key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Idempotency {
    /// The external system honors the supplied idempotency key.
    Idempotent,
    /// Repeating the operation could duplicate user-visible behavior.
    NonIdempotent,
}

/// Durable execution state of a side-effect intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectState {
    /// Intent is persisted but has not crossed the external boundary.
    Prepared,
    /// Dispatch began and completion is not yet recorded.
    Executing,
    /// External operation completed successfully.
    Succeeded,
    /// External operation returned a known failure.
    Failed,
    /// Process interruption left the external result uncertain.
    NeedsReview,
}

/// Invalid lifecycle operation for a side effect.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid effect transition from {from:?} to {to:?}")]
pub struct EffectTransitionError {
    /// Existing state.
    pub from: EffectState,
    /// Requested state.
    pub to: EffectState,
}

/// Persisted intent surrounding every externally visible mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SideEffect {
    /// Stable effect and idempotency identifier.
    pub id: EffectId,
    /// Owning run.
    pub run_id: RunId,
    /// Policy classification.
    pub kind: EffectKind,
    /// Non-secret target description.
    pub target: String,
    /// Retry contract.
    pub idempotency: Idempotency,
    /// Current lifecycle state.
    pub state: EffectState,
    /// Optimistic revision.
    pub revision: u64,
    /// Creation timestamp.
    pub created_at: UnixMillis,
    /// Last state change.
    pub updated_at: UnixMillis,
}

impl SideEffect {
    /// Records an intent before an external operation begins.
    #[must_use]
    pub const fn prepare(
        id: EffectId,
        run_id: RunId,
        kind: EffectKind,
        target: String,
        idempotency: Idempotency,
        now: UnixMillis,
    ) -> Self {
        Self {
            id,
            run_id,
            kind,
            target,
            idempotency,
            state: EffectState::Prepared,
            revision: 0,
            created_at: now,
            updated_at: now,
        }
    }

    /// Marks dispatch as started.
    ///
    /// # Errors
    ///
    /// Returns [`EffectTransitionError`] unless this intent is prepared.
    pub fn start(&mut self, now: UnixMillis) -> Result<(), EffectTransitionError> {
        self.move_to(EffectState::Executing, now)
    }

    /// Records a known external result.
    ///
    /// # Errors
    ///
    /// Returns [`EffectTransitionError`] unless dispatch is currently executing.
    pub fn finish(
        &mut self,
        succeeded: bool,
        now: UnixMillis,
    ) -> Result<(), EffectTransitionError> {
        self.move_to(
            if succeeded {
                EffectState::Succeeded
            } else {
                EffectState::Failed
            },
            now,
        )
    }

    /// Converts an interrupted dispatch into an explicit recovery decision.
    ///
    /// # Errors
    ///
    /// Returns [`EffectTransitionError`] unless dispatch is currently executing.
    pub fn interrupt(&mut self, now: UnixMillis) -> Result<(), EffectTransitionError> {
        self.move_to(EffectState::NeedsReview, now)
    }

    fn move_to(&mut self, next: EffectState, now: UnixMillis) -> Result<(), EffectTransitionError> {
        let valid = matches!(
            (self.state, next),
            (EffectState::Prepared, EffectState::Executing)
                | (
                    EffectState::Executing,
                    EffectState::Succeeded | EffectState::Failed | EffectState::NeedsReview
                )
        );
        if !valid {
            return Err(EffectTransitionError {
                from: self.state,
                to: next,
            });
        }
        self.state = next;
        self.revision = self.revision.saturating_add(1);
        self.updated_at = now.max(self.updated_at);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_dispatch_never_returns_to_prepared() {
        let mut effect = SideEffect::prepare(
            EffectId::new("effect-1").expect("id"),
            RunId::new("run-1").expect("id"),
            EffectKind::ExternalMutation,
            "publish".into(),
            Idempotency::NonIdempotent,
            1,
        );
        effect.start(2).expect("start");
        effect.interrupt(3).expect("interrupt");
        assert_eq!(effect.state, EffectState::NeedsReview);
        assert!(effect.start(4).is_err());
    }
}
