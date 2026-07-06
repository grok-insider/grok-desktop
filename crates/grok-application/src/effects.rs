use std::sync::Arc;

use grok_domain::{EffectId, EffectKind, Idempotency, RunEventKind, RunId, RunState, SideEffect};

use crate::{ApplicationError, Clock, ExecutionStore, IdGenerator, NewRunEvent};

/// Input for recording a side-effect intent before dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrepareEffect {
    /// Owning active run.
    pub run_id: RunId,
    /// Broad operation class.
    pub kind: EffectKind,
    /// Non-secret target description.
    pub target: String,
    /// Retry guarantee.
    pub idempotency: Idempotency,
}

/// Enforces persist-before-dispatch and explicit interrupted recovery.
pub struct SideEffectService {
    store: Arc<dyn ExecutionStore>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
}

impl SideEffectService {
    /// Creates a side-effect lifecycle service.
    #[must_use]
    pub fn new(
        store: Arc<dyn ExecutionStore>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        Self { store, clock, ids }
    }

    /// Persists intent and its audit event before external dispatch is allowed.
    ///
    /// # Errors
    ///
    /// Returns [`ApplicationError`] when the run is not active, input is invalid,
    /// or durable intent persistence fails.
    pub async fn prepare(&self, input: PrepareEffect) -> Result<SideEffect, ApplicationError> {
        let run = self.store.get_run(&input.run_id).await?;
        if run.state != RunState::Running {
            return Err(ApplicationError::InvalidState(
                "side effects require a running run".into(),
            ));
        }
        if input.target.trim().is_empty() {
            return Err(ApplicationError::InvalidInput(
                "effect target cannot be empty".into(),
            ));
        }
        let now = self.clock.now();
        let effect = SideEffect::prepare(
            EffectId::new(self.ids.generate("effect"))?,
            input.run_id,
            input.kind,
            input.target,
            input.idempotency,
            now,
        );
        self.store
            .create_effect(
                effect.clone(),
                NewRunEvent {
                    occurred_at: now,
                    kind: RunEventKind::EffectPrepared {
                        effect_id: effect.id.clone(),
                    },
                },
            )
            .await?;
        Ok(effect)
    }

    /// Records that dispatch crossed the external boundary.
    ///
    /// # Errors
    ///
    /// Returns [`ApplicationError`] for missing or stale effects, invalid state,
    /// or storage failure.
    pub async fn start(
        &self,
        id: &EffectId,
        expected_revision: u64,
    ) -> Result<SideEffect, ApplicationError> {
        let mut effect = self.store.get_effect(id).await?;
        if effect.revision != expected_revision {
            return Err(ApplicationError::Conflict);
        }
        effect.start(self.clock.now())?;
        self.store
            .save_effect(effect.clone(), expected_revision)
            .await?;
        Ok(effect)
    }

    /// Records a known external result.
    ///
    /// # Errors
    ///
    /// Returns [`ApplicationError`] for missing or stale effects, invalid state,
    /// or storage failure.
    pub async fn finish(
        &self,
        id: &EffectId,
        expected_revision: u64,
        succeeded: bool,
    ) -> Result<SideEffect, ApplicationError> {
        let mut effect = self.store.get_effect(id).await?;
        if effect.revision != expected_revision {
            return Err(ApplicationError::Conflict);
        }
        effect.finish(succeeded, self.clock.now())?;
        self.store
            .save_effect(effect.clone(), expected_revision)
            .await?;
        Ok(effect)
    }

    /// Marks both an uncertain effect and its owning run as requiring review.
    ///
    /// # Errors
    ///
    /// Returns [`ApplicationError`] when the effect or run is missing, stale, in
    /// an incompatible state, or cannot be updated atomically.
    pub async fn interrupt(
        &self,
        id: &EffectId,
        expected_revision: u64,
    ) -> Result<SideEffect, ApplicationError> {
        let now = self.clock.now();
        let mut effect = self.store.get_effect(id).await?;
        if effect.revision != expected_revision {
            return Err(ApplicationError::Conflict);
        }
        effect.interrupt(now)?;
        let mut run = self.store.get_run(&effect.run_id).await?;
        let expected_run_revision = run.revision;
        let from = run.state;
        run.transition(RunState::InterruptedNeedsReview, now)?;
        self.store
            .interrupt_effect(
                effect.clone(),
                expected_revision,
                run,
                expected_run_revision,
                vec![
                    NewRunEvent {
                        occurred_at: now,
                        kind: RunEventKind::EffectNeedsReview {
                            effect_id: effect.id.clone(),
                        },
                    },
                    NewRunEvent {
                        occurred_at: now,
                        kind: RunEventKind::StateChanged {
                            from,
                            to: RunState::InterruptedNeedsReview,
                        },
                    },
                ],
            )
            .await?;
        Ok(effect)
    }
}
