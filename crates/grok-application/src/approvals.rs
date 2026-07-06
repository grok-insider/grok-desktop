use std::sync::Arc;

use grok_domain::{
    Approval, ApprovalDecision, ApprovalError, ApprovalId, ApprovalScope, ApprovalStatus,
    RequestedAction, RunEventKind, RunId, RunState,
};

use crate::{ApplicationError, Clock, ExecutionStore, IdGenerator, NewRunEvent};
use crate::{ExecutionMutationOutcome, mutations::mutation_command};

/// Input for a scoped approval checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestApproval {
    /// Run that must be actively planning or executing.
    pub run_id: RunId,
    /// Revision observed by the caller.
    pub expected_run_revision: u64,
    /// Exact user-visible operation.
    pub action: RequestedAction,
    /// Maximum grant reuse.
    pub scope: ApprovalScope,
    /// Absolute decision deadline.
    pub expires_at: u64,
}

/// Coordinates approval and run state changes transactionally.
pub struct ApprovalService {
    store: Arc<dyn ExecutionStore>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
}

impl ApprovalService {
    /// Creates an approval service.
    #[must_use]
    pub fn new(
        store: Arc<dyn ExecutionStore>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        Self { store, clock, ids }
    }

    /// Blocks a run on a newly persisted approval request.
    ///
    /// # Errors
    ///
    /// Returns [`ApplicationError`] when input is invalid, the run is stale or in
    /// an incompatible state, or the atomic store operation fails.
    pub async fn request(
        &self,
        input: RequestApproval,
        idempotency_key: &str,
    ) -> Result<Approval, ApplicationError> {
        let (scope, resource) = approval_scope_key(&input.scope);
        let command = mutation_command(
            "request_approval",
            idempotency_key,
            &[
                input.run_id.as_str().into(),
                input.expected_run_revision.to_string(),
                input.action.action.clone(),
                input.action.target.clone(),
                input.action.data_summary.clone(),
                approval_risk_key(input.action.risk).into(),
                scope.into(),
                resource.into(),
                input.expires_at.to_string(),
            ],
        )?;
        if let Some(outcome) = self.store.resolve_execution_mutation(&command).await? {
            return approval_outcome(outcome);
        }
        let now = self.clock.now();
        if input.expires_at <= now {
            return Err(ApplicationError::InvalidInput(
                "approval expiry must be in the future".into(),
            ));
        }
        let mut run = self.store.get_run(&input.run_id).await?;
        if run.revision != input.expected_run_revision {
            return Err(ApplicationError::Conflict);
        }
        let from = run.state;
        run.transition(RunState::AwaitingApproval, now)?;
        let approval = Approval::pending(
            ApprovalId::new(self.ids.generate("approval"))?,
            input.run_id,
            input.action,
            input.scope,
            now,
            input.expires_at,
        );
        Ok(self
            .store
            .create_approval(
                approval.clone(),
                run.clone(),
                input.expected_run_revision,
                vec![
                    NewRunEvent {
                        occurred_at: now,
                        kind: RunEventKind::StateChanged {
                            from,
                            to: RunState::AwaitingApproval,
                        },
                    },
                    NewRunEvent {
                        occurred_at: now,
                        kind: RunEventKind::ApprovalRequested {
                            approval_id: approval.id.clone(),
                        },
                    },
                ],
                &command,
            )
            .await?)
    }

    /// Applies a single decision and resumes the run only when granted.
    ///
    /// # Errors
    ///
    /// Returns [`ApplicationError`] when the approval is stale, expired, already
    /// decided, missing, or cannot be committed atomically with its run.
    pub async fn decide(
        &self,
        id: &ApprovalId,
        expected_revision: u64,
        decision: ApprovalDecision,
        idempotency_key: &str,
    ) -> Result<Approval, ApplicationError> {
        let command = mutation_command(
            "decide_approval",
            idempotency_key,
            &[
                id.as_str().into(),
                expected_revision.to_string(),
                approval_decision_key(decision).into(),
            ],
        )?;
        if let Some(outcome) = self.store.resolve_execution_mutation(&command).await? {
            return decided_approval_outcome(outcome);
        }
        let now = self.clock.now();
        let mut approval = self.store.get_approval(id).await?;
        if approval.revision != expected_revision {
            return Err(ApplicationError::Conflict);
        }
        let expired = match approval.decide(decision, now) {
            Ok(()) => false,
            Err(ApprovalError::Expired { .. }) => true,
            Err(error) => return Err(error.into()),
        };

        // A denied or expired checkpoint must not strand its run in
        // awaiting_approval. It becomes paused until a trusted producer creates
        // a new, explicit continuation intent.
        let next_run_state = if decision == ApprovalDecision::Grant && !expired {
            RunState::Running
        } else {
            RunState::Paused
        };
        let mut run = self.store.get_run(&approval.run_id).await?;
        let expected_run_revision = run.revision;
        let from = run.state;
        run.transition(next_run_state, now)?;
        let saved = self
            .store
            .decide_approval(
                approval,
                expected_revision,
                Some((
                    run,
                    expected_run_revision,
                    NewRunEvent {
                        occurred_at: now,
                        kind: RunEventKind::StateChanged {
                            from,
                            to: next_run_state,
                        },
                    },
                )),
                &command,
            )
            .await?;
        if expired {
            return Err(ApprovalError::Expired {
                expires_at: saved.expires_at,
            }
            .into());
        }
        Ok(saved)
    }
}

fn decided_approval_outcome(
    outcome: ExecutionMutationOutcome,
) -> Result<Approval, ApplicationError> {
    let approval = approval_outcome(outcome)?;
    match approval.status {
        ApprovalStatus::Granted | ApprovalStatus::Denied => Ok(approval),
        ApprovalStatus::Expired => Err(ApprovalError::Expired {
            expires_at: approval.expires_at,
        }
        .into()),
        ApprovalStatus::Pending | ApprovalStatus::Cancelled => Err(ApplicationError::Storage(
            "approval decision command returned an invalid lifecycle result".into(),
        )),
    }
}

fn approval_outcome(outcome: ExecutionMutationOutcome) -> Result<Approval, ApplicationError> {
    match outcome {
        ExecutionMutationOutcome::Approval(approval) => Ok(approval),
        ExecutionMutationOutcome::Run(_) => Err(ApplicationError::Storage(
            "execution command returned an incompatible result".into(),
        )),
    }
}

fn approval_scope_key(scope: &ApprovalScope) -> (&'static str, &str) {
    match scope {
        ApprovalScope::Once => ("once", ""),
        ApprovalScope::Run => ("run", ""),
        ApprovalScope::Resource(resource) => ("resource", resource),
    }
}

const fn approval_risk_key(risk: grok_domain::ApprovalRisk) -> &'static str {
    match risk {
        grok_domain::ApprovalRisk::Low => "low",
        grok_domain::ApprovalRisk::Elevated => "elevated",
        grok_domain::ApprovalRisk::High => "high",
        grok_domain::ApprovalRisk::Critical => "critical",
    }
}

const fn approval_decision_key(decision: ApprovalDecision) -> &'static str {
    match decision {
        ApprovalDecision::Grant => "grant",
        ApprovalDecision::Deny => "deny",
    }
}
