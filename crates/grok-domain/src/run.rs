use thiserror::Error;

use crate::{ProjectId, RunId, ThreadId, UnixMillis};

/// Product flow that owns a durable run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    /// Legacy row whose producer predates explicit classification.
    Unspecified,
    /// Unprivileged conversation execution.
    Chat,
    /// User-started execution with an explicitly bound tool backend.
    Work,
    /// Background automation; `HostDirect` is structurally unavailable.
    Scheduled,
}

/// Concrete execution backend immutably bound to a Work run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkExecutionBackend {
    /// Explicitly risk-enrolled execution with desktop-user authority.
    HostDirect,
    /// Execution inside a qualified isolated guest.
    IsolatedGuest,
}

/// Durable lifecycle of an agent run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    /// Accepted but not started.
    Queued,
    /// Producing or revising an execution plan.
    Planning,
    /// Blocked until the user decides a scoped approval.
    AwaitingApproval,
    /// Actively executing work.
    Running,
    /// Explicitly paused by the user or policy engine.
    Paused,
    /// Finished successfully.
    Completed,
    /// Finished with an error.
    Failed,
    /// Cancelled before successful completion.
    Cancelled,
    /// A crash occurred while an external side effect might have been in flight.
    InterruptedNeedsReview,
}

impl RunState {
    /// Returns whether no further transitions are permitted.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    const fn permits(self, next: Self) -> bool {
        use RunState::{
            AwaitingApproval, Cancelled, Completed, Failed, InterruptedNeedsReview, Paused,
            Planning, Queued, Running,
        };
        matches!(
            (self, next),
            (Queued, Planning | Cancelled | InterruptedNeedsReview)
                | (
                    Planning,
                    AwaitingApproval | Running | Failed | Cancelled | InterruptedNeedsReview
                )
                | (
                    AwaitingApproval,
                    Running | Paused | Failed | Cancelled | InterruptedNeedsReview
                )
                | (
                    Running,
                    AwaitingApproval
                        | Paused
                        | Completed
                        | Failed
                        | Cancelled
                        | InterruptedNeedsReview
                )
                | (
                    Paused,
                    Running | Failed | Cancelled | InterruptedNeedsReview
                )
                | (InterruptedNeedsReview, Running | Failed | Cancelled)
        )
    }
}

/// Failure to apply a domain transition.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TransitionError {
    /// The requested lifecycle edge is not part of the product state machine.
    #[error("invalid run transition from {from:?} to {to:?}")]
    InvalidRunTransition {
        /// Existing lifecycle state.
        from: RunState,
        /// Requested lifecycle state.
        to: RunState,
    },
    /// A transition timestamp predates the entity's last update.
    #[error("transition timestamp {attempted} predates last update {current}")]
    ClockRegression {
        /// Current update timestamp.
        current: UnixMillis,
        /// Attempted update timestamp.
        attempted: UnixMillis,
    },
}

/// A durable unit of agent work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// Stable run identifier.
    pub id: RunId,
    /// Owning local project.
    pub project_id: ProjectId,
    /// Conversation associated with the work.
    pub thread_id: ThreadId,
    /// Product flow that created this run.
    pub kind: RunKind,
    /// Concrete backend for Work only.
    pub work_backend: Option<WorkExecutionBackend>,
    /// Current lifecycle state.
    pub state: RunState,
    /// Optimistic concurrency revision.
    pub revision: u64,
    /// Creation time.
    pub created_at: UnixMillis,
    /// Last successful state change.
    pub updated_at: UnixMillis,
}

impl Run {
    /// Creates a queued run.
    #[must_use]
    pub const fn queued(
        id: RunId,
        project_id: ProjectId,
        thread_id: ThreadId,
        now: UnixMillis,
    ) -> Self {
        Self {
            id,
            project_id,
            thread_id,
            kind: RunKind::Chat,
            work_backend: None,
            state: RunState::Queued,
            revision: 0,
            created_at: now,
            updated_at: now,
        }
    }

    /// Creates a queued Work run with an immutable concrete backend.
    #[must_use]
    pub const fn queued_work(
        id: RunId,
        project_id: ProjectId,
        thread_id: ThreadId,
        backend: WorkExecutionBackend,
        now: UnixMillis,
    ) -> Self {
        Self {
            id,
            project_id,
            thread_id,
            kind: RunKind::Work,
            work_backend: Some(backend),
            state: RunState::Queued,
            revision: 0,
            created_at: now,
            updated_at: now,
        }
    }

    /// Creates an unprivileged queued scheduler run.
    #[must_use]
    pub const fn queued_scheduled(
        id: RunId,
        project_id: ProjectId,
        thread_id: ThreadId,
        now: UnixMillis,
    ) -> Self {
        Self {
            id,
            project_id,
            thread_id,
            kind: RunKind::Scheduled,
            work_backend: None,
            state: RunState::Queued,
            revision: 0,
            created_at: now,
            updated_at: now,
        }
    }

    /// Returns whether this run may dispatch through the given Work backend.
    #[must_use]
    pub fn is_work_bound_to(&self, backend: WorkExecutionBackend) -> bool {
        self.kind == RunKind::Work && self.work_backend == Some(backend)
    }

    /// Applies a legal state transition and increments the revision.
    ///
    /// # Errors
    ///
    /// Returns [`TransitionError`] for an invalid lifecycle edge or a timestamp
    /// older than the entity's latest update.
    pub fn transition(&mut self, next: RunState, now: UnixMillis) -> Result<(), TransitionError> {
        if !self.state.permits(next) {
            return Err(TransitionError::InvalidRunTransition {
                from: self.state,
                to: next,
            });
        }
        if now < self.updated_at {
            return Err(TransitionError::ClockRegression {
                current: self.updated_at,
                attempted: now,
            });
        }
        self.state = next;
        self.revision = self.revision.saturating_add(1);
        self.updated_at = now;
        Ok(())
    }
}

/// Append-only audit event emitted by run use cases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunEvent {
    /// Sequence number scoped to a run, beginning at one.
    pub sequence: u64,
    /// Owning run.
    pub run_id: RunId,
    /// Event occurrence time.
    pub occurred_at: UnixMillis,
    /// Structured event kind.
    pub kind: RunEventKind,
}

/// Events clients can replay after reconnecting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEventKind {
    /// A run was accepted.
    Created,
    /// The state machine moved between lifecycle states.
    StateChanged {
        /// Previous lifecycle state.
        from: RunState,
        /// New lifecycle state.
        to: RunState,
    },
    /// A scoped approval blocked execution.
    ApprovalRequested {
        /// Created approval request.
        approval_id: crate::ApprovalId,
    },
    /// A side-effect intent was durably recorded.
    EffectPrepared {
        /// Persisted side-effect intent.
        effect_id: crate::EffectId,
    },
    /// A possible in-flight side effect requires user review.
    EffectNeedsReview {
        /// Effect whose external result is uncertain.
        effect_id: crate::EffectId,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run() -> Run {
        Run::queued(
            RunId::new("run-1").expect("id"),
            ProjectId::new("project-1").expect("id"),
            ThreadId::new("thread-1").expect("id"),
            10,
        )
    }

    #[test]
    fn happy_path_reaches_completion_with_monotonic_revision() {
        let mut run = run();
        run.transition(RunState::Planning, 11).expect("planning");
        run.transition(RunState::Running, 12).expect("running");
        run.transition(RunState::Completed, 13).expect("complete");
        assert_eq!(run.revision, 3);
        assert!(run.state.is_terminal());
    }

    #[test]
    fn terminal_and_clock_regression_transitions_are_rejected() {
        let mut run = run();
        assert!(matches!(
            run.transition(RunState::Completed, 11),
            Err(TransitionError::InvalidRunTransition { .. })
        ));
        run.transition(RunState::Planning, 11).expect("planning");
        assert!(matches!(
            run.transition(RunState::Running, 9),
            Err(TransitionError::ClockRegression { .. })
        ));
    }
}
