use std::sync::Arc;

use grok_domain::{ProjectId, Run, RunEventKind, RunId, RunState, ThreadId, WorkExecutionBackend};

use crate::{
    ApplicationError, Clock, ExecutionMutationOutcome, ExecutionStore, IdGenerator, NewRunEvent,
    mutations::mutation_command,
};

/// Input for creating an agent run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateRun {
    /// Existing local project.
    pub project_id: String,
    /// Existing conversation thread.
    pub thread_id: String,
}

/// Orchestrates the run lifecycle without depending on a transport or database.
pub struct RunService {
    store: Arc<dyn ExecutionStore>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
}

impl RunService {
    /// Creates a run use-case service.
    #[must_use]
    pub fn new(
        store: Arc<dyn ExecutionStore>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        Self { store, clock, ids }
    }

    /// Persists a queued run and its first audit event in one transaction.
    ///
    /// # Errors
    ///
    /// Returns [`ApplicationError`] for invalid identifiers, duplicate commands,
    /// or storage failure.
    pub async fn create(
        &self,
        input: CreateRun,
        idempotency_key: &str,
    ) -> Result<Run, ApplicationError> {
        let command = mutation_command(
            "create_run",
            idempotency_key,
            &[input.project_id.clone(), input.thread_id.clone()],
        )?;
        if let Some(outcome) = self.store.resolve_execution_mutation(&command).await? {
            return run_outcome(outcome);
        }
        let now = self.clock.now();
        let run = Run::queued(
            RunId::new(self.ids.generate("run"))?,
            ProjectId::new(input.project_id)?,
            ThreadId::new(input.thread_id)?,
            now,
        );
        Ok(self
            .store
            .create_run(
                run.clone(),
                NewRunEvent {
                    occurred_at: now,
                    kind: RunEventKind::Created,
                },
                &command,
            )
            .await?)
    }

    /// Persists a queued Work run with one immutable concrete backend.
    ///
    /// This method is intentionally separate from generic Chat run creation so
    /// existing producers cannot acquire tool authority by adding wire fields.
    ///
    /// # Errors
    ///
    /// Returns [`ApplicationError`] for invalid identifiers, conflicting
    /// idempotency, or failure to atomically persist the bound run.
    pub async fn create_work(
        &self,
        input: CreateRun,
        backend: WorkExecutionBackend,
        idempotency_key: &str,
    ) -> Result<Run, ApplicationError> {
        let command = mutation_command(
            "create_work_run",
            idempotency_key,
            &[
                input.project_id.clone(),
                input.thread_id.clone(),
                work_backend_key(backend).into(),
            ],
        )?;
        if let Some(outcome) = self.store.resolve_execution_mutation(&command).await? {
            return run_outcome(outcome);
        }
        let now = self.clock.now();
        let run = Run::queued_work(
            RunId::new(self.ids.generate("run"))?,
            ProjectId::new(input.project_id)?,
            ThreadId::new(input.thread_id)?,
            backend,
            now,
        );
        Ok(self
            .store
            .create_run(
                run.clone(),
                NewRunEvent {
                    occurred_at: now,
                    kind: RunEventKind::Created,
                },
                &command,
            )
            .await?)
    }

    /// Applies a legal state edge using optimistic concurrency.
    ///
    /// # Errors
    ///
    /// Returns [`ApplicationError`] when the run is missing or stale, the edge is
    /// invalid, or the atomic store update fails.
    pub async fn transition(
        &self,
        id: &RunId,
        expected_revision: u64,
        next: RunState,
        idempotency_key: &str,
    ) -> Result<Run, ApplicationError> {
        let command = mutation_command(
            "transition_run",
            idempotency_key,
            &[
                id.as_str().into(),
                expected_revision.to_string(),
                run_state_key(next).into(),
            ],
        )?;
        if let Some(outcome) = self.store.resolve_execution_mutation(&command).await? {
            return run_outcome(outcome);
        }
        let mut run = self.store.get_run(id).await?;
        if run.revision != expected_revision {
            return Err(ApplicationError::Conflict);
        }
        let from = run.state;
        run.transition(next, self.clock.now())?;
        Ok(self
            .store
            .save_run(
                run.clone(),
                expected_revision,
                NewRunEvent {
                    occurred_at: run.updated_at,
                    kind: RunEventKind::StateChanged { from, to: next },
                },
                &command,
            )
            .await?)
    }

    /// Loads ordered audit events after a reconnect cursor.
    ///
    /// # Errors
    ///
    /// Returns [`ApplicationError`] for an invalid limit, missing run, or storage
    /// failure.
    pub async fn events_since(
        &self,
        id: &RunId,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<grok_domain::RunEvent>, ApplicationError> {
        if !(1..=1_000).contains(&limit) {
            return Err(ApplicationError::InvalidInput(
                "event limit must be between 1 and 1000".into(),
            ));
        }
        Ok(self.store.events_since(id, after_sequence, limit).await?)
    }
}

const fn work_backend_key(backend: WorkExecutionBackend) -> &'static str {
    match backend {
        WorkExecutionBackend::HostDirect => "host_direct",
        WorkExecutionBackend::IsolatedGuest => "isolated_guest",
    }
}

fn run_outcome(outcome: ExecutionMutationOutcome) -> Result<Run, ApplicationError> {
    match outcome {
        ExecutionMutationOutcome::Run(run) => Ok(run),
        ExecutionMutationOutcome::Approval(_) => Err(ApplicationError::Storage(
            "execution command returned an incompatible result".into(),
        )),
    }
}

const fn run_state_key(state: RunState) -> &'static str {
    match state {
        RunState::Queued => "queued",
        RunState::Planning => "planning",
        RunState::AwaitingApproval => "awaiting_approval",
        RunState::Running => "running",
        RunState::Paused => "paused",
        RunState::Completed => "completed",
        RunState::Failed => "failed",
        RunState::Cancelled => "cancelled",
        RunState::InterruptedNeedsReview => "interrupted_needs_review",
    }
}
