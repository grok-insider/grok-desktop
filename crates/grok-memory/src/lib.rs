//! In-memory adapters for deterministic tests and development startup.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Mutex as StdMutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use grok_application::{
    AccountState, ArtifactContentReadyResult, ArtifactImportFailureCode, ArtifactImportPlan,
    ArtifactImportReservation, ArtifactImportState, ArtifactOpenFailureCode, ArtifactOpenPlan,
    ArtifactOpenReservation, ArtifactOpenState, ArtifactQuotaUsage, ArtifactRemovalPlan,
    ArtifactRemovalReservation, ArtifactRemovalState, ArtifactRetentionRecord,
    ArtifactRetentionState, ArtifactStore, AutomationOccurrenceClaimAttempt,
    AutomationOccurrenceClaimCompletion, AutomationScheduleCandidate,
    AutomationScheduleEvaluationCommit, AutomationScheduleEvaluationResult,
    AutomationSchedulerJournalStatus, AutomationSchedulerLeaseAcquisition,
    AutomationSchedulerRecoverySummary, AutomationSchedulerStore, CancelConversationTurnCommit,
    ChatModelPreferenceStore, ClaimAutomationOccurrence, Clock, ConversationForkCommandResolution,
    ConversationForkDelivery, ConversationForkDeliveryState, ConversationForkMetadata,
    ConversationForkPlan, ConversationForkReservation, ConversationForkSnapshot,
    ConversationInheritedAssistantOutcome, ConversationThreadCredentialBinding,
    ConversationTurnEventPage, ConversationTurnReservation, ConversationTurnReservationSource,
    ConversationTurnSnapshot, ConversationTurnStore, CredentialMutationReservation,
    CredentialMutationStore, DatabaseKey, DesktopPreferencesStore, ExecutionMutationOutcome,
    ExecutionStore, IdGenerator, KeyProviderError, MAX_ARTIFACT_FILE_BYTES,
    MAX_AUTOMATION_SCHEDULER_EVALUATION_OCCURRENCES, MAX_AUTOMATION_SCHEDULER_RECOVERY_BATCH,
    MAX_AUTOMATION_SCHEDULER_TICK_DEFINITIONS, MAX_CONVERSATION_CONTEXT_BYTES,
    MAX_CONVERSATION_CONTEXT_MESSAGES, MAX_CONVERSATION_EVENT_BATCH,
    MAX_CONVERSATION_FORK_DELIVERY_ALIASES, MAX_CONVERSATION_FORK_DIRECT_CHILDREN,
    MAX_CONVERSATION_FORK_FAMILY_THREADS, MAX_CONVERSATION_FORK_INHERITED_OUTCOMES,
    MAX_GLOBAL_ARTIFACT_BYTES, MAX_PROJECT_ARTIFACT_BYTES, MAX_PROJECT_ARTIFACT_COUNT,
    MutationCommand, NewRunEvent, PrivilegedDispatchAttempt, PrivilegedOperationStore,
    PrivilegedPreparation, PrivilegedRecoveryCandidate, ProviderStartCommit, SecretName,
    SecretValue, SecretVault, SecureKeyProvider, StoreError, TerminalTurnCommit, VaultError,
    WorkspaceSearchHit, WorkspaceSearchKind, WorkspaceStore, automation_occurrence_is_active,
    conversation_fork_metadata_is_within_bounds,
};
use grok_domain::{
    Approval, ApprovalId, Artifact, ArtifactId, ArtifactState, ArtifactVersion, Automation,
    AutomationExecutionSnapshot, AutomationHistoryEntry, AutomationHistoryStatus, AutomationId,
    AutomationOccurrence, AutomationOccurrenceId, AutomationOccurrenceState,
    AutomationScheduleCursor, AutomationSchedulerLease, AutomationSchedulerLeaseToken,
    AutomationSchedulerOwnerId, AutomationState, ChatModelPreference, ConversationForkKind,
    ConversationMessageDerivation, ConversationMessageDerivationKind, ConversationThreadOrigin,
    ConversationTurn, ConversationTurnEvent, ConversationTurnEventKind, ConversationTurnEventLog,
    ConversationTurnId, ConversationTurnLineage, ConversationTurnOrigin, ConversationTurnState,
    DesktopPreferences, EffectId, EffectKind, Idempotency,
    MAX_AUTOMATION_OCCURRENCE_CLAIM_ATTEMPTS, MAX_AUTOMATION_SCHEDULE_DECISIONS,
    MAX_AUTOMATION_SCHEDULER_LEASE_MS, MAX_CONVERSATION_TEXT_CHUNK_BYTES, Message, MessageId,
    MessageRole, MessageState, MissedRunPolicy, OverlapPolicy, PrivilegedOperation,
    PrivilegedOperationId, PrivilegedOperationState, Project, ProjectId, ProjectState, Run,
    RunEvent, RunEventKind, RunId, RunState, SideEffect, Thread, ThreadId, ThreadState, UnixMillis,
};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use zeroize::Zeroize;

/// Ephemeral key source for tests and explicitly configured debug processes.
///
/// Production builds must replace this with a platform-vault adapter.
pub struct EphemeralKeyProvider([u8; 32]);

impl EphemeralKeyProvider {
    /// Creates a provider from caller-owned test key material.
    #[must_use]
    pub const fn new(key: [u8; 32]) -> Self {
        Self(key)
    }

    /// Generates a process-local key suitable for temporary databases.
    #[must_use]
    pub fn random() -> Self {
        let mut key = [0; 32];
        key[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        key[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        Self(key)
    }
}

impl std::fmt::Debug for EphemeralKeyProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("EphemeralKeyProvider([REDACTED])")
    }
}

impl Drop for EphemeralKeyProvider {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl SecureKeyProvider for EphemeralKeyProvider {
    fn database_key(&self) -> Result<DatabaseKey, KeyProviderError> {
        DatabaseKey::from_slice(&self.0)
    }
}

#[derive(Debug, Default)]
struct State {
    runs: HashMap<RunId, Run>,
    approvals: HashMap<ApprovalId, Approval>,
    effects: HashMap<EffectId, SideEffect>,
    events: HashMap<RunId, Vec<RunEvent>>,
    projects: HashMap<ProjectId, Project>,
    threads: HashMap<ThreadId, Thread>,
    messages: HashMap<MessageId, Message>,
    conversation_turns: HashMap<ConversationTurnId, ConversationTurn>,
    conversation_lineages: HashMap<ConversationTurnId, ConversationTurnLineage>,
    conversation_thread_bindings: HashMap<ThreadId, String>,
    conversation_turn_keys: HashMap<String, ConversationTurnId>,
    conversation_contexts: HashMap<ConversationTurnId, Vec<Message>>,
    conversation_events: HashMap<ConversationTurnId, Vec<ConversationTurnEvent>>,
    conversation_fork_commands: HashMap<(String, String), ConversationForkCommandRecord>,
    conversation_fork_deliveries: HashMap<ThreadId, ConversationForkDeliveryRecord>,
    conversation_fork_delivery_ack_commands:
        HashMap<(String, String), ConversationForkDeliveryAckCommandRecord>,
    conversation_inherited_outcomes: HashMap<MessageId, ConversationTurnId>,
    conversation_cancel_commands: HashMap<(String, String), ConversationCancelCommandRecord>,
    artifacts: HashMap<ArtifactId, Artifact>,
    artifact_versions: HashMap<(ArtifactId, u32), ArtifactVersion>,
    artifact_retention: HashMap<(ArtifactId, u32), ArtifactRetentionRecord>,
    artifact_import_commands: HashMap<(String, String), ArtifactImportCommandRecord>,
    artifact_import_artifacts: HashMap<ArtifactId, (String, String)>,
    active_artifact_import: Option<(String, String)>,
    artifact_open_commands: HashMap<(String, String), ArtifactOpenCommandRecord>,
    active_artifact_open: Option<(String, String)>,
    artifact_removal_commands: HashMap<(String, String), ArtifactRemovalCommandRecord>,
    artifact_removal_artifacts: HashMap<ArtifactId, (String, String)>,
    active_artifact_removal: Option<(String, String)>,
    automations: HashMap<AutomationId, Automation>,
    automation_history: HashMap<AutomationId, Vec<AutomationHistoryEntry>>,
    automation_scheduler_lease: Option<AutomationSchedulerLease>,
    automation_schedule_cursors: HashMap<AutomationId, AutomationScheduleCursor>,
    automation_schedule_evaluation_commands:
        HashMap<(String, String), AutomationScheduleEvaluationCommandRecord>,
    automation_occurrences: HashMap<AutomationOccurrenceId, AutomationOccurrence>,
    automation_occurrence_claim_attempts:
        HashMap<AutomationOccurrenceId, Vec<AutomationOccurrenceClaimAttempt>>,
    automation_occurrence_claim_commands:
        HashMap<(String, String), AutomationOccurrenceClaimCommandRecord>,
    credential_commands: HashMap<(String, String), CredentialCommandRecord>,
    execution_commands: HashMap<(String, String), ExecutionCommandRecord>,
    workspace_commands: HashMap<(String, String), CommandRecord>,
    desktop_preferences: DesktopPreferences,
    desktop_preference_commands: HashMap<(String, String), DesktopPreferenceCommandRecord>,
    chat_model_preference: ChatModelPreference,
    chat_model_preference_commands: HashMap<(String, String), ChatModelPreferenceCommandRecord>,
    privileged_operations: HashMap<PrivilegedOperationId, PrivilegedOperation>,
    privileged_operation_keys: HashMap<(String, String), PrivilegedOperationId>,
    privileged_operation_payloads: HashMap<PrivilegedOperationId, Vec<u8>>,
    privileged_operation_attempts:
        HashMap<PrivilegedOperationId, Vec<StoredPrivilegedDispatchAttempt>>,
    privileged_transport_ids: HashSet<String>,
}

#[derive(Debug, Clone)]
struct StoredPrivilegedDispatchAttempt {
    attempt: PrivilegedDispatchAttempt,
    completed_at: Option<UnixMillis>,
}

#[derive(Debug, Clone)]
struct ExecutionCommandRecord {
    fingerprint: [u8; 32],
    outcome: ExecutionMutationOutcome,
}

#[derive(Debug, Clone)]
struct ArtifactImportCommandRecord {
    fingerprint: [u8; 32],
    plan: ArtifactImportPlan,
}

#[derive(Debug, Clone)]
struct ArtifactOpenCommandRecord {
    fingerprint: [u8; 32],
    plan: ArtifactOpenPlan,
}

#[derive(Debug, Clone)]
struct ArtifactRemovalCommandRecord {
    fingerprint: [u8; 32],
    plan: ArtifactRemovalPlan,
}

#[derive(Debug, Clone)]
struct ConversationCancelCommandRecord {
    fingerprint: [u8; 32],
    turn_id: ConversationTurnId,
    outcome_state: ConversationTurnState,
    outcome_revision: u64,
}

#[derive(Debug, Clone)]
struct ConversationForkCommandRecord {
    fingerprint: [u8; 32],
    child_thread_id: ThreadId,
    started_turn_id: Option<ConversationTurnId>,
    canonical: bool,
}

#[derive(Debug, Clone)]
struct ConversationForkDeliveryRecord {
    scope: String,
    request_fingerprint: [u8; 32],
    state: ConversationForkDeliveryState,
    revision: u64,
}

#[derive(Debug, Clone)]
struct ConversationForkDeliveryAckCommandRecord {
    fingerprint: [u8; 32],
    child_thread_id: ThreadId,
    expected_revision: u64,
    outcome_revision: u64,
}

#[derive(Debug, Clone)]
struct CredentialCommandRecord {
    fingerprint: [u8; 32],
    outcome: Option<AccountState>,
}

#[derive(Debug, Clone)]
struct DesktopPreferenceCommandRecord {
    fingerprint: [u8; 32],
    outcome: DesktopPreferences,
}

#[derive(Debug, Clone)]
struct ChatModelPreferenceCommandRecord {
    fingerprint: [u8; 32],
    outcome: ChatModelPreference,
}

/// Process-local credential vault for tests and explicit ephemeral debug mode.
#[derive(Debug, Default)]
pub struct InMemorySecretVault {
    entries: StdMutex<HashMap<SecretName, SecretValue>>,
}

impl InMemorySecretVault {
    /// Creates an empty non-persistent vault.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretVault for InMemorySecretVault {
    fn get(&self, name: &SecretName) -> Result<SecretValue, VaultError> {
        self.entries
            .lock()
            .map_err(|_| VaultError::Internal)?
            .get(name)
            .cloned()
            .ok_or(VaultError::NotFound)
    }

    fn set(&self, name: &SecretName, value: &SecretValue) -> Result<(), VaultError> {
        self.entries
            .lock()
            .map_err(|_| VaultError::Internal)?
            .insert(name.clone(), value.clone());
        Ok(())
    }

    fn delete(&self, name: &SecretName) -> Result<(), VaultError> {
        self.entries
            .lock()
            .map_err(|_| VaultError::Internal)?
            .remove(name);
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct CommandRecord {
    fingerprint: [u8; 32],
    entity_id: String,
}

#[derive(Debug, Clone)]
struct AutomationScheduleEvaluationCommandRecord {
    fingerprint: [u8; 32],
    result: AutomationScheduleEvaluationResult,
}

#[derive(Debug, Clone)]
struct AutomationOccurrenceClaimCommandRecord {
    fingerprint: [u8; 32],
    occurrence_id: AutomationOccurrenceId,
    result: AutomationOccurrence,
}

const AUTOMATION_EVALUATION_SCOPE: &str = "automation_scheduler_evaluate_v1";
const AUTOMATION_CLAIM_SCOPE: &str = "automation_scheduler_claim_v1";
const MAX_AUTOMATION_OCCURRENCE_PAGE_SIZE: usize = 100;
const AUTOMATION_SKIPPED_MISSED_SUMMARY: &str = "Skipped by missed-run policy.";
const AUTOMATION_SKIPPED_OVERLAP_SUMMARY: &str = "Skipped by overlap policy.";

/// Transactional in-memory implementation of the execution aggregate store.
#[derive(Debug, Default)]
pub struct InMemoryExecutionStore {
    state: Mutex<State>,
}

impl InMemoryExecutionStore {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

fn append_events(state: &mut State, run_id: &RunId, events: Vec<NewRunEvent>) {
    let stream = state.events.entry(run_id.clone()).or_default();
    for event in events {
        let sequence = u64::try_from(stream.len())
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        stream.push(RunEvent {
            sequence,
            run_id: run_id.clone(),
            occurred_at: event.occurred_at,
            kind: event.kind,
        });
    }
}

#[async_trait]
impl ExecutionStore for InMemoryExecutionStore {
    async fn resolve_execution_mutation(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<ExecutionMutationOutcome>, StoreError> {
        let state = self.state.lock().await;
        prior_execution_command(&state, command)
    }

    async fn create_run(
        &self,
        run: Run,
        event: NewRunEvent,
        command: &MutationCommand,
    ) -> Result<Run, StoreError> {
        let mut state = self.state.lock().await;
        if let Some(outcome) = prior_execution_command(&state, command)? {
            return run_outcome(outcome);
        }
        if state.runs.contains_key(&run.id) {
            return Err(StoreError::Conflict);
        }
        let id = run.id.clone();
        state.runs.insert(id.clone(), run.clone());
        append_events(&mut state, &id, vec![event]);
        record_execution_command(
            &mut state,
            command,
            ExecutionMutationOutcome::Run(run.clone()),
        );
        Ok(run)
    }

    async fn get_run(&self, id: &RunId) -> Result<Run, StoreError> {
        self.state
            .lock()
            .await
            .runs
            .get(id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn save_run(
        &self,
        run: Run,
        expected_revision: u64,
        event: NewRunEvent,
        command: &MutationCommand,
    ) -> Result<Run, StoreError> {
        let mut state = self.state.lock().await;
        if let Some(outcome) = prior_execution_command(&state, command)? {
            return run_outcome(outcome);
        }
        let current = state.runs.get(&run.id).ok_or(StoreError::NotFound)?;
        if current.revision != expected_revision || run.revision != expected_revision + 1 {
            return Err(StoreError::Conflict);
        }
        let id = run.id.clone();
        state.runs.insert(id.clone(), run.clone());
        append_events(&mut state, &id, vec![event]);
        record_execution_command(
            &mut state,
            command,
            ExecutionMutationOutcome::Run(run.clone()),
        );
        Ok(run)
    }

    async fn create_approval(
        &self,
        approval: Approval,
        run: Run,
        expected_run_revision: u64,
        events: Vec<NewRunEvent>,
        command: &MutationCommand,
    ) -> Result<Approval, StoreError> {
        let mut state = self.state.lock().await;
        if let Some(outcome) = prior_execution_command(&state, command)? {
            return approval_outcome(outcome);
        }
        let current = state.runs.get(&run.id).ok_or(StoreError::NotFound)?;
        if current.revision != expected_run_revision || run.revision != expected_run_revision + 1 {
            return Err(StoreError::Conflict);
        }
        if state.approvals.contains_key(&approval.id) || approval.run_id != run.id {
            return Err(StoreError::Conflict);
        }
        let run_id = run.id.clone();
        state.runs.insert(run_id.clone(), run);
        state
            .approvals
            .insert(approval.id.clone(), approval.clone());
        append_events(&mut state, &run_id, events);
        record_execution_command(
            &mut state,
            command,
            ExecutionMutationOutcome::Approval(approval.clone()),
        );
        Ok(approval)
    }

    async fn get_approval(&self, id: &ApprovalId) -> Result<Approval, StoreError> {
        self.state
            .lock()
            .await
            .approvals
            .get(id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn decide_approval(
        &self,
        approval: Approval,
        expected_approval_revision: u64,
        run_update: Option<(Run, u64, NewRunEvent)>,
        command: &MutationCommand,
    ) -> Result<Approval, StoreError> {
        let mut state = self.state.lock().await;
        if let Some(outcome) = prior_execution_command(&state, command)? {
            return approval_outcome(outcome);
        }
        let current = state
            .approvals
            .get(&approval.id)
            .ok_or(StoreError::NotFound)?;
        if current.revision != expected_approval_revision
            || approval.revision != expected_approval_revision + 1
        {
            return Err(StoreError::Conflict);
        }
        if let Some((run, expected_run_revision, _)) = &run_update {
            let current_run = state.runs.get(&run.id).ok_or(StoreError::NotFound)?;
            if current_run.revision != *expected_run_revision
                || run.revision != expected_run_revision + 1
                || run.id != approval.run_id
            {
                return Err(StoreError::Conflict);
            }
        }
        state
            .approvals
            .insert(approval.id.clone(), approval.clone());
        if let Some((run, _, event)) = run_update {
            let run_id = run.id.clone();
            state.runs.insert(run_id.clone(), run);
            append_events(&mut state, &run_id, vec![event]);
        }
        record_execution_command(
            &mut state,
            command,
            ExecutionMutationOutcome::Approval(approval.clone()),
        );
        Ok(approval)
    }

    async fn create_effect(
        &self,
        effect: SideEffect,
        event: NewRunEvent,
    ) -> Result<(), StoreError> {
        let mut state = self.state.lock().await;
        if state.effects.contains_key(&effect.id) {
            return Err(StoreError::Conflict);
        }
        if !state.runs.contains_key(&effect.run_id) {
            return Err(StoreError::NotFound);
        }
        let run_id = effect.run_id.clone();
        state.effects.insert(effect.id.clone(), effect);
        append_events(&mut state, &run_id, vec![event]);
        Ok(())
    }

    async fn get_effect(&self, id: &EffectId) -> Result<SideEffect, StoreError> {
        self.state
            .lock()
            .await
            .effects
            .get(id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn save_effect(
        &self,
        effect: SideEffect,
        expected_revision: u64,
    ) -> Result<(), StoreError> {
        let mut state = self.state.lock().await;
        let current = state.effects.get(&effect.id).ok_or(StoreError::NotFound)?;
        if current.revision != expected_revision || effect.revision != expected_revision + 1 {
            return Err(StoreError::Conflict);
        }
        state.effects.insert(effect.id.clone(), effect);
        Ok(())
    }

    async fn interrupt_effect(
        &self,
        effect: SideEffect,
        expected_effect_revision: u64,
        run: Run,
        expected_run_revision: u64,
        events: Vec<NewRunEvent>,
    ) -> Result<(), StoreError> {
        let mut state = self.state.lock().await;
        let current_effect = state.effects.get(&effect.id).ok_or(StoreError::NotFound)?;
        let current_run = state.runs.get(&run.id).ok_or(StoreError::NotFound)?;
        if current_effect.revision != expected_effect_revision
            || effect.revision != expected_effect_revision + 1
            || current_run.revision != expected_run_revision
            || run.revision != expected_run_revision + 1
            || effect.run_id != run.id
        {
            return Err(StoreError::Conflict);
        }
        let run_id = run.id.clone();
        state.effects.insert(effect.id.clone(), effect);
        state.runs.insert(run_id.clone(), run);
        append_events(&mut state, &run_id, events);
        Ok(())
    }

    async fn events_since(
        &self,
        run_id: &RunId,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<RunEvent>, StoreError> {
        let state = self.state.lock().await;
        if !state.runs.contains_key(run_id) {
            return Err(StoreError::NotFound);
        }
        Ok(state
            .events
            .get(run_id)
            .into_iter()
            .flatten()
            .filter(|event| event.sequence > after_sequence)
            .take(limit)
            .cloned()
            .collect())
    }
}

#[async_trait]
impl ConversationTurnStore for InMemoryExecutionStore {
    #[allow(clippy::too_many_lines)]
    async fn reserve_turn(
        &self,
        turn: ConversationTurn,
        lineage: ConversationTurnLineage,
        source: ConversationTurnReservationSource,
        user_message: Message,
        run: Run,
        event: NewRunEvent,
        turn_event: ConversationTurnEventKind,
    ) -> Result<ConversationTurnReservation, StoreError> {
        if !is_canonical_reservation_input(&turn, &user_message, &run, &event)
            || ConversationTurnLineage::restore(lineage.clone(), &turn.id).is_err()
            || !reservation_source_matches_lineage(&source, &lineage)
            || turn_event != ConversationTurnEventKind::Created
        {
            return Err(StoreError::Conflict);
        }
        let mut state = self.state.lock().await;
        if let Some(id) = state
            .conversation_turn_keys
            .get(&turn.idempotency_key)
            .cloned()
        {
            let existing = state
                .conversation_turns
                .get(&id)
                .ok_or_else(|| StoreError::Internal("conversation command lost its turn".into()))?;
            if existing.request_fingerprint != turn.request_fingerprint {
                return Err(StoreError::Conflict);
            }
            if state.conversation_lineages.get(&id) != Some(&lineage) {
                return Err(StoreError::Conflict);
            }
            return Ok(ConversationTurnReservation {
                snapshot: conversation_snapshot(&state, existing)?,
                context: state
                    .conversation_contexts
                    .get(&id)
                    .cloned()
                    .ok_or_else(|| {
                        StoreError::Internal("conversation turn lost its immutable context".into())
                    })?,
                created: false,
            });
        }
        if state.conversation_turns.values().any(|existing| {
            existing.thread_id == turn.thread_id
                && matches!(
                    existing.state,
                    ConversationTurnState::Reserved | ConversationTurnState::ProviderStarted
                )
        }) {
            return Err(StoreError::Conflict);
        }
        let thread = state
            .threads
            .get(&turn.thread_id)
            .ok_or(StoreError::NotFound)?;
        let project = state
            .projects
            .get(&thread.project_id)
            .ok_or(StoreError::NotFound)?;
        if thread.state != ThreadState::Open
            || project.state != ProjectState::Active
            || turn.project_id != thread.project_id
            || user_message.thread_id != turn.thread_id
            || user_message.id != turn.user_message_id
            || user_message.role != MessageRole::User
            || run.id != turn.run_id
            || run.thread_id != turn.thread_id
            || run.project_id != turn.project_id
            || state.messages.contains_key(&user_message.id)
            || state.runs.contains_key(&run.id)
            || state.conversation_turns.contains_key(&turn.id)
        {
            return Err(StoreError::Conflict);
        }
        let credential_binding = lineage
            .credential_binding_id
            .as_deref()
            .ok_or(StoreError::Conflict)?;
        let bind_thread = match state.conversation_thread_bindings.get(&turn.thread_id) {
            Some(existing) if existing == credential_binding => false,
            Some(_) => return Err(StoreError::Conflict),
            None if matches!(&source, ConversationTurnReservationSource::Retry { .. })
                || state
                    .conversation_turns
                    .values()
                    .any(|existing| existing.thread_id == turn.thread_id) =>
            {
                return Err(StoreError::Conflict);
            }
            None => true,
        };
        let (user_message, context) = match source {
            ConversationTurnReservationSource::CurrentThread => {
                capture_conversation_context(&state, &turn, user_message)?
            }
            ConversationTurnReservationSource::Retry {
                source_turn_id,
                expected_source_revision,
            } => capture_retry_context(
                &state,
                &turn,
                &lineage,
                &source_turn_id,
                expected_source_revision,
                user_message,
            )?,
        };

        let prospective = ConversationTurnSnapshot {
            turn: turn.clone(),
            user_message: user_message.clone(),
            assistant_message: None,
            run: run.clone(),
            effect: None,
            lineage: lineage.clone(),
        };
        if !is_canonical_conversation_snapshot(&prospective) {
            return Err(StoreError::Conflict);
        }

        let mut turn_event_log = ConversationTurnEventLog::new(turn.id.clone());
        let persisted_turn_event = turn_event_log
            .append_kind(turn_event)
            .map_err(|_| StoreError::Conflict)?;
        turn_event_log
            .validate_snapshot(&turn, None)
            .map_err(|_| StoreError::Conflict)?;

        let turn_id = turn.id.clone();
        let run_id = run.id.clone();
        if bind_thread {
            state
                .conversation_thread_bindings
                .insert(turn.thread_id.clone(), credential_binding.to_owned());
        }
        state.messages.insert(user_message.id.clone(), user_message);
        state.runs.insert(run_id.clone(), run);
        append_events(&mut state, &run_id, vec![event]);
        state
            .conversation_turn_keys
            .insert(turn.idempotency_key.clone(), turn_id.clone());
        state.conversation_lineages.insert(turn_id.clone(), lineage);
        state
            .conversation_contexts
            .insert(turn_id.clone(), context.clone());
        state
            .conversation_events
            .insert(turn_id.clone(), vec![persisted_turn_event]);
        state.conversation_turns.insert(turn_id.clone(), turn);
        let persisted_turn = state.conversation_turns.get(&turn_id).ok_or_else(|| {
            StoreError::Internal("conversation reservation was not stored".into())
        })?;
        Ok(ConversationTurnReservation {
            snapshot: conversation_snapshot(&state, persisted_turn)?,
            context,
            created: true,
        })
    }

    async fn load_turn_by_command(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<ConversationTurnSnapshot>, StoreError> {
        if !matches!(
            command.scope.as_str(),
            "execute_conversation_turn"
                | "retry_conversation_turn"
                | CONVERSATION_EDIT_BRANCH_COMMAND_SCOPE
                | CONVERSATION_REGENERATE_COMMAND_SCOPE
        ) {
            return Ok(None);
        }
        let state = self.state.lock().await;
        let Some(id) = state.conversation_turn_keys.get(&command.key) else {
            return Ok(None);
        };
        let turn = state
            .conversation_turns
            .get(id)
            .ok_or_else(|| StoreError::Internal("conversation command lost its turn".into()))?;
        if turn.request_fingerprint != command.fingerprint {
            return Err(StoreError::Conflict);
        }
        let lineage = state
            .conversation_lineages
            .get(id)
            .ok_or_else(|| StoreError::Internal("conversation command lost its lineage".into()))?;
        let scope_matches = matches!(
            (&lineage.origin, command.scope.as_str()),
            (
                ConversationTurnOrigin::Original,
                "execute_conversation_turn"
            ) | (
                ConversationTurnOrigin::Retry { .. },
                "retry_conversation_turn"
            ) | (
                ConversationTurnOrigin::EditAndBranch { .. },
                CONVERSATION_EDIT_BRANCH_COMMAND_SCOPE
            ) | (
                ConversationTurnOrigin::Regenerate { .. },
                CONVERSATION_REGENERATE_COMMAND_SCOPE
            )
        );
        if !scope_matches {
            return Err(StoreError::Conflict);
        }
        Ok(Some(conversation_snapshot(&state, turn)?))
    }

    async fn load_turn(
        &self,
        id: &ConversationTurnId,
    ) -> Result<Option<ConversationTurnSnapshot>, StoreError> {
        let state = self.state.lock().await;
        state
            .conversation_turns
            .get(id)
            .map(|turn| conversation_snapshot(&state, turn))
            .transpose()
    }

    async fn load_turn_context(&self, id: &ConversationTurnId) -> Result<Vec<Message>, StoreError> {
        self.state
            .lock()
            .await
            .conversation_contexts
            .get(id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn commit_provider_start(
        &self,
        commit: ProviderStartCommit,
    ) -> Result<ConversationTurnSnapshot, StoreError> {
        let mut state = self.state.lock().await;
        let current = state
            .conversation_turns
            .get(&commit.turn.id)
            .ok_or(StoreError::NotFound)
            .and_then(|turn| conversation_snapshot(&state, turn))?;
        if state.effects.contains_key(&commit.effect.id)
            || !is_exact_provider_start(&current, &commit)
        {
            return Err(StoreError::Conflict);
        }

        let prospective = ConversationTurnSnapshot {
            turn: commit.turn.clone(),
            user_message: current.user_message,
            assistant_message: None,
            run: commit.run.clone(),
            effect: Some(commit.effect.clone()),
            lineage: current.lineage,
        };
        if !is_canonical_conversation_snapshot(&prospective) {
            return Err(StoreError::Conflict);
        }

        let mut turn_event_log = conversation_event_log(&state, &commit.turn.id)?;
        let persisted_turn_event = turn_event_log
            .append_kind(commit.turn_event.clone())
            .map_err(|_| StoreError::Conflict)?;
        turn_event_log
            .validate_snapshot(&commit.turn, None)
            .map_err(|_| StoreError::Conflict)?;

        let turn_id = commit.turn.id.clone();
        let run_id = commit.run.id.clone();
        state
            .conversation_turns
            .insert(turn_id.clone(), commit.turn);
        state.runs.insert(run_id.clone(), commit.run);
        state
            .effects
            .insert(commit.effect.id.clone(), commit.effect);
        state
            .conversation_events
            .get_mut(&turn_id)
            .ok_or_else(|| StoreError::Internal("conversation turn lost its event log".into()))?
            .push(persisted_turn_event);
        append_events(&mut state, &run_id, commit.events);
        let turn = state
            .conversation_turns
            .get(&turn_id)
            .ok_or(StoreError::NotFound)?;
        conversation_snapshot(&state, turn)
    }

    async fn commit_terminal(
        &self,
        commit: TerminalTurnCommit,
    ) -> Result<ConversationTurnSnapshot, StoreError> {
        let mut state = self.state.lock().await;
        commit_conversation_terminal(&mut state, commit)
    }

    async fn commit_cancellation(
        &self,
        commit: CancelConversationTurnCommit,
    ) -> Result<ConversationTurnSnapshot, StoreError> {
        let mut state = self.state.lock().await;
        commit_conversation_cancellation(&mut state, commit, "cancel_conversation_turn")
    }

    async fn commit_dispatch_exit_reconciliation(
        &self,
        commit: CancelConversationTurnCommit,
    ) -> Result<ConversationTurnSnapshot, StoreError> {
        let mut state = self.state.lock().await;
        commit_conversation_cancellation(&mut state, commit, "reconcile_conversation_dispatch_exit")
    }

    async fn append_turn_text(
        &self,
        turn_id: &ConversationTurnId,
        expected_turn_revision: u64,
        start_utf8_offset: u64,
        text: String,
    ) -> Result<Vec<ConversationTurnEvent>, StoreError> {
        if text.is_empty() {
            return Err(StoreError::Conflict);
        }
        let mut state = self.state.lock().await;
        let turn = state
            .conversation_turns
            .get(turn_id)
            .ok_or(StoreError::NotFound)?;
        if turn.revision != expected_turn_revision
            || turn.state != ConversationTurnState::ProviderStarted
        {
            return Err(StoreError::Conflict);
        }
        let current_log = conversation_event_log(&state, turn_id)?;
        current_log
            .validate_snapshot(turn, None)
            .map_err(|_| StoreError::Conflict)?;
        let current_offset = current_log.next_utf8_offset();
        if start_utf8_offset < current_offset {
            return replayed_text_append(
                state.conversation_events.get(turn_id).ok_or_else(|| {
                    StoreError::Internal("conversation turn lost its event log".into())
                })?,
                start_utf8_offset,
                &text,
            )
            .ok_or(StoreError::Conflict);
        }
        if start_utf8_offset != current_offset {
            return Err(StoreError::Conflict);
        }

        let mut prospective_log = current_log;
        let mut appended = Vec::new();
        for chunk in split_conversation_text(&text) {
            let event = prospective_log
                .append_kind(ConversationTurnEventKind::TextAppended {
                    start_utf8_offset: prospective_log.next_utf8_offset(),
                    text: chunk.to_owned(),
                })
                .map_err(|_| StoreError::Conflict)?;
            appended.push(event);
        }
        if appended.is_empty() {
            return Err(StoreError::Conflict);
        }
        state
            .conversation_events
            .get_mut(turn_id)
            .ok_or_else(|| StoreError::Internal("conversation turn lost its event log".into()))?
            .extend(appended.iter().cloned());
        Ok(appended)
    }

    async fn list_turn_events_since(
        &self,
        turn_id: &ConversationTurnId,
        after_sequence: u64,
        limit: usize,
    ) -> Result<ConversationTurnEventPage, StoreError> {
        if !(1..=MAX_CONVERSATION_EVENT_BATCH).contains(&limit) {
            return Err(StoreError::Conflict);
        }
        let state = self.state.lock().await;
        let turn = state
            .conversation_turns
            .get(turn_id)
            .ok_or(StoreError::NotFound)?;
        let log = conversation_event_log(&state, turn_id)?;
        let assistant_text = turn
            .assistant_message_id
            .as_ref()
            .and_then(|id| state.messages.get(id))
            .map(|message| message.content.as_str());
        log.validate_snapshot(turn, assistant_text)
            .map_err(|error| {
                StoreError::Internal(format!("invalid conversation event log: {error}"))
            })?;
        let mut events = state
            .conversation_events
            .get(turn_id)
            .ok_or_else(|| StoreError::Internal("conversation turn lost its event log".into()))?
            .iter()
            .filter(|event| event.sequence > after_sequence)
            .take(limit.saturating_add(1))
            .cloned()
            .collect::<Vec<_>>();
        let has_more = events.len() > limit;
        events.truncate(limit);
        Ok(ConversationTurnEventPage { events, has_more })
    }

    async fn list_incomplete_turns_for_recovery(
        &self,
        limit: usize,
    ) -> Result<Vec<ConversationTurnSnapshot>, StoreError> {
        let state = self.state.lock().await;
        let mut turns = state
            .conversation_turns
            .values()
            .filter(|turn| !turn.state.is_terminal())
            .collect::<Vec<_>>();
        turns.sort_by_key(|turn| (turn.created_at, turn.id.as_str().to_owned()));
        turns
            .into_iter()
            .take(limit)
            .map(|turn| conversation_snapshot(&state, turn))
            .collect()
    }

    async fn list_thread_turns(
        &self,
        thread_id: &ThreadId,
        after: Option<&ConversationTurnId>,
        limit: usize,
    ) -> Result<Vec<ConversationTurnSnapshot>, StoreError> {
        let state = self.state.lock().await;
        if !state.threads.contains_key(thread_id) {
            return Err(StoreError::NotFound);
        }
        let mut turns = state
            .conversation_turns
            .values()
            .filter(|turn| &turn.thread_id == thread_id)
            .collect::<Vec<_>>();
        turns.sort_by_key(|turn| (turn.created_at, turn.id.as_str().to_owned()));
        let start = if let Some(cursor) = after {
            turns
                .iter()
                .position(|turn| &turn.id == cursor)
                .map(|index| index.saturating_add(1))
                .ok_or(StoreError::NotFound)?
        } else {
            0
        };
        turns
            .into_iter()
            .skip(start)
            .take(limit)
            .map(|turn| conversation_snapshot(&state, turn))
            .collect()
    }

    async fn retry_source_is_latest(&self, id: &ConversationTurnId) -> Result<bool, StoreError> {
        let state = self.state.lock().await;
        let turn = state
            .conversation_turns
            .get(id)
            .ok_or(StoreError::NotFound)?;
        conversation_snapshot(&state, turn)?;
        let source_sequence = state
            .messages
            .get(&turn.user_message_id)
            .ok_or_else(|| StoreError::Internal("retry source lost its user message".into()))?
            .sequence;
        let latest_sequence = state
            .messages
            .values()
            .filter(|message| message.thread_id == turn.thread_id)
            .map(|message| message.sequence)
            .max();
        let has_retry_child = state.conversation_lineages.values().any(|lineage| {
            matches!(
                &lineage.origin,
                ConversationTurnOrigin::Retry { source_turn_id } if source_turn_id == id
            )
        });
        Ok(latest_sequence == Some(source_sequence) && !has_retry_child)
    }

    #[allow(clippy::too_many_lines)]
    async fn reserve_conversation_fork(
        &self,
        plan: ConversationForkPlan,
    ) -> Result<ConversationForkReservation, StoreError> {
        let kind = conversation_fork_kind(&plan.child_thread)?;
        if !conversation_fork_scope_matches(&plan.command.scope, kind) {
            return Err(StoreError::Conflict);
        }
        let record_key = (plan.command.scope.clone(), plan.command.key.clone());
        let mut state = self.state.lock().await;
        if let Some(record) = state.conversation_fork_commands.get(&record_key) {
            if record.fingerprint != plan.command.fingerprint {
                return Err(StoreError::Conflict);
            }
            let snapshot = conversation_fork_snapshot(&state, record)?;
            let context = record
                .started_turn_id
                .as_ref()
                .map(|turn_id| {
                    state
                        .conversation_contexts
                        .get(turn_id)
                        .cloned()
                        .ok_or_else(|| {
                            StoreError::Internal(
                                "conversation fork turn lost its immutable context".into(),
                            )
                        })
                })
                .transpose()?;
            return Ok(ConversationForkReservation {
                snapshot,
                context,
                created: false,
                reconciled_pending_delivery: false,
            });
        }
        if let Some(reconciled) =
            reconcile_pending_conversation_fork_command(&mut state, &plan.command)?
        {
            return Ok(ConversationForkReservation {
                snapshot: reconciled.snapshot,
                context: reconciled.context,
                created: false,
                reconciled_pending_delivery: true,
            });
        }

        let prepared = prepare_conversation_fork(&state, &plan)?;
        let prepared_turn = match (
            &plan.started_turn,
            &prepared.context,
            &prepared.created_turn_event,
        ) {
            (Some(_), Some(context), Some(event)) => Some((context.clone(), event.clone())),
            (None, None, None) => None,
            _ => return Err(StoreError::Conflict),
        };
        let child_thread_id = plan.child_thread.id.clone();
        if state
            .conversation_fork_deliveries
            .contains_key(&child_thread_id)
        {
            return Err(StoreError::Conflict);
        }
        let started_turn_id = plan
            .started_turn
            .as_ref()
            .map(|turn_plan| turn_plan.turn.id.clone());

        state
            .conversation_thread_bindings
            .insert(child_thread_id.clone(), prepared.credential_binding_id);
        state
            .threads
            .insert(child_thread_id.clone(), plan.child_thread);
        for message in plan.messages {
            state.messages.insert(message.id.clone(), message);
        }
        for (message_id, source_turn_id) in prepared.inherited_outcomes {
            state
                .conversation_inherited_outcomes
                .insert(message_id, source_turn_id);
        }

        if let (Some(turn_plan), Some((context, created_turn_event))) =
            (plan.started_turn, prepared_turn)
        {
            let turn_id = turn_plan.turn.id.clone();
            let run_id = turn_plan.run.id.clone();
            state.runs.insert(run_id.clone(), turn_plan.run);
            append_events(&mut state, &run_id, vec![turn_plan.run_event]);
            state
                .conversation_turn_keys
                .insert(turn_plan.turn.idempotency_key.clone(), turn_id.clone());
            state
                .conversation_lineages
                .insert(turn_id.clone(), turn_plan.lineage);
            state.conversation_contexts.insert(turn_id.clone(), context);
            state
                .conversation_events
                .insert(turn_id.clone(), vec![created_turn_event]);
            state.conversation_turns.insert(turn_id, turn_plan.turn);
        }

        let record = ConversationForkCommandRecord {
            fingerprint: plan.command.fingerprint,
            child_thread_id: child_thread_id.clone(),
            started_turn_id,
            canonical: true,
        };
        state.conversation_fork_deliveries.insert(
            child_thread_id,
            ConversationForkDeliveryRecord {
                scope: plan.command.scope,
                request_fingerprint: plan.command.fingerprint,
                state: ConversationForkDeliveryState::Pending,
                revision: 0,
            },
        );
        state
            .conversation_fork_commands
            .insert(record_key, record.clone());
        let snapshot = conversation_fork_snapshot(&state, &record)?;
        Ok(ConversationForkReservation {
            snapshot,
            context: prepared.context,
            created: true,
            reconciled_pending_delivery: false,
        })
    }

    async fn load_conversation_fork_by_command(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<ConversationForkSnapshot>, StoreError> {
        if !conversation_fork_scope_is_supported(&command.scope) {
            return Ok(None);
        }
        let state = self.state.lock().await;
        let Some(record) = state
            .conversation_fork_commands
            .get(&(command.scope.clone(), command.key.clone()))
        else {
            return Ok(None);
        };
        if record.fingerprint != command.fingerprint {
            return Err(StoreError::Conflict);
        }
        let snapshot = conversation_fork_snapshot(&state, record)?;
        if !conversation_fork_scope_matches(
            &command.scope,
            conversation_fork_kind(&snapshot.child_thread).map_err(|_| {
                StoreError::Internal("conversation fork command lineage is invalid".into())
            })?,
        ) {
            return Err(StoreError::Internal(
                "conversation fork command scope is inconsistent".into(),
            ));
        }
        Ok(Some(snapshot))
    }

    async fn resolve_conversation_fork_command(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<ConversationForkCommandResolution>, StoreError> {
        if !conversation_fork_scope_is_supported(&command.scope) {
            return Ok(None);
        }
        let record_key = (command.scope.clone(), command.key.clone());
        let mut state = self.state.lock().await;
        if let Some(record) = state.conversation_fork_commands.get(&record_key) {
            if record.fingerprint != command.fingerprint {
                return Err(StoreError::Conflict);
            }
            return Ok(Some(ConversationForkCommandResolution {
                snapshot: conversation_fork_snapshot(&state, record)?,
                reconciled_pending_delivery: false,
            }));
        }
        Ok(
            reconcile_pending_conversation_fork_command(&mut state, command)?.map(|reconciled| {
                ConversationForkCommandResolution {
                    snapshot: reconciled.snapshot,
                    reconciled_pending_delivery: true,
                }
            }),
        )
    }

    async fn acknowledge_conversation_fork_delivery(
        &self,
        command: MutationCommand,
        child_thread_id: ThreadId,
        expected_revision: u64,
    ) -> Result<ConversationForkDelivery, StoreError> {
        const ACK_SCOPE: &str = "acknowledge_conversation_fork_delivery";
        if command.scope != ACK_SCOPE {
            return Err(StoreError::Conflict);
        }
        let command_key = (command.scope.clone(), command.key.clone());
        let mut state = self.state.lock().await;
        if let Some(record) = state
            .conversation_fork_delivery_ack_commands
            .get(&command_key)
        {
            if record.fingerprint != command.fingerprint
                || record.child_thread_id != child_thread_id
                || record.expected_revision != expected_revision
            {
                return Err(StoreError::Conflict);
            }
            if record.expected_revision != 0
                || record.outcome_revision != 1
                || record.expected_revision.checked_add(1) != Some(record.outcome_revision)
            {
                return Err(StoreError::Internal(
                    "conversation fork acknowledgement command is invalid".into(),
                ));
            }
            let delivery = conversation_fork_delivery(&state, &child_thread_id)?;
            if delivery.state != ConversationForkDeliveryState::Acknowledged
                || delivery.revision != record.outcome_revision
            {
                return Err(StoreError::Internal(
                    "conversation fork acknowledgement outcome is inconsistent".into(),
                ));
            }
            return Ok(delivery);
        }

        // Prove that the child is the canonical outcome of at least one fork
        // command before mutating its delivery row.
        let canonical_commands = state
            .conversation_fork_commands
            .values()
            .filter(|record| record.canonical && record.child_thread_id == child_thread_id)
            .count();
        if canonical_commands == 0 {
            return if state.threads.contains_key(&child_thread_id) {
                Err(StoreError::Conflict)
            } else {
                Err(StoreError::NotFound)
            };
        }
        if canonical_commands != 1 {
            return Err(StoreError::Internal(
                "conversation fork delivery has ambiguous canonical commands".into(),
            ));
        }

        // Validate the complete canonical correlation before taking a mutable
        // reference. In-memory transactions cannot roll back a partial change.
        let observed = conversation_fork_delivery(&state, &child_thread_id)?;
        if observed.state != ConversationForkDeliveryState::Pending
            || observed.revision != expected_revision
        {
            return Err(StoreError::Conflict);
        }

        let delivery = state
            .conversation_fork_deliveries
            .get_mut(&child_thread_id)
            .ok_or_else(|| {
                StoreError::Internal("conversation fork lost its delivery journal".into())
            })?;
        delivery.revision = delivery.revision.checked_add(1).ok_or_else(|| {
            StoreError::Internal("conversation fork delivery revision exhausted".into())
        })?;
        delivery.state = ConversationForkDeliveryState::Acknowledged;
        let outcome_revision = delivery.revision;
        state.conversation_fork_delivery_ack_commands.insert(
            command_key,
            ConversationForkDeliveryAckCommandRecord {
                fingerprint: command.fingerprint,
                child_thread_id: child_thread_id.clone(),
                expected_revision,
                outcome_revision,
            },
        );
        conversation_fork_delivery(&state, &child_thread_id)
    }

    async fn load_conversation_fork_metadata(
        &self,
        thread_id: &ThreadId,
    ) -> Result<ConversationForkMetadata, StoreError> {
        let state = self.state.lock().await;
        conversation_fork_metadata(&state, thread_id)
    }

    async fn thread_credential_binding(
        &self,
        thread_id: &ThreadId,
    ) -> Result<ConversationThreadCredentialBinding, StoreError> {
        let state = self.state.lock().await;
        if !state.threads.contains_key(thread_id) {
            return Err(StoreError::NotFound);
        }
        if let Some(binding) = state.conversation_thread_bindings.get(thread_id) {
            return Ok(ConversationThreadCredentialBinding::Bound(binding.clone()));
        }
        if state
            .conversation_turns
            .values()
            .any(|turn| &turn.thread_id == thread_id)
        {
            return Ok(ConversationThreadCredentialBinding::LegacyUnbound);
        }
        Ok(ConversationThreadCredentialBinding::UnboundEmpty)
    }
}

const CONVERSATION_BRANCH_COMMAND_SCOPE: &str = "branch_conversation_thread";
const CONVERSATION_EDIT_BRANCH_COMMAND_SCOPE: &str = "edit_and_branch_conversation_turn";
const CONVERSATION_REGENERATE_COMMAND_SCOPE: &str = "regenerate_conversation_turn";

struct PreparedConversationFork {
    credential_binding_id: String,
    inherited_outcomes: Vec<(MessageId, ConversationTurnId)>,
    context: Option<Vec<Message>>,
    created_turn_event: Option<ConversationTurnEvent>,
}

type PreparedConversationForkMessages =
    (Vec<(MessageId, ConversationTurnId)>, Option<Vec<Message>>);

fn conversation_fork_scope_is_supported(scope: &str) -> bool {
    matches!(
        scope,
        CONVERSATION_BRANCH_COMMAND_SCOPE
            | CONVERSATION_EDIT_BRANCH_COMMAND_SCOPE
            | CONVERSATION_REGENERATE_COMMAND_SCOPE
    )
}

fn conversation_fork_scope_matches(scope: &str, kind: ConversationForkKind) -> bool {
    matches!(
        (scope, kind),
        (
            CONVERSATION_BRANCH_COMMAND_SCOPE,
            ConversationForkKind::Branch
        ) | (
            CONVERSATION_EDIT_BRANCH_COMMAND_SCOPE,
            ConversationForkKind::EditAndBranch
        ) | (
            CONVERSATION_REGENERATE_COMMAND_SCOPE,
            ConversationForkKind::Regenerate
        )
    )
}

fn conversation_fork_kind(thread: &Thread) -> Result<ConversationForkKind, StoreError> {
    match &thread.lineage.origin {
        ConversationThreadOrigin::Fork { kind, .. } => Ok(*kind),
        ConversationThreadOrigin::Original => Err(StoreError::Conflict),
    }
}

#[allow(clippy::too_many_lines)]
fn prepare_conversation_fork(
    state: &State,
    plan: &ConversationForkPlan,
) -> Result<PreparedConversationFork, StoreError> {
    if plan.command.key.is_empty() || plan.command.key.len() > 128 {
        return Err(StoreError::Conflict);
    }
    let child = Thread::restore(plan.child_thread.clone()).map_err(|_| StoreError::Conflict)?;
    let kind = conversation_fork_kind(&child)?;
    if !conversation_fork_scope_matches(&plan.command.scope, kind)
        || state.threads.contains_key(&child.id)
        || state.conversation_thread_bindings.contains_key(&child.id)
    {
        return Err(StoreError::Conflict);
    }

    let source_turn = state
        .conversation_turns
        .get(&plan.source_turn_id)
        .ok_or(StoreError::NotFound)?;
    let source = conversation_snapshot(state, source_turn)?;
    if source.turn.revision != plan.expected_source_revision {
        return Err(StoreError::Conflict);
    }
    let parent = state
        .threads
        .get(&source.turn.thread_id)
        .cloned()
        .ok_or_else(|| StoreError::Internal("conversation fork source lost its thread".into()))?;
    Thread::restore(parent.clone())
        .map_err(|_| StoreError::Internal("conversation fork parent is invalid".into()))?;
    validate_persisted_thread_lineage(state, &parent)?;
    let project = state
        .projects
        .get(&source.turn.project_id)
        .ok_or_else(|| StoreError::Internal("conversation fork source lost its project".into()))?;
    if project.state != ProjectState::Active
        || parent.project_id != source.turn.project_id
        || child.project_id != source.turn.project_id
        || child.title != parent.title
        || child.created_at < source.turn.updated_at
        || child.created_at < parent.updated_at
    {
        return Err(StoreError::Conflict);
    }

    let source_context = state
        .conversation_contexts
        .get(&source.turn.id)
        .cloned()
        .ok_or_else(|| StoreError::Internal("conversation fork source lost its context".into()))?;
    validate_fork_source_context(&source, &source_context)?;
    let source_message = match kind {
        ConversationForkKind::EditAndBranch => &source.user_message,
        ConversationForkKind::Branch | ConversationForkKind::Regenerate => {
            source.assistant_message.as_ref().ok_or_else(|| {
                StoreError::Internal("completed fork source lost its assistant".into())
            })?
        }
    };
    let expected_child = Thread::new_fork(
        child.id.clone(),
        parent.project_id.clone(),
        parent.title.clone(),
        parent.id.clone(),
        &parent.lineage,
        source.turn.id.clone(),
        source_message.id.clone(),
        source_message.role,
        kind,
        child.created_at,
    )
    .map_err(|_| StoreError::Conflict)?;
    if child != expected_child {
        return Err(StoreError::Conflict);
    }

    ensure_fork_source_state(kind, &source)?;
    let credential_binding_id = source
        .lineage
        .credential_binding_id
        .clone()
        .ok_or(StoreError::Conflict)?;
    if state
        .conversation_thread_bindings
        .get(&parent.id)
        .is_none_or(|binding| binding != &credential_binding_id)
    {
        return Err(StoreError::Conflict);
    }

    let root = state
        .threads
        .get(&parent.lineage.root_thread_id)
        .ok_or_else(|| StoreError::Internal("conversation fork family lost its root".into()))?;
    if root.project_id != parent.project_id
        || root.lineage.root_thread_id != root.id
        || !matches!(root.lineage.origin, ConversationThreadOrigin::Original)
    {
        return Err(StoreError::Internal(
            "conversation fork family root is invalid".into(),
        ));
    }
    let direct_children = state
        .threads
        .values()
        .filter(|thread| {
            matches!(
                &thread.lineage.origin,
                ConversationThreadOrigin::Fork { parent_thread_id, .. }
                    if parent_thread_id == &parent.id
            )
        })
        .count();
    let family_threads = state
        .threads
        .values()
        .filter(|thread| thread.lineage.root_thread_id == parent.lineage.root_thread_id)
        .count();
    if direct_children >= MAX_CONVERSATION_FORK_DIRECT_CHILDREN
        || family_threads >= MAX_CONVERSATION_FORK_FAMILY_THREADS
    {
        return Err(StoreError::Conflict);
    }

    let (inherited_outcomes, provider_context) =
        validate_fork_messages(state, plan, &source, &source_context, kind)?;
    let created_turn_event = validate_fork_turn_plan(
        state,
        plan,
        &source,
        &credential_binding_id,
        provider_context.as_deref(),
        kind,
    )?;
    validate_projected_fork_metadata_budget(state, plan, &inherited_outcomes)?;

    Ok(PreparedConversationFork {
        credential_binding_id,
        inherited_outcomes,
        context: provider_context,
        created_turn_event,
    })
}

fn validate_projected_fork_metadata_budget(
    state: &State,
    plan: &ConversationForkPlan,
    inherited_outcomes: &[(MessageId, ConversationTurnId)],
) -> Result<(), StoreError> {
    if inherited_outcomes.len() > MAX_CONVERSATION_FORK_INHERITED_OUTCOMES {
        return Err(StoreError::Conflict);
    }
    let mut family_threads = state
        .threads
        .values()
        .filter(|thread| thread.lineage.root_thread_id == plan.child_thread.lineage.root_thread_id)
        .cloned()
        .collect::<Vec<_>>();
    family_threads.push(plan.child_thread.clone());
    let mut inherited_assistant_outcomes = Vec::with_capacity(inherited_outcomes.len());
    for (child_message_id, source_turn_id) in inherited_outcomes {
        let child_message = plan
            .messages
            .iter()
            .find(|message| message.id == *child_message_id)
            .ok_or(StoreError::Conflict)?;
        let source_turn = state
            .conversation_turns
            .get(source_turn_id)
            .ok_or(StoreError::Conflict)?;
        let source = conversation_snapshot(state, source_turn)?;
        if source.turn.state != ConversationTurnState::Completed
            || source
                .assistant_message
                .as_ref()
                .is_none_or(|assistant| assistant.content != child_message.content)
        {
            return Err(StoreError::Conflict);
        }
        inherited_assistant_outcomes.push(ConversationInheritedAssistantOutcome {
            child_assistant_message_id: child_message_id.clone(),
            source_turn_id: source_turn_id.clone(),
            model_id: source.turn.model_id,
            citations: source.turn.citations,
            usage: source.turn.usage,
            zero_data_retention: source.turn.zero_data_retention,
        });
    }
    let metadata = ConversationForkMetadata {
        lineage: plan.child_thread.lineage.clone(),
        inherited_assistant_outcomes,
        family_threads,
    };
    if !conversation_fork_metadata_is_within_bounds(&metadata) {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

fn ensure_fork_source_state(
    kind: ConversationForkKind,
    source: &ConversationTurnSnapshot,
) -> Result<(), StoreError> {
    let eligible = match kind {
        ConversationForkKind::Branch | ConversationForkKind::Regenerate => {
            source.turn.state == ConversationTurnState::Completed
        }
        ConversationForkKind::EditAndBranch => matches!(
            source.turn.state,
            ConversationTurnState::Completed
                | ConversationTurnState::Cancelled
                | ConversationTurnState::Failed
        ),
    };
    if eligible {
        Ok(())
    } else {
        Err(StoreError::Conflict)
    }
}

fn validate_fork_source_context(
    source: &ConversationTurnSnapshot,
    context: &[Message],
) -> Result<(), StoreError> {
    validate_context(context)
        .map_err(|_| StoreError::Internal("conversation fork source context is invalid".into()))?;
    let mut ids = HashSet::with_capacity(context.len());
    let mut previous_sequence = 0;
    for message in context {
        if Message::restore(message.clone()).is_err()
            || message.thread_id != source.turn.thread_id
            || message.state != MessageState::Active
            || message.sequence <= previous_sequence
            || !ids.insert(message.id.clone())
        {
            return Err(StoreError::Internal(
                "conversation fork source context is invalid".into(),
            ));
        }
        previous_sequence = message.sequence;
    }
    if context.last() != Some(&source.user_message) {
        return Err(StoreError::Internal(
            "conversation fork source context has an invalid final user message".into(),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn validate_fork_messages(
    state: &State,
    plan: &ConversationForkPlan,
    source: &ConversationTurnSnapshot,
    source_context: &[Message],
    kind: ConversationForkKind,
) -> Result<PreparedConversationForkMessages, StoreError> {
    let context_copy_count = if kind == ConversationForkKind::EditAndBranch {
        source_context
            .len()
            .checked_sub(1)
            .ok_or(StoreError::Conflict)?
    } else {
        source_context.len()
    };
    let expected_count = context_copy_count
        + usize::from(matches!(
            kind,
            ConversationForkKind::Branch | ConversationForkKind::EditAndBranch
        ));
    if plan.messages.len() != expected_count || plan.messages.is_empty() {
        return Err(StoreError::Conflict);
    }
    let mut ids = HashSet::with_capacity(plan.messages.len());
    let mut inherited = Vec::new();
    for (index, (message, source_message)) in plan
        .messages
        .iter()
        .take(context_copy_count)
        .zip(source_context.iter())
        .enumerate()
    {
        let sequence = u64::try_from(index + 1).map_err(|_| StoreError::Conflict)?;
        let context_position = u32::try_from(index + 1).map_err(|_| StoreError::Conflict)?;
        let expected = Message::new_derived(
            message.id.clone(),
            plan.child_thread.id.clone(),
            sequence,
            source_message.role,
            source_message.content.clone(),
            source_message.id.clone(),
            source.turn.id.clone(),
            Some(context_position),
            ConversationMessageDerivationKind::ContextCopy,
            plan.child_thread.created_at,
        )
        .map_err(|_| StoreError::Conflict)?;
        validate_new_fork_message(state, &mut ids, message, &expected)?;
        if source_message.role == MessageRole::Assistant {
            inherited.push((
                message.id.clone(),
                inherited_source_turn(state, source_message)?,
            ));
        }
    }

    match kind {
        ConversationForkKind::Branch => {
            let source_assistant = source
                .assistant_message
                .as_ref()
                .ok_or(StoreError::Conflict)?;
            let message = plan.messages.last().ok_or(StoreError::Conflict)?;
            let expected = Message::new_derived(
                message.id.clone(),
                plan.child_thread.id.clone(),
                u64::try_from(plan.messages.len()).map_err(|_| StoreError::Conflict)?,
                MessageRole::Assistant,
                source_assistant.content.clone(),
                source_assistant.id.clone(),
                source.turn.id.clone(),
                None,
                ConversationMessageDerivationKind::SourceAssistantCopy,
                plan.child_thread.created_at,
            )
            .map_err(|_| StoreError::Conflict)?;
            validate_new_fork_message(state, &mut ids, message, &expected)?;
            inherited.push((message.id.clone(), source.turn.id.clone()));
            Ok((inherited, None))
        }
        ConversationForkKind::EditAndBranch => {
            let message = plan.messages.last().ok_or(StoreError::Conflict)?;
            if message.content == source.user_message.content {
                return Err(StoreError::Conflict);
            }
            let expected = Message::new_derived(
                message.id.clone(),
                plan.child_thread.id.clone(),
                u64::try_from(plan.messages.len()).map_err(|_| StoreError::Conflict)?,
                MessageRole::User,
                message.content.clone(),
                source.user_message.id.clone(),
                source.turn.id.clone(),
                Some(u32::try_from(source_context.len()).map_err(|_| StoreError::Conflict)?),
                ConversationMessageDerivationKind::EditedUser,
                plan.child_thread.created_at,
            )
            .map_err(|_| StoreError::Conflict)?;
            validate_new_fork_message(state, &mut ids, message, &expected)?;
            validate_context(&plan.messages)?;
            Ok((inherited, Some(plan.messages.clone())))
        }
        ConversationForkKind::Regenerate => {
            validate_context(&plan.messages)?;
            Ok((inherited, Some(plan.messages.clone())))
        }
    }
}

fn validate_new_fork_message(
    state: &State,
    ids: &mut HashSet<MessageId>,
    actual: &Message,
    expected: &Message,
) -> Result<(), StoreError> {
    if actual != expected
        || state.messages.contains_key(&actual.id)
        || !ids.insert(actual.id.clone())
    {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

fn inherited_source_turn(
    state: &State,
    source_message: &Message,
) -> Result<ConversationTurnId, StoreError> {
    let direct = state
        .conversation_turns
        .values()
        .filter(|turn| turn.assistant_message_id.as_ref() == Some(&source_message.id))
        .map(|turn| turn.id.clone())
        .collect::<Vec<_>>();
    let inherited = state
        .conversation_inherited_outcomes
        .get(&source_message.id)
        .cloned();
    let source_turn_id = match (direct.as_slice(), inherited) {
        ([turn_id], None) => turn_id.clone(),
        ([], Some(turn_id)) => turn_id,
        _ => {
            return Err(StoreError::Internal(
                "copied assistant has ambiguous or missing outcome lineage".into(),
            ));
        }
    };
    let source_turn = state
        .conversation_turns
        .get(&source_turn_id)
        .ok_or_else(|| StoreError::Internal("inherited outcome lost its source turn".into()))?;
    let snapshot = conversation_snapshot(state, source_turn)?;
    let Some(source_assistant) = snapshot.assistant_message.as_ref() else {
        return Err(StoreError::Internal(
            "inherited assistant outcome is inconsistent".into(),
        ));
    };
    if snapshot.turn.state != ConversationTurnState::Completed
        || source_assistant.content != source_message.content
        || validate_inherited_assistant_provenance(
            state,
            source_message,
            source_assistant,
            &source_turn_id,
        )
        .is_err()
    {
        return Err(StoreError::Internal(
            "inherited assistant outcome is inconsistent".into(),
        ));
    }
    Ok(source_turn_id)
}

fn validate_inherited_assistant_provenance(
    state: &State,
    message: &Message,
    source_assistant: &Message,
    source_turn_id: &ConversationTurnId,
) -> Result<(), StoreError> {
    let mut current = message;
    let mut visited = HashSet::new();
    for _ in 0..=MAX_CONVERSATION_FORK_FAMILY_THREADS {
        if current.role != MessageRole::Assistant
            || current.state != MessageState::Active
            || current.content != source_assistant.content
            || !visited.insert(current.id.clone())
        {
            return Err(StoreError::Internal(
                "inherited assistant provenance is invalid".into(),
            ));
        }
        if current.id == source_assistant.id {
            return Ok(());
        }
        if state.conversation_inherited_outcomes.get(&current.id) != Some(source_turn_id) {
            return Err(StoreError::Internal(
                "inherited assistant provenance is invalid".into(),
            ));
        }
        let ConversationMessageDerivation::Fork {
            kind,
            source_message_id,
            source_turn_id: derivation_source_turn_id,
            ..
        } = &current.derivation
        else {
            return Err(StoreError::Internal(
                "inherited assistant provenance is invalid".into(),
            ));
        };
        if *kind == ConversationMessageDerivationKind::EditedUser
            || (*kind == ConversationMessageDerivationKind::SourceAssistantCopy
                && derivation_source_turn_id != source_turn_id)
        {
            return Err(StoreError::Internal(
                "inherited assistant provenance is invalid".into(),
            ));
        }
        current = state.messages.get(source_message_id).ok_or_else(|| {
            StoreError::Internal("inherited assistant provenance lost its source".into())
        })?;
    }
    Err(StoreError::Internal(
        "inherited assistant provenance exceeds its bound".into(),
    ))
}

fn validate_fork_turn_plan(
    state: &State,
    plan: &ConversationForkPlan,
    source: &ConversationTurnSnapshot,
    credential_binding_id: &str,
    context: Option<&[Message]>,
    kind: ConversationForkKind,
) -> Result<Option<ConversationTurnEvent>, StoreError> {
    if kind == ConversationForkKind::Branch {
        return if plan.started_turn.is_none() && context.is_none() {
            Ok(None)
        } else {
            Err(StoreError::Conflict)
        };
    }
    let turn_plan = plan.started_turn.as_ref().ok_or(StoreError::Conflict)?;
    let context = context.ok_or(StoreError::Conflict)?;
    let user_message = context.last().ok_or(StoreError::Conflict)?;
    let expected_origin = match kind {
        ConversationForkKind::EditAndBranch => ConversationTurnOrigin::EditAndBranch {
            source_turn_id: source.turn.id.clone(),
        },
        ConversationForkKind::Regenerate => ConversationTurnOrigin::Regenerate {
            source_turn_id: source.turn.id.clone(),
        },
        ConversationForkKind::Branch => return Err(StoreError::Conflict),
    };
    if turn_plan.lineage.origin != expected_origin
        || turn_plan.lineage.credential_binding_id.as_deref() != Some(credential_binding_id)
        || turn_plan.lineage.retry_depth != 0
        || ConversationTurnLineage::restore(turn_plan.lineage.clone(), &turn_plan.turn.id).is_err()
        || turn_plan.turn.idempotency_key != plan.command.key
        || turn_plan.turn.request_fingerprint != plan.command.fingerprint
        || turn_plan.turn.project_id != plan.child_thread.project_id
        || turn_plan.turn.thread_id != plan.child_thread.id
        || turn_plan.turn.user_message_id != user_message.id
        || turn_plan.turn.model_id != source.turn.model_id
        || turn_plan.turn.created_at != plan.child_thread.created_at
        || turn_plan.turn.state != ConversationTurnState::Reserved
        || turn_plan.turn.revision != 0
        || user_message.role != MessageRole::User
        || state.conversation_turns.contains_key(&turn_plan.turn.id)
        || state.runs.contains_key(&turn_plan.run.id)
        || state
            .conversation_turn_keys
            .contains_key(&turn_plan.turn.idempotency_key)
    {
        return Err(StoreError::Conflict);
    }
    let expected_run = Run::queued(
        turn_plan.run.id.clone(),
        plan.child_thread.project_id.clone(),
        plan.child_thread.id.clone(),
        plan.child_thread.created_at,
    );
    if turn_plan.run != expected_run
        || turn_plan.run.id != turn_plan.turn.run_id
        || turn_plan.run_event
            != (NewRunEvent {
                occurred_at: plan.child_thread.created_at,
                kind: RunEventKind::Created,
            })
        || turn_plan.turn_event != ConversationTurnEventKind::Created
    {
        return Err(StoreError::Conflict);
    }
    let prospective = ConversationTurnSnapshot {
        turn: turn_plan.turn.clone(),
        user_message: user_message.clone(),
        assistant_message: None,
        run: turn_plan.run.clone(),
        effect: None,
        lineage: turn_plan.lineage.clone(),
    };
    if !is_canonical_conversation_snapshot(&prospective) {
        return Err(StoreError::Conflict);
    }
    let mut event_log = ConversationTurnEventLog::new(turn_plan.turn.id.clone());
    let event = event_log
        .append_kind(ConversationTurnEventKind::Created)
        .map_err(|_| StoreError::Conflict)?;
    event_log
        .validate_snapshot(&turn_plan.turn, None)
        .map_err(|_| StoreError::Conflict)?;
    Ok(Some(event))
}

struct ReconciledConversationForkCommand {
    snapshot: ConversationForkSnapshot,
    context: Option<Vec<Message>>,
}

fn reconcile_pending_conversation_fork_command(
    state: &mut State,
    command: &MutationCommand,
) -> Result<Option<ReconciledConversationForkCommand>, StoreError> {
    if !conversation_fork_scope_is_supported(&command.scope) {
        return Ok(None);
    }
    let command_key = (command.scope.clone(), command.key.clone());
    if let Some(record) = state.conversation_fork_commands.get(&command_key) {
        if record.fingerprint != command.fingerprint {
            return Err(StoreError::Conflict);
        }
        let snapshot = conversation_fork_snapshot(state, record)?;
        let context = fork_command_context(state, record)?;
        return Ok(Some(ReconciledConversationForkCommand {
            snapshot,
            context,
        }));
    }

    let pending_children = state
        .conversation_fork_deliveries
        .iter()
        .filter(|(_child_thread_id, delivery)| {
            delivery.state == ConversationForkDeliveryState::Pending
                && delivery.scope == command.scope
                && delivery.request_fingerprint == command.fingerprint
        })
        .map(|(child_thread_id, _delivery)| child_thread_id.clone())
        .collect::<Vec<_>>();
    let Some(child_thread_id) = pending_children.first() else {
        return Ok(None);
    };
    if pending_children.len() != 1 {
        return Err(StoreError::Internal(
            "conversation fork pending delivery is not unique".into(),
        ));
    }

    let canonical_records = state
        .conversation_fork_commands
        .iter()
        .filter(|((_scope, _key), record)| {
            record.canonical && record.child_thread_id == *child_thread_id
        })
        .map(|((_scope, _key), record)| record.clone())
        .collect::<Vec<_>>();
    if canonical_records.len() != 1 {
        return Err(StoreError::Internal(
            "conversation fork pending delivery lost its canonical command".into(),
        ));
    }
    let aliases = state
        .conversation_fork_commands
        .values()
        .filter(|record| !record.canonical && record.child_thread_id == *child_thread_id)
        .count();
    if aliases >= MAX_CONVERSATION_FORK_DELIVERY_ALIASES {
        return Err(StoreError::Conflict);
    }
    let mut alias = canonical_records
        .into_iter()
        .next()
        .ok_or_else(|| StoreError::Internal("conversation fork command disappeared".into()))?;
    if alias.fingerprint != command.fingerprint {
        return Err(StoreError::Internal(
            "conversation fork delivery fingerprint is inconsistent".into(),
        ));
    }
    alias.canonical = false;
    let snapshot = conversation_fork_snapshot(state, &alias)?;
    let context = fork_command_context(state, &alias)?;
    state.conversation_fork_commands.insert(command_key, alias);
    Ok(Some(ReconciledConversationForkCommand {
        snapshot,
        context,
    }))
}

fn fork_command_context(
    state: &State,
    record: &ConversationForkCommandRecord,
) -> Result<Option<Vec<Message>>, StoreError> {
    record
        .started_turn_id
        .as_ref()
        .map(|turn_id| {
            state
                .conversation_contexts
                .get(turn_id)
                .cloned()
                .ok_or_else(|| {
                    StoreError::Internal("conversation fork turn lost its immutable context".into())
                })
        })
        .transpose()
}

fn conversation_fork_delivery(
    state: &State,
    child_thread_id: &ThreadId,
) -> Result<ConversationForkDelivery, StoreError> {
    let record = state
        .conversation_fork_deliveries
        .get(child_thread_id)
        .ok_or_else(|| {
            StoreError::Internal("conversation fork lost its delivery journal".into())
        })?;
    let canonical = state
        .conversation_fork_commands
        .iter()
        .filter(|((_scope, _key), command)| {
            command.canonical && command.child_thread_id == *child_thread_id
        })
        .collect::<Vec<_>>();
    if canonical.len() != 1 {
        return Err(StoreError::Internal(
            "conversation fork delivery has an invalid canonical command".into(),
        ));
    }
    let ((scope, _key), command) = canonical[0];
    let aliases = state
        .conversation_fork_commands
        .iter()
        .filter(|((_scope, _key), command)| {
            !command.canonical && command.child_thread_id == *child_thread_id
        })
        .collect::<Vec<_>>();
    if aliases.len() > MAX_CONVERSATION_FORK_DELIVERY_ALIASES
        || aliases.iter().any(|((alias_scope, _key), alias)| {
            alias_scope != scope || alias.fingerprint != record.request_fingerprint
        })
        || scope != &record.scope
        || command.fingerprint != record.request_fingerprint
        || !matches!(
            (record.state, record.revision),
            (ConversationForkDeliveryState::Pending, 0)
                | (ConversationForkDeliveryState::Acknowledged, 1)
        )
    {
        return Err(StoreError::Internal(
            "conversation fork delivery correlation is invalid".into(),
        ));
    }
    let thread = state.threads.get(child_thread_id).ok_or_else(|| {
        StoreError::Internal("conversation fork delivery lost its child thread".into())
    })?;
    if conversation_fork_kind(thread).is_err() {
        return Err(StoreError::Internal(
            "conversation fork delivery points to a non-fork thread".into(),
        ));
    }
    Ok(ConversationForkDelivery {
        child_thread_id: child_thread_id.clone(),
        state: record.state,
        revision: record.revision,
    })
}

fn conversation_fork_snapshot(
    state: &State,
    record: &ConversationForkCommandRecord,
) -> Result<ConversationForkSnapshot, StoreError> {
    let child_thread = state
        .threads
        .get(&record.child_thread_id)
        .cloned()
        .ok_or_else(|| StoreError::Internal("conversation fork lost its child thread".into()))?;
    Thread::restore(child_thread.clone())
        .map_err(|_| StoreError::Internal("conversation fork child is invalid".into()))?;
    validate_persisted_thread_lineage(state, &child_thread)?;
    let kind = conversation_fork_kind(&child_thread)
        .map_err(|_| StoreError::Internal("conversation fork child has no lineage".into()))?;
    let delivery_record = state
        .conversation_fork_deliveries
        .get(&child_thread.id)
        .ok_or_else(|| {
            StoreError::Internal("conversation fork lost its delivery journal".into())
        })?;
    if !conversation_fork_scope_matches(&delivery_record.scope, kind) {
        return Err(StoreError::Internal(
            "conversation fork delivery scope is inconsistent".into(),
        ));
    }
    let mut messages = state
        .messages
        .values()
        .filter(|message| message.thread_id == child_thread.id && !message.derivation.is_original())
        .cloned()
        .collect::<Vec<_>>();
    messages.sort_by_key(|message| message.sequence);
    if messages.is_empty()
        || messages.iter().enumerate().any(|(index, message)| {
            Message::restore(message.clone()).is_err()
                || message.sequence != u64::try_from(index + 1).unwrap_or(u64::MAX)
        })
    {
        return Err(StoreError::Internal(
            "conversation fork copied messages are invalid".into(),
        ));
    }
    let started_turn = record
        .started_turn_id
        .as_ref()
        .map(|turn_id| {
            let turn = state.conversation_turns.get(turn_id).ok_or_else(|| {
                StoreError::Internal("conversation fork lost its started turn".into())
            })?;
            conversation_snapshot(state, turn)
        })
        .transpose()?;
    if (kind == ConversationForkKind::Branch) != started_turn.is_none()
        || started_turn.as_ref().is_some_and(|turn| {
            turn.turn.thread_id != child_thread.id
                || turn.turn.request_fingerprint != record.fingerprint
        })
    {
        return Err(StoreError::Internal(
            "conversation fork command outcome is inconsistent".into(),
        ));
    }
    if record.fingerprint != delivery_record.request_fingerprint {
        return Err(StoreError::Internal(
            "conversation fork command and delivery fingerprints differ".into(),
        ));
    }
    let delivery = conversation_fork_delivery(state, &child_thread.id)?;
    Ok(ConversationForkSnapshot {
        child_thread,
        messages,
        started_turn,
        delivery,
    })
}

#[allow(clippy::too_many_lines)]
fn conversation_fork_metadata(
    state: &State,
    thread_id: &ThreadId,
) -> Result<ConversationForkMetadata, StoreError> {
    let thread = state
        .threads
        .get(thread_id)
        .cloned()
        .ok_or(StoreError::NotFound)?;
    Thread::restore(thread.clone())
        .map_err(|_| StoreError::Internal("conversation fork metadata thread is invalid".into()))?;
    validate_persisted_thread_lineage(state, &thread)?;
    let mut family_threads = state
        .threads
        .values()
        .filter(|candidate| candidate.lineage.root_thread_id == thread.lineage.root_thread_id)
        .cloned()
        .collect::<Vec<_>>();
    family_threads.sort_by_key(|candidate| {
        (
            candidate.lineage.fork_depth,
            candidate.created_at,
            candidate.id.as_str().to_owned(),
        )
    });
    if family_threads.is_empty()
        || family_threads.len() > MAX_CONVERSATION_FORK_FAMILY_THREADS
        || family_threads.iter().any(|candidate| {
            candidate.project_id != thread.project_id
                || Thread::restore(candidate.clone()).is_err()
                || validate_persisted_thread_lineage(state, candidate).is_err()
        })
    {
        return Err(StoreError::Internal(
            "conversation fork family metadata is invalid".into(),
        ));
    }

    let mut derived_assistants = state
        .messages
        .values()
        .filter(|message| {
            message.thread_id == *thread_id
                && message.role == MessageRole::Assistant
                && !message.derivation.is_original()
        })
        .collect::<Vec<_>>();
    derived_assistants.sort_by_key(|message| message.sequence);
    let mut inherited_assistant_outcomes = Vec::with_capacity(derived_assistants.len());
    for message in derived_assistants {
        let source_turn_id = state
            .conversation_inherited_outcomes
            .get(&message.id)
            .ok_or_else(|| {
                StoreError::Internal("copied assistant lost its inherited outcome".into())
            })?;
        if inherited_source_turn(state, message)? != *source_turn_id {
            return Err(StoreError::Internal(
                "copied assistant outcome has invalid provenance".into(),
            ));
        }
        let source_turn = state
            .conversation_turns
            .get(source_turn_id)
            .ok_or_else(|| StoreError::Internal("inherited outcome lost its turn".into()))?;
        let snapshot = conversation_snapshot(state, source_turn)?;
        if snapshot.turn.state != ConversationTurnState::Completed
            || snapshot
                .assistant_message
                .as_ref()
                .is_none_or(|assistant| assistant.content != message.content)
        {
            return Err(StoreError::Internal(
                "copied assistant outcome is inconsistent".into(),
            ));
        }
        inherited_assistant_outcomes.push(ConversationInheritedAssistantOutcome {
            child_assistant_message_id: message.id.clone(),
            source_turn_id: source_turn_id.clone(),
            model_id: snapshot.turn.model_id,
            citations: snapshot.turn.citations,
            usage: snapshot.turn.usage,
            zero_data_retention: snapshot.turn.zero_data_retention,
        });
    }
    let unexpected_outcome = state
        .conversation_inherited_outcomes
        .keys()
        .any(|message_id| {
            state.messages.get(message_id).is_some_and(|message| {
                message.thread_id == *thread_id && message.role != MessageRole::Assistant
            })
        });
    if unexpected_outcome {
        return Err(StoreError::Internal(
            "conversation fork outcome is attached to a non-assistant".into(),
        ));
    }
    let metadata = ConversationForkMetadata {
        lineage: thread.lineage,
        inherited_assistant_outcomes,
        family_threads,
    };
    if !conversation_fork_metadata_is_within_bounds(&metadata) {
        return Err(StoreError::Internal(
            "conversation fork metadata exceeds its bound".into(),
        ));
    }
    Ok(metadata)
}

fn validate_persisted_thread_lineage(state: &State, thread: &Thread) -> Result<(), StoreError> {
    match &thread.lineage.origin {
        ConversationThreadOrigin::Original => {
            if thread.lineage.root_thread_id != thread.id || thread.lineage.fork_depth != 0 {
                return Err(StoreError::Internal(
                    "original conversation thread lineage is invalid".into(),
                ));
            }
        }
        ConversationThreadOrigin::Fork {
            parent_thread_id,
            source_turn_id,
            source_message_id,
            kind,
        } => {
            let parent = state.threads.get(parent_thread_id).ok_or_else(|| {
                StoreError::Internal("conversation fork lineage lost its parent".into())
            })?;
            let source_turn = state
                .conversation_turns
                .get(source_turn_id)
                .ok_or_else(|| {
                    StoreError::Internal("conversation fork lineage lost its source turn".into())
                })?;
            let source = conversation_snapshot(state, source_turn)?;
            ensure_fork_source_state(*kind, &source).map_err(|_| {
                StoreError::Internal("conversation fork source lifecycle is invalid".into())
            })?;
            let expected_source_message = match kind {
                ConversationForkKind::EditAndBranch => &source.user_message,
                ConversationForkKind::Branch | ConversationForkKind::Regenerate => {
                    source.assistant_message.as_ref().ok_or_else(|| {
                        StoreError::Internal(
                            "conversation fork lineage lost its source assistant".into(),
                        )
                    })?
                }
            };
            if source.turn.thread_id != parent.id
                || parent.project_id != thread.project_id
                || parent.lineage.root_thread_id != thread.lineage.root_thread_id
                || parent.lineage.fork_depth.checked_add(1) != Some(thread.lineage.fork_depth)
                || expected_source_message.id != *source_message_id
                || kind.source_message_role() != expected_source_message.role
                || state
                    .conversation_thread_bindings
                    .get(&thread.id)
                    .zip(state.conversation_thread_bindings.get(&parent.id))
                    .is_none_or(|(child, parent)| child != parent)
            {
                return Err(StoreError::Internal(
                    "conversation fork lineage is inconsistent".into(),
                ));
            }
        }
    }
    Ok(())
}

fn commit_conversation_terminal(
    state: &mut State,
    commit: TerminalTurnCommit,
) -> Result<ConversationTurnSnapshot, StoreError> {
    let current = state
        .conversation_turns
        .get(&commit.turn.id)
        .ok_or(StoreError::NotFound)
        .and_then(|turn| conversation_snapshot(state, turn))?;
    if !is_exact_terminal_transition(&current, &commit) {
        return Err(StoreError::Conflict);
    }

    let mut persisted_assistant = commit.assistant_message.clone();
    if let Some(assistant) = persisted_assistant.as_mut() {
        if state.messages.contains_key(&assistant.id) {
            return Err(StoreError::Conflict);
        }
        assistant.sequence = state
            .messages
            .values()
            .filter(|message| message.thread_id == assistant.thread_id)
            .map(|message| message.sequence)
            .max()
            .unwrap_or_default()
            .checked_add(1)
            .ok_or(StoreError::Conflict)?;
    }

    let prospective = ConversationTurnSnapshot {
        turn: commit.turn.clone(),
        user_message: current.user_message,
        assistant_message: persisted_assistant.clone(),
        run: commit.run.clone(),
        effect: commit.effect.clone(),
        lineage: current.lineage,
    };
    if !is_canonical_conversation_snapshot(&prospective) {
        return Err(StoreError::Conflict);
    }

    let mut turn_event_log = conversation_event_log(state, &commit.turn.id)?;
    let persisted_turn_event = turn_event_log
        .append_kind(commit.turn_event.clone())
        .map_err(|_| StoreError::Conflict)?;
    turn_event_log
        .validate_snapshot(
            &commit.turn,
            persisted_assistant
                .as_ref()
                .map(|assistant| assistant.content.as_str()),
        )
        .map_err(|_| StoreError::Conflict)?;

    let turn_id = commit.turn.id.clone();
    let run_id = commit.run.id.clone();
    if let Some(assistant) = persisted_assistant {
        state.messages.insert(assistant.id.clone(), assistant);
    }
    if let Some(effect) = commit.effect {
        state.effects.insert(effect.id.clone(), effect);
    }
    state.runs.insert(run_id.clone(), commit.run);
    state
        .conversation_turns
        .insert(turn_id.clone(), commit.turn);
    state
        .conversation_events
        .get_mut(&turn_id)
        .ok_or_else(|| StoreError::Internal("conversation turn lost its event log".into()))?
        .push(persisted_turn_event);
    append_events(state, &run_id, commit.events);
    let turn = state
        .conversation_turns
        .get(&turn_id)
        .ok_or(StoreError::NotFound)?;
    conversation_snapshot(state, turn)
}

fn commit_conversation_cancellation(
    state: &mut State,
    commit: CancelConversationTurnCommit,
    expected_scope: &str,
) -> Result<ConversationTurnSnapshot, StoreError> {
    if commit.command.scope != expected_scope {
        return Err(StoreError::Conflict);
    }
    let record_key = (commit.command.scope.clone(), commit.command.key.clone());
    if let Some(record) = state.conversation_cancel_commands.get(&record_key) {
        if record.fingerprint != commit.command.fingerprint || record.turn_id != commit.turn_id {
            return Err(StoreError::Conflict);
        }
        let snapshot = state
            .conversation_turns
            .get(&record.turn_id)
            .ok_or_else(|| StoreError::Internal("cancel command lost its turn".into()))
            .and_then(|turn| conversation_snapshot(state, turn))?;
        if !snapshot.turn.state.is_terminal()
            || snapshot.turn.state != record.outcome_state
            || snapshot.turn.revision != record.outcome_revision
        {
            return Err(StoreError::Internal(
                "cancel command outcome binding is invalid".into(),
            ));
        }
        return Ok(snapshot);
    }

    let current = state
        .conversation_turns
        .get(&commit.turn_id)
        .ok_or(StoreError::NotFound)
        .and_then(|turn| conversation_snapshot(state, turn))?;
    let outcome = if current.turn.state.is_terminal() {
        if commit.expected_turn_revision.checked_add(1) != Some(current.turn.revision) {
            return Err(StoreError::Conflict);
        }
        current
    } else {
        if current.turn.revision != commit.expected_turn_revision {
            return Err(StoreError::Conflict);
        }
        let terminal = commit.terminal.ok_or(StoreError::Conflict)?;
        if terminal.turn.id != commit.turn_id
            || terminal.expected_turn_revision != commit.expected_turn_revision
            || !is_exact_cancellation_edge(current.turn.state, terminal.turn.state)
        {
            return Err(StoreError::Conflict);
        }
        commit_conversation_terminal(state, terminal)?
    };
    let outcome_state = outcome.turn.state;
    let outcome_revision = outcome.turn.revision;
    state.conversation_cancel_commands.insert(
        record_key,
        ConversationCancelCommandRecord {
            fingerprint: commit.command.fingerprint,
            turn_id: commit.turn_id,
            outcome_state,
            outcome_revision,
        },
    );
    Ok(outcome)
}

const fn is_exact_cancellation_edge(
    from: ConversationTurnState,
    to: ConversationTurnState,
) -> bool {
    matches!(
        (from, to),
        (
            ConversationTurnState::Reserved,
            ConversationTurnState::Cancelled
        ) | (
            ConversationTurnState::ProviderStarted,
            ConversationTurnState::InterruptedNeedsReview
        )
    )
}

fn capture_conversation_context(
    state: &State,
    turn: &ConversationTurn,
    mut user_message: Message,
) -> Result<(Message, Vec<Message>), StoreError> {
    user_message.sequence = state
        .messages
        .values()
        .filter(|message| message.thread_id == turn.thread_id)
        .map(|message| message.sequence)
        .max()
        .unwrap_or_default()
        .checked_add(1)
        .ok_or(StoreError::Conflict)?;
    let mut context = Vec::with_capacity(MAX_CONVERSATION_CONTEXT_MESSAGES.min(64));
    let mut context_bytes = user_message.content.len();
    for message in state.messages.values().filter(|message| {
        message.thread_id == turn.thread_id && message.state == MessageState::Active
    }) {
        let unresolved_prompt = state.conversation_turns.values().any(|existing| {
            existing.thread_id == turn.thread_id
                && existing.user_message_id == message.id
                && existing.state != ConversationTurnState::Completed
        });
        if unresolved_prompt {
            continue;
        }
        if context.len() >= MAX_CONVERSATION_CONTEXT_MESSAGES - 1 {
            return Err(StoreError::Conflict);
        }
        context_bytes = context_bytes
            .checked_add(message.content.len())
            .ok_or(StoreError::Conflict)?;
        if context_bytes > MAX_CONVERSATION_CONTEXT_BYTES {
            return Err(StoreError::Conflict);
        }
        context.push(message.clone());
    }
    context.sort_by_key(|message| message.sequence);
    context.push(user_message.clone());
    validate_context(&context)?;
    Ok((user_message, context))
}

fn reservation_source_matches_lineage(
    source: &ConversationTurnReservationSource,
    lineage: &ConversationTurnLineage,
) -> bool {
    match (source, &lineage.origin) {
        (ConversationTurnReservationSource::CurrentThread, ConversationTurnOrigin::Original) => {
            true
        }
        (
            ConversationTurnReservationSource::Retry { source_turn_id, .. },
            ConversationTurnOrigin::Retry {
                source_turn_id: lineage_source,
            },
        ) => source_turn_id == lineage_source,
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
fn capture_retry_context(
    state: &State,
    turn: &ConversationTurn,
    lineage: &ConversationTurnLineage,
    source_turn_id: &ConversationTurnId,
    expected_source_revision: u64,
    mut user_message: Message,
) -> Result<(Message, Vec<Message>), StoreError> {
    let source_turn = state
        .conversation_turns
        .get(source_turn_id)
        .ok_or(StoreError::NotFound)?;
    let source_lineage = state
        .conversation_lineages
        .get(source_turn_id)
        .ok_or_else(|| StoreError::Internal("retry source lost its lineage".into()))?;
    let source_user = state
        .messages
        .get(&source_turn.user_message_id)
        .ok_or_else(|| StoreError::Internal("retry source lost its user message".into()))?;
    let eligible_state = source_turn.state == ConversationTurnState::Cancelled
        || (source_turn.state == ConversationTurnState::Failed
            && source_turn
                .failure
                .as_ref()
                .is_some_and(|failure| failure.retryable));
    let expected_source_depth = source_lineage
        .retry_depth
        .checked_add(1)
        .filter(|depth| *depth <= 64);
    let newest_sequence = state
        .messages
        .values()
        .filter(|message| message.thread_id == source_turn.thread_id)
        .map(|message| message.sequence)
        .max();
    let already_retried = state.conversation_lineages.values().any(|candidate| {
        matches!(
            &candidate.origin,
            ConversationTurnOrigin::Retry { source_turn_id: candidate_source }
                if candidate_source == source_turn_id
        )
    });
    if source_turn.revision != expected_source_revision
        || !eligible_state
        || source_turn.thread_id != turn.thread_id
        || source_turn.project_id != turn.project_id
        || source_turn.model_id != turn.model_id
        || newest_sequence != Some(source_user.sequence)
        || already_retried
        || source_lineage.credential_binding_id.is_none()
        || source_lineage.credential_binding_id != lineage.credential_binding_id
        || expected_source_depth != Some(lineage.retry_depth)
        || user_message.content != source_user.content
    {
        return Err(StoreError::Conflict);
    }

    user_message.sequence = source_user
        .sequence
        .checked_add(1)
        .ok_or(StoreError::Conflict)?;
    let mut context = state
        .conversation_contexts
        .get(source_turn_id)
        .cloned()
        .ok_or_else(|| StoreError::Internal("retry source lost its immutable context".into()))?;
    if context.last() != Some(source_user) {
        return Err(StoreError::Internal(
            "retry source context is internally inconsistent".into(),
        ));
    }
    context.pop();
    context.push(user_message.clone());
    validate_context(&context)?;
    Ok((user_message, context))
}

fn is_canonical_reservation_input(
    turn: &ConversationTurn,
    user_message: &Message,
    run: &Run,
    event: &NewRunEvent,
) -> bool {
    turn.state == ConversationTurnState::Reserved
        && ConversationTurn::restore(turn.clone()).is_ok()
        && turn.user_message_id == user_message.id
        && turn.thread_id == user_message.thread_id
        && turn.run_id == run.id
        && turn.project_id == run.project_id
        && turn.thread_id == run.thread_id
        && is_canonical_message(user_message, MessageRole::User, turn.created_at, 0)
        && *run
            == Run::queued(
                turn.run_id.clone(),
                turn.project_id.clone(),
                turn.thread_id.clone(),
                turn.created_at,
            )
        && *event
            == NewRunEvent {
                occurred_at: turn.created_at,
                kind: RunEventKind::Created,
            }
}

fn is_exact_provider_start(
    current: &ConversationTurnSnapshot,
    commit: &ProviderStartCommit,
) -> bool {
    if current.turn.state != ConversationTurnState::Reserved
        || current.effect.is_some()
        || current.assistant_message.is_some()
        || commit.expected_turn_revision != current.turn.revision
        || commit.expected_run_revision != current.run.revision
    {
        return false;
    }

    let Some(provider_fingerprint) = commit.turn.provider_request_fingerprint else {
        return false;
    };
    let transition_at = commit.turn.updated_at;
    let mut expected_turn = current.turn.clone();
    if expected_turn
        .start_provider(
            commit.effect.id.clone(),
            provider_fingerprint,
            transition_at,
        )
        .is_err()
        || expected_turn != commit.turn
    {
        return false;
    }

    let mut expected_run = current.run.clone();
    if expected_run
        .transition(RunState::Planning, transition_at)
        .and_then(|()| expected_run.transition(RunState::Running, transition_at))
        .is_err()
        || expected_run != commit.run
    {
        return false;
    }

    let mut expected_effect = SideEffect::prepare(
        commit.effect.id.clone(),
        current.run.id.clone(),
        EffectKind::ExternalMutation,
        format!("official xAI Responses API model {}", current.turn.model_id),
        Idempotency::NonIdempotent,
        transition_at,
    );
    if expected_effect.start(transition_at).is_err() || expected_effect != commit.effect {
        return false;
    }

    commit.events
        == vec![
            NewRunEvent {
                occurred_at: transition_at,
                kind: RunEventKind::StateChanged {
                    from: RunState::Queued,
                    to: RunState::Planning,
                },
            },
            NewRunEvent {
                occurred_at: transition_at,
                kind: RunEventKind::StateChanged {
                    from: RunState::Planning,
                    to: RunState::Running,
                },
            },
            NewRunEvent {
                occurred_at: transition_at,
                kind: RunEventKind::EffectPrepared {
                    effect_id: commit.effect.id.clone(),
                },
            },
        ]
}

fn is_exact_terminal_transition(
    current: &ConversationTurnSnapshot,
    commit: &TerminalTurnCommit,
) -> bool {
    if commit.expected_turn_revision != current.turn.revision
        || commit.expected_run_revision != current.run.revision
        || !is_exact_terminal_turn_transition(current, commit)
    {
        return false;
    }

    let transition_at = commit.turn.updated_at;
    let Some(expected_run_state) = terminal_run_state(commit.turn.state) else {
        return false;
    };
    let mut expected_run = current.run.clone();
    if expected_run
        .transition(expected_run_state, transition_at)
        .is_err()
        || expected_run != commit.run
    {
        return false;
    }

    expected_terminal_events(
        current,
        commit.turn.state,
        transition_at,
        expected_run_state,
    )
    .is_some_and(|events| commit.events == events)
        && is_exact_terminal_effect_transition(current, commit, transition_at)
}

fn is_exact_terminal_turn_transition(
    current: &ConversationTurnSnapshot,
    commit: &TerminalTurnCommit,
) -> bool {
    let transition_at = commit.turn.updated_at;
    let mut expected = current.turn.clone();
    let transition = match commit.turn.state {
        ConversationTurnState::Completed => {
            let Some(assistant) = commit.assistant_message.as_ref() else {
                return false;
            };
            if !is_canonical_message(assistant, MessageRole::Assistant, transition_at, 0) {
                return false;
            }
            expected.complete(
                assistant.id.clone(),
                commit.turn.provider_response_id.clone(),
                commit.turn.citations.clone(),
                commit.turn.usage,
                commit.turn.zero_data_retention,
                transition_at,
            )
        }
        ConversationTurnState::Failed => {
            if commit.assistant_message.is_some() {
                return false;
            }
            let Some(failure) = commit.turn.failure.clone() else {
                return false;
            };
            expected.fail(failure, transition_at)
        }
        ConversationTurnState::Cancelled => {
            if commit.assistant_message.is_some() {
                return false;
            }
            expected.cancel(transition_at)
        }
        ConversationTurnState::InterruptedNeedsReview => {
            if commit.assistant_message.is_some() {
                return false;
            }
            expected.interrupt(transition_at)
        }
        ConversationTurnState::Reserved | ConversationTurnState::ProviderStarted => return false,
    };
    transition.is_ok() && expected == commit.turn
}

const fn terminal_run_state(state: ConversationTurnState) -> Option<RunState> {
    match state {
        ConversationTurnState::Completed => Some(RunState::Completed),
        ConversationTurnState::Failed => Some(RunState::Failed),
        ConversationTurnState::Cancelled => Some(RunState::Cancelled),
        ConversationTurnState::InterruptedNeedsReview => Some(RunState::InterruptedNeedsReview),
        ConversationTurnState::Reserved | ConversationTurnState::ProviderStarted => None,
    }
}

fn expected_terminal_events(
    current: &ConversationTurnSnapshot,
    turn_state: ConversationTurnState,
    occurred_at: UnixMillis,
    run_state: RunState,
) -> Option<Vec<NewRunEvent>> {
    let mut events = Vec::new();
    if turn_state == ConversationTurnState::InterruptedNeedsReview {
        events.push(NewRunEvent {
            occurred_at,
            kind: RunEventKind::EffectNeedsReview {
                effect_id: current.effect.as_ref()?.id.clone(),
            },
        });
    }
    events.push(NewRunEvent {
        occurred_at,
        kind: RunEventKind::StateChanged {
            from: current.run.state,
            to: run_state,
        },
    });
    Some(events)
}

fn is_exact_terminal_effect_transition(
    current: &ConversationTurnSnapshot,
    commit: &TerminalTurnCommit,
    transition_at: UnixMillis,
) -> bool {
    match (
        current.turn.state,
        current.effect.as_ref(),
        commit.effect.as_ref(),
        commit.expected_effect_revision,
    ) {
        (ConversationTurnState::Reserved, None, None, None) => {
            commit.turn.state == ConversationTurnState::Cancelled
        }
        (
            ConversationTurnState::ProviderStarted,
            Some(current_effect),
            Some(next_effect),
            Some(expected_revision),
        ) if expected_revision == current_effect.revision => {
            let mut expected_effect = current_effect.clone();
            let transition = match commit.turn.state {
                ConversationTurnState::Completed => expected_effect.finish(true, transition_at),
                ConversationTurnState::Failed => expected_effect.finish(false, transition_at),
                ConversationTurnState::InterruptedNeedsReview => {
                    expected_effect.interrupt(transition_at)
                }
                ConversationTurnState::Reserved
                | ConversationTurnState::ProviderStarted
                | ConversationTurnState::Cancelled => return false,
            };
            transition.is_ok() && expected_effect == *next_effect
        }
        _ => false,
    }
}

fn is_canonical_conversation_snapshot(snapshot: &ConversationTurnSnapshot) -> bool {
    if ConversationTurn::restore(snapshot.turn.clone()).is_err()
        || ConversationTurnLineage::restore(snapshot.lineage.clone(), &snapshot.turn.id).is_err()
        || snapshot.turn.user_message_id != snapshot.user_message.id
        || snapshot.turn.thread_id != snapshot.user_message.thread_id
        || snapshot.user_message.sequence == 0
        || !is_reachable_turn_message(
            &snapshot.user_message,
            MessageRole::User,
            snapshot.turn.created_at,
            snapshot.user_message.sequence,
        )
    {
        return false;
    }

    canonical_run_for_snapshot(snapshot).is_some_and(|run| run == snapshot.run)
        && is_canonical_provider_effect(snapshot)
        && is_canonical_assistant_message(snapshot)
}

fn canonical_run_for_snapshot(snapshot: &ConversationTurnSnapshot) -> Option<Run> {
    let mut expected_run = Run::queued(
        snapshot.turn.run_id.clone(),
        snapshot.turn.project_id.clone(),
        snapshot.turn.thread_id.clone(),
        snapshot.turn.created_at,
    );
    match snapshot.turn.state {
        ConversationTurnState::Reserved | ConversationTurnState::Cancelled => {}
        ConversationTurnState::ProviderStarted
        | ConversationTurnState::Completed
        | ConversationTurnState::Failed
        | ConversationTurnState::InterruptedNeedsReview => {
            let effect = snapshot.effect.as_ref()?;
            expected_run
                .transition(RunState::Planning, effect.created_at)
                .and_then(|()| expected_run.transition(RunState::Running, effect.created_at))
                .ok()?;
            if snapshot.turn.state == ConversationTurnState::ProviderStarted
                && snapshot.turn.updated_at != effect.created_at
            {
                return None;
            }
        }
    }

    if let Some(state) = terminal_run_state(snapshot.turn.state) {
        expected_run
            .transition(state, snapshot.turn.updated_at)
            .ok()?;
    }
    Some(expected_run)
}

fn is_canonical_provider_effect(snapshot: &ConversationTurnSnapshot) -> bool {
    if matches!(
        snapshot.turn.state,
        ConversationTurnState::Reserved | ConversationTurnState::Cancelled
    ) {
        return snapshot.effect.is_none();
    }
    let Some(effect) = snapshot.effect.as_ref() else {
        return false;
    };
    let mut expected = SideEffect::prepare(
        effect.id.clone(),
        snapshot.turn.run_id.clone(),
        EffectKind::ExternalMutation,
        format!(
            "official xAI Responses API model {}",
            snapshot.turn.model_id
        ),
        Idempotency::NonIdempotent,
        effect.created_at,
    );
    if expected.start(effect.created_at).is_err() {
        return false;
    }
    let terminal_transition = match snapshot.turn.state {
        ConversationTurnState::ProviderStarted => None,
        ConversationTurnState::Completed => Some(expected.finish(true, snapshot.turn.updated_at)),
        ConversationTurnState::Failed => Some(expected.finish(false, snapshot.turn.updated_at)),
        ConversationTurnState::InterruptedNeedsReview => {
            Some(expected.interrupt(snapshot.turn.updated_at))
        }
        ConversationTurnState::Reserved | ConversationTurnState::Cancelled => return false,
    };
    terminal_transition.is_none_or(|result| result.is_ok())
        && expected == *effect
        && snapshot.turn.effect_id.as_ref() == Some(&effect.id)
}

fn is_canonical_assistant_message(snapshot: &ConversationTurnSnapshot) -> bool {
    match (&snapshot.turn.state, snapshot.assistant_message.as_ref()) {
        (ConversationTurnState::Completed, Some(assistant)) => {
            snapshot.turn.assistant_message_id.as_ref() == Some(&assistant.id)
                && assistant.thread_id == snapshot.turn.thread_id
                && assistant.sequence > snapshot.user_message.sequence
                && is_canonical_message(
                    assistant,
                    MessageRole::Assistant,
                    snapshot.turn.updated_at,
                    assistant.sequence,
                )
        }
        (ConversationTurnState::Completed, None) | (_, Some(_)) => false,
        (_, None) => true,
    }
}

fn is_canonical_message(
    message: &Message,
    role: MessageRole,
    created_at: UnixMillis,
    sequence: u64,
) -> bool {
    let Ok(mut canonical) = Message::new(
        message.id.clone(),
        message.thread_id.clone(),
        role,
        message.content.clone(),
        created_at,
    ) else {
        return false;
    };
    canonical.sequence = sequence;
    canonical == *message
}

fn is_reachable_turn_message(
    message: &Message,
    role: MessageRole,
    created_at: UnixMillis,
    sequence: u64,
) -> bool {
    message.role == role
        && message.created_at == created_at
        && message.sequence == sequence
        && message.state == MessageState::Active
        && Message::restore(message.clone()).is_ok()
}

fn conversation_snapshot(
    state: &State,
    turn: &ConversationTurn,
) -> Result<ConversationTurnSnapshot, StoreError> {
    let user_message = state
        .messages
        .get(&turn.user_message_id)
        .cloned()
        .ok_or_else(|| StoreError::Internal("conversation turn lost its user message".into()))?;
    let assistant_message = turn
        .assistant_message_id
        .as_ref()
        .map(|id| {
            state.messages.get(id).cloned().ok_or_else(|| {
                StoreError::Internal("conversation turn lost its assistant message".into())
            })
        })
        .transpose()?;
    let run = state
        .runs
        .get(&turn.run_id)
        .cloned()
        .ok_or_else(|| StoreError::Internal("conversation turn lost its run".into()))?;
    let effect = turn
        .effect_id
        .as_ref()
        .map(|id| {
            state.effects.get(id).cloned().ok_or_else(|| {
                StoreError::Internal("conversation turn lost its provider effect".into())
            })
        })
        .transpose()?;
    let lineage = state
        .conversation_lineages
        .get(&turn.id)
        .cloned()
        .ok_or_else(|| StoreError::Internal("conversation turn lost its lineage".into()))
        .and_then(|lineage| {
            ConversationTurnLineage::restore(lineage, &turn.id)
                .map_err(|_| StoreError::Internal("conversation turn lineage is invalid".into()))
        })?;
    let snapshot = ConversationTurnSnapshot {
        turn: turn.clone(),
        user_message,
        assistant_message,
        run,
        effect,
        lineage,
    };
    let thread = state
        .threads
        .get(&snapshot.turn.thread_id)
        .ok_or_else(|| StoreError::Internal("conversation turn lost its thread".into()))?;
    if thread.project_id != snapshot.turn.project_id
        || !state.projects.contains_key(&snapshot.turn.project_id)
        || Thread::restore(thread.clone()).is_err()
        || validate_persisted_thread_lineage(state, thread).is_err()
        || !is_canonical_conversation_snapshot(&snapshot)
    {
        return Err(StoreError::Internal(
            "conversation snapshot is internally inconsistent".into(),
        ));
    }
    let event_log = conversation_event_log(state, &snapshot.turn.id)?;
    event_log
        .validate_snapshot(
            &snapshot.turn,
            snapshot
                .assistant_message
                .as_ref()
                .map(|message| message.content.as_str()),
        )
        .map_err(|error| {
            StoreError::Internal(format!("conversation event log is inconsistent: {error}"))
        })?;
    Ok(snapshot)
}

fn conversation_event_log(
    state: &State,
    turn_id: &ConversationTurnId,
) -> Result<ConversationTurnEventLog, StoreError> {
    let events = state
        .conversation_events
        .get(turn_id)
        .ok_or_else(|| StoreError::Internal("conversation turn lost its event log".into()))?;
    ConversationTurnEventLog::restore(turn_id.clone(), events).map_err(|error| {
        StoreError::Internal(format!("conversation event log is invalid: {error}"))
    })
}

fn split_conversation_text(text: &str) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        let mut end = remaining.len().min(MAX_CONVERSATION_TEXT_CHUNK_BYTES);
        while !remaining.is_char_boundary(end) {
            end -= 1;
        }
        let (chunk, tail) = remaining.split_at(end);
        chunks.push(chunk);
        remaining = tail;
    }
    chunks
}

fn replayed_text_append(
    events: &[ConversationTurnEvent],
    start_utf8_offset: u64,
    expected_text: &str,
) -> Option<Vec<ConversationTurnEvent>> {
    let mut replay = Vec::new();
    let mut actual = String::with_capacity(expected_text.len());
    let mut expected_offset = start_utf8_offset;
    let mut found_start = false;
    for event in events {
        let ConversationTurnEventKind::TextAppended {
            start_utf8_offset: event_offset,
            text,
        } = &event.kind
        else {
            if found_start {
                break;
            }
            continue;
        };
        if !found_start {
            if *event_offset != start_utf8_offset {
                continue;
            }
            found_start = true;
        }
        if *event_offset != expected_offset || actual.len() >= expected_text.len() {
            return None;
        }
        actual.push_str(text);
        expected_offset = expected_offset.checked_add(u64::try_from(text.len()).ok()?)?;
        replay.push(event.clone());
        if actual.len() >= expected_text.len() {
            break;
        }
    }
    (found_start && actual == expected_text).then_some(replay)
}

fn validate_context(context: &[Message]) -> Result<(), StoreError> {
    if context.is_empty() || context.len() > MAX_CONVERSATION_CONTEXT_MESSAGES {
        return Err(StoreError::Conflict);
    }
    let bytes = context
        .iter()
        .try_fold(0usize, |total, message| {
            total.checked_add(message.content.len())
        })
        .ok_or(StoreError::Conflict)?;
    if bytes > MAX_CONVERSATION_CONTEXT_BYTES {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

#[async_trait]
impl CredentialMutationStore for InMemoryExecutionStore {
    async fn resolve_credential_mutation(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<CredentialMutationReservation>, StoreError> {
        let state = self.state.lock().await;
        let key = (command.scope.clone(), command.key.clone());
        match state.credential_commands.get(&key) {
            Some(record) if record.fingerprint != command.fingerprint => Err(StoreError::Conflict),
            Some(record) => Ok(Some(record.outcome.map_or(
                CredentialMutationReservation::Pending,
                CredentialMutationReservation::Completed,
            ))),
            None => Ok(None),
        }
    }

    async fn begin_credential_mutation(
        &self,
        command: &MutationCommand,
    ) -> Result<CredentialMutationReservation, StoreError> {
        let mut state = self.state.lock().await;
        let key = (command.scope.clone(), command.key.clone());
        match state.credential_commands.get(&key) {
            Some(record) if record.fingerprint != command.fingerprint => Err(StoreError::Conflict),
            Some(record) => Ok(record.outcome.map_or(
                CredentialMutationReservation::Pending,
                CredentialMutationReservation::Completed,
            )),
            None => {
                state.credential_commands.insert(
                    key,
                    CredentialCommandRecord {
                        fingerprint: command.fingerprint,
                        outcome: None,
                    },
                );
                Ok(CredentialMutationReservation::NewlyReserved)
            }
        }
    }

    async fn complete_credential_mutation(
        &self,
        command: &MutationCommand,
        outcome: AccountState,
    ) -> Result<(), StoreError> {
        let mut state = self.state.lock().await;
        let record = state
            .credential_commands
            .get_mut(&(command.scope.clone(), command.key.clone()))
            .ok_or(StoreError::NotFound)?;
        if record.fingerprint != command.fingerprint {
            return Err(StoreError::Conflict);
        }
        match record.outcome {
            Some(existing) if existing != outcome => Err(StoreError::Conflict),
            Some(_) => Ok(()),
            None => {
                record.outcome = Some(outcome);
                Ok(())
            }
        }
    }
}

#[async_trait]
impl DesktopPreferencesStore for InMemoryExecutionStore {
    async fn resolve_desktop_preferences_mutation(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<DesktopPreferences>, StoreError> {
        let state = self.state.lock().await;
        match state
            .desktop_preference_commands
            .get(&(command.scope.clone(), command.key.clone()))
        {
            Some(record) if record.fingerprint == command.fingerprint => {
                Ok(Some(record.outcome.clone()))
            }
            Some(_) => Err(StoreError::Conflict),
            None => Ok(None),
        }
    }

    async fn get_desktop_preferences(&self) -> Result<DesktopPreferences, StoreError> {
        Ok(self.state.lock().await.desktop_preferences.clone())
    }

    async fn save_desktop_preferences(
        &self,
        preferences: DesktopPreferences,
        expected_revision: u64,
        command: &MutationCommand,
    ) -> Result<DesktopPreferences, StoreError> {
        let mut state = self.state.lock().await;
        if let Some(record) = state
            .desktop_preference_commands
            .get(&(command.scope.clone(), command.key.clone()))
        {
            return if record.fingerprint == command.fingerprint {
                Ok(record.outcome.clone())
            } else {
                Err(StoreError::Conflict)
            };
        }
        if command.scope != "update_desktop_preferences"
            || state.desktop_preferences.revision != expected_revision
            || preferences.revision
                != expected_revision
                    .checked_add(1)
                    .ok_or(StoreError::Conflict)?
        {
            return Err(StoreError::Conflict);
        }
        state.desktop_preferences = preferences.clone();
        state.desktop_preference_commands.insert(
            (command.scope.clone(), command.key.clone()),
            DesktopPreferenceCommandRecord {
                fingerprint: command.fingerprint,
                outcome: preferences.clone(),
            },
        );
        Ok(preferences)
    }
}

#[async_trait]
impl ChatModelPreferenceStore for InMemoryExecutionStore {
    async fn resolve_chat_model_preference_mutation(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<ChatModelPreference>, StoreError> {
        let state = self.state.lock().await;
        match state
            .chat_model_preference_commands
            .get(&(command.scope.clone(), command.key.clone()))
        {
            Some(record) if record.fingerprint == command.fingerprint => {
                Ok(Some(record.outcome.clone()))
            }
            Some(_) => Err(StoreError::Conflict),
            None => Ok(None),
        }
    }

    async fn get_chat_model_preference(&self) -> Result<ChatModelPreference, StoreError> {
        Ok(self.state.lock().await.chat_model_preference.clone())
    }

    async fn save_chat_model_preference(
        &self,
        preference: ChatModelPreference,
        expected_revision: u64,
        command: &MutationCommand,
    ) -> Result<ChatModelPreference, StoreError> {
        let mut state = self.state.lock().await;
        if let Some(record) = state
            .chat_model_preference_commands
            .get(&(command.scope.clone(), command.key.clone()))
        {
            return if record.fingerprint == command.fingerprint {
                Ok(record.outcome.clone())
            } else {
                Err(StoreError::Conflict)
            };
        }
        if command.scope != "select_chat_model"
            || state.chat_model_preference.revision != expected_revision
            || preference.revision
                != expected_revision
                    .checked_add(1)
                    .ok_or(StoreError::Conflict)?
        {
            return Err(StoreError::Conflict);
        }
        state.chat_model_preference = preference.clone();
        state.chat_model_preference_commands.insert(
            (command.scope.clone(), command.key.clone()),
            ChatModelPreferenceCommandRecord {
                fingerprint: command.fingerprint,
                outcome: preference.clone(),
            },
        );
        Ok(preference)
    }
}

#[async_trait]
impl PrivilegedOperationStore for InMemoryExecutionStore {
    async fn resolve_preparation(
        &self,
        intent: &grok_domain::PrivilegedOperationIntent,
    ) -> Result<Option<PrivilegedOperation>, StoreError> {
        let state = self.state.lock().await;
        let key = (
            intent.authority.grant_id.as_str().into(),
            intent.idempotency.key.as_str().into(),
        );
        let Some(id) = state.privileged_operation_keys.get(&key) else {
            return Ok(None);
        };
        let existing = state
            .privileged_operations
            .get(id)
            .cloned()
            .ok_or_else(invalid_privileged_journal)?;
        let existing = validate_privileged_operation(existing)?;
        if exact_privileged_intent(&existing, intent) {
            Ok(Some(existing))
        } else {
            Err(StoreError::Conflict)
        }
    }

    async fn prepare_with_payload(
        &self,
        operation: PrivilegedOperation,
        payload: Vec<u8>,
    ) -> Result<PrivilegedPreparation, StoreError> {
        if operation.state != PrivilegedOperationState::Prepared {
            return Err(invalid_privileged_journal());
        }
        validate_privileged_payload(&operation, &payload)?;
        let operation = validate_privileged_operation(operation)?;
        let mut state = self.state.lock().await;
        let key = privileged_operation_key(&operation);
        if let Some(id) = state.privileged_operation_keys.get(&key) {
            let existing = state
                .privileged_operations
                .get(id)
                .cloned()
                .ok_or_else(invalid_privileged_journal)?;
            let existing = validate_privileged_operation(existing)?;
            if !exact_privileged_replay(&existing, &operation) {
                return Err(StoreError::Conflict);
            }
            return Ok(PrivilegedPreparation {
                operation: existing,
                created: false,
            });
        }
        if state.privileged_operations.contains_key(&operation.id) {
            return Err(StoreError::Conflict);
        }
        state
            .privileged_operation_keys
            .insert(key, operation.id.clone());
        state
            .privileged_operation_payloads
            .insert(operation.id.clone(), payload);
        state
            .privileged_operations
            .insert(operation.id.clone(), operation.clone());
        Ok(PrivilegedPreparation {
            operation,
            created: true,
        })
    }

    async fn get_privileged_operation(
        &self,
        id: &PrivilegedOperationId,
    ) -> Result<PrivilegedOperation, StoreError> {
        let operation = self
            .state
            .lock()
            .await
            .privileged_operations
            .get(id)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        validate_privileged_operation(operation)
    }

    async fn begin_dispatch_with_attempt(
        &self,
        operation: PrivilegedOperation,
        expected_revision: u64,
        attempt: PrivilegedDispatchAttempt,
    ) -> Result<PrivilegedOperation, StoreError> {
        let operation = validate_privileged_operation(operation)?;
        validate_privileged_attempt(&operation, &attempt)?;
        let mut state = self.state.lock().await;
        let current = state
            .privileged_operations
            .get(&operation.id)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        let current = validate_privileged_operation(current)?;
        let mut expected = current.clone();
        if current.revision != expected_revision
            || expected.dispatch(attempt.started_at).is_err()
            || expected != operation
            || state
                .privileged_transport_ids
                .contains(&attempt.transport_operation_id)
        {
            return Err(StoreError::Conflict);
        }
        let attempts = state
            .privileged_operation_attempts
            .entry(operation.id.clone())
            .or_default();
        if attempts.len()
            != usize::try_from(attempt.sequence.saturating_sub(1)).unwrap_or(usize::MAX)
        {
            return Err(StoreError::Conflict);
        }
        attempts.push(StoredPrivilegedDispatchAttempt {
            attempt: attempt.clone(),
            completed_at: None,
        });
        state
            .privileged_transport_ids
            .insert(attempt.transport_operation_id);
        state
            .privileged_operations
            .insert(operation.id.clone(), operation.clone());
        Ok(operation)
    }

    async fn list_dispatching_for_recovery(
        &self,
        limit: usize,
    ) -> Result<Vec<PrivilegedRecoveryCandidate>, StoreError> {
        let state = self.state.lock().await;
        let mut operations = state
            .privileged_operations
            .values()
            .filter(|operation| operation.state == PrivilegedOperationState::Dispatching)
            .cloned()
            .collect::<Vec<_>>();
        operations.sort_by(|left, right| {
            (left.updated_at, left.id.as_str()).cmp(&(right.updated_at, right.id.as_str()))
        });
        operations
            .into_iter()
            .take(limit)
            .map(|operation| {
                let operation = validate_privileged_operation(operation)?;
                let attempt = state
                    .privileged_operation_attempts
                    .get(&operation.id)
                    .and_then(|attempts| attempts.last())
                    .ok_or_else(invalid_privileged_journal)?;
                validate_privileged_attempt(&operation, &attempt.attempt)?;
                if attempt.completed_at.is_some() {
                    return Err(invalid_privileged_journal());
                }
                Ok(PrivilegedRecoveryCandidate {
                    attempt_sequence: attempt.attempt.sequence,
                    attempt_started_at: attempt.attempt.started_at,
                    operation,
                })
            })
            .collect()
    }

    async fn recover_interrupted_attempt(
        &self,
        operation: PrivilegedOperation,
        expected_revision: u64,
        attempt_sequence: u32,
        completed_at: UnixMillis,
    ) -> Result<PrivilegedOperation, StoreError> {
        let operation = validate_privileged_operation(operation)?;
        let mut state = self.state.lock().await;
        let current = state
            .privileged_operations
            .get(&operation.id)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        let current = validate_privileged_operation(current)?;
        let mut expected = current.clone();
        if current.revision != expected_revision
            || current.attempt_count != attempt_sequence
            || expected.interrupt(completed_at).is_err()
            || expected != operation
        {
            return Err(StoreError::Conflict);
        }
        let attempt = state
            .privileged_operation_attempts
            .get_mut(&operation.id)
            .and_then(|attempts| attempts.last_mut())
            .ok_or_else(invalid_privileged_journal)?;
        if attempt.attempt.sequence != attempt_sequence
            || attempt.completed_at.is_some()
            || completed_at < attempt.attempt.started_at
        {
            return Err(StoreError::Conflict);
        }
        attempt.completed_at = Some(completed_at);
        state
            .privileged_operations
            .insert(operation.id.clone(), operation.clone());
        Ok(operation)
    }
}

fn validate_privileged_operation(
    operation: PrivilegedOperation,
) -> Result<PrivilegedOperation, StoreError> {
    PrivilegedOperation::restore(operation).map_err(|_| invalid_privileged_journal())
}

fn validate_privileged_payload(
    operation: &PrivilegedOperation,
    payload: &[u8],
) -> Result<(), StoreError> {
    if !(2..=8 * 1024 * 1024).contains(&payload.len())
        || Sha256::digest(payload).as_slice() != operation.payload_digest.as_bytes()
    {
        return Err(invalid_privileged_journal());
    }
    Ok(())
}

fn validate_privileged_attempt(
    operation: &PrivilegedOperation,
    attempt: &PrivilegedDispatchAttempt,
) -> Result<(), StoreError> {
    let valid_transport_id = (16..=128).contains(&attempt.transport_operation_id.len())
        && attempt
            .transport_operation_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'));
    let duration = attempt.deadline_unix_ms.checked_sub(attempt.started_at);
    if operation.state != PrivilegedOperationState::Dispatching
        || attempt.sequence != operation.attempt_count
        || attempt.started_at != operation.updated_at
        || !valid_transport_id
        || attempt.transport_operation_id == operation.id.as_str()
        || attempt.broker_boot_id == [0; 16]
        || attempt.guest_boot_id == [0; 16]
        || duration.is_none_or(|duration| duration == 0 || duration > 30_000)
    {
        return Err(invalid_privileged_journal());
    }
    Ok(())
}

fn privileged_operation_key(operation: &PrivilegedOperation) -> (String, String) {
    (
        operation.authority.grant_id.as_str().into(),
        operation.idempotency.key.as_str().into(),
    )
}

fn exact_privileged_replay(existing: &PrivilegedOperation, proposed: &PrivilegedOperation) -> bool {
    existing.kind == proposed.kind
        && existing.target == proposed.target
        && existing.payload_digest == proposed.payload_digest
        && existing.authority == proposed.authority
        && existing.idempotency == proposed.idempotency
        && existing.links == proposed.links
}

fn exact_privileged_intent(
    existing: &PrivilegedOperation,
    intent: &grok_domain::PrivilegedOperationIntent,
) -> bool {
    existing.kind == intent.kind
        && existing.target == intent.target
        && existing.payload_digest == intent.payload_digest
        && existing.authority == intent.authority
        && existing.idempotency == intent.idempotency
        && existing.links == intent.links
}

fn invalid_privileged_journal() -> StoreError {
    StoreError::Internal("invalid durable privileged-operation journal".into())
}

#[async_trait]
impl AutomationSchedulerStore for InMemoryExecutionStore {
    async fn acquire_automation_scheduler_lease(
        &self,
        owner_id: &AutomationSchedulerOwnerId,
        now: UnixMillis,
        ttl_ms: u64,
    ) -> Result<AutomationSchedulerLeaseAcquisition, StoreError> {
        if ttl_ms == 0 || ttl_ms > MAX_AUTOMATION_SCHEDULER_LEASE_MS {
            return Err(StoreError::Conflict);
        }
        let mut state = self.state.lock().await;
        if let Some(durable_floor) = automation_scheduler_durable_floor(&state)
            && now < durable_floor
        {
            return Ok(AutomationSchedulerLeaseAcquisition::ClockRegressed { durable_floor });
        }
        let Some(mut lease) = state.automation_scheduler_lease.clone() else {
            let lease = AutomationSchedulerLease::acquire(owner_id.clone(), 1, now, ttl_ms)
                .map_err(|_| StoreError::Conflict)?;
            state.automation_scheduler_lease = Some(lease.clone());
            return Ok(AutomationSchedulerLeaseAcquisition::Acquired {
                lease,
                continuous: false,
                continuity_started_at: now,
            });
        };
        AutomationSchedulerLease::restore(lease.clone())
            .map_err(|_| invalid_automation_scheduler_journal())?;
        if now < lease.renewed_at {
            return Ok(AutomationSchedulerLeaseAcquisition::ClockRegressed {
                durable_floor: lease.renewed_at,
            });
        }
        if now < lease.expires_at {
            if &lease.owner_id != owner_id {
                return Ok(AutomationSchedulerLeaseAcquisition::Busy { lease });
            }
            let continuity_started_at = lease.renewed_at;
            let token = lease.token();
            lease
                .renew(&token, now, ttl_ms)
                .map_err(|_| StoreError::Conflict)?;
            state.automation_scheduler_lease = Some(lease.clone());
            return Ok(AutomationSchedulerLeaseAcquisition::Acquired {
                lease,
                continuous: true,
                continuity_started_at,
            });
        }
        let next_fence = lease
            .fence
            .checked_add(1)
            .ok_or_else(invalid_automation_scheduler_journal)?;
        lease
            .take_over(owner_id.clone(), next_fence, now, ttl_ms)
            .map_err(|_| StoreError::Conflict)?;
        state.automation_scheduler_lease = Some(lease.clone());
        Ok(AutomationSchedulerLeaseAcquisition::Acquired {
            lease,
            continuous: false,
            continuity_started_at: now,
        })
    }

    async fn list_automation_schedule_candidates(
        &self,
        after: Option<&AutomationId>,
        limit: usize,
    ) -> Result<Vec<AutomationScheduleCandidate>, StoreError> {
        if limit == 0 || limit > MAX_AUTOMATION_SCHEDULER_TICK_DEFINITIONS.saturating_add(1) {
            return Err(StoreError::Conflict);
        }
        let state = self.state.lock().await;
        let mut automations = state
            .automations
            .values()
            .filter(|automation| {
                automation.state == AutomationState::Enabled
                    && state
                        .projects
                        .get(&automation.project_id)
                        .is_some_and(|project| project.state == ProjectState::Active)
            })
            .cloned()
            .collect::<Vec<_>>();
        automations.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
        automations
            .into_iter()
            .filter(|automation| after.is_none_or(|after| automation.id.as_str() > after.as_str()))
            .take(limit)
            .map(|automation| {
                let snapshot = automation_scheduler_snapshot(&automation)?;
                let cursor = matching_automation_schedule_cursor(&state, &automation, &snapshot)?;
                Ok(AutomationScheduleCandidate { automation, cursor })
            })
            .collect()
    }

    #[allow(clippy::too_many_lines)]
    async fn commit_automation_schedule_evaluation(
        &self,
        evaluation: AutomationScheduleEvaluationCommit,
    ) -> Result<AutomationScheduleEvaluationResult, StoreError> {
        let mut state = self.state.lock().await;
        validate_scheduler_command(&evaluation.command, AUTOMATION_EVALUATION_SCOPE)?;
        let command_key = (
            evaluation.command.scope.clone(),
            evaluation.command.key.clone(),
        );
        if let Some(record) = state
            .automation_schedule_evaluation_commands
            .get(&command_key)
        {
            return if record.fingerprint == evaluation.command.fingerprint
                && record.result.cursor.automation_id == evaluation.cursor.automation_id
            {
                Ok(record.result.clone())
            } else {
                Err(StoreError::Conflict)
            };
        }
        require_automation_scheduler_lease(&state, &evaluation.lease, evaluation.observed_at)?;
        if evaluation.occurrences.len() > MAX_AUTOMATION_SCHEDULER_EVALUATION_OCCURRENCES {
            return Err(StoreError::Conflict);
        }
        let automation = state
            .automations
            .get(&evaluation.cursor.automation_id)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        if automation.state != AutomationState::Enabled
            || automation.revision != evaluation.expected_automation_revision
            || state
                .projects
                .get(&automation.project_id)
                .is_none_or(|project| project.state != ProjectState::Active)
        {
            return Err(StoreError::Conflict);
        }
        let snapshot = automation_scheduler_snapshot(&automation)?;
        let current_cursor = matching_automation_schedule_cursor(&state, &automation, &snapshot)?;
        if current_cursor.as_ref().map(|cursor| cursor.revision)
            != evaluation.expected_cursor_revision
        {
            return Err(StoreError::Conflict);
        }
        let prior_evaluated_through = current_cursor
            .as_ref()
            .map(|cursor| cursor.evaluated_through);
        let initializing = current_cursor.is_none();
        if initializing
            && (evaluation.cursor.evaluated_through != automation.updated_at
                || !evaluation.occurrences.is_empty())
        {
            return Err(StoreError::Conflict);
        }
        let expected_cursor = match current_cursor {
            Some(mut cursor) => {
                cursor
                    .advance(
                        evaluation.cursor.evaluated_through,
                        evaluation.cursor.next_decision,
                        evaluation.observed_at,
                    )
                    .map_err(|_| StoreError::Conflict)?;
                cursor
            }
            None => AutomationScheduleCursor::new(
                automation.id.clone(),
                &snapshot,
                evaluation.cursor.evaluated_through,
                evaluation.cursor.next_decision,
                evaluation.observed_at,
            )
            .map_err(|_| StoreError::Conflict)?,
        };
        if expected_cursor != evaluation.cursor {
            return Err(StoreError::Conflict);
        }
        if !automation_schedule_cursor_is_valid(&evaluation.cursor, &snapshot) {
            return Err(StoreError::Conflict);
        }
        let allowed_slots = if let Some(prior_evaluated_through) = prior_evaluated_through {
            let calculation = snapshot
                .schedule
                .decisions_between(
                    &snapshot.timezone,
                    prior_evaluated_through,
                    evaluation.cursor.evaluated_through,
                    MAX_AUTOMATION_SCHEDULE_DECISIONS,
                )
                .map_err(|_| StoreError::Conflict)?;
            if calculation.truncated {
                return Err(StoreError::Conflict);
            }
            calculation
                .decisions
                .into_iter()
                .map(grok_domain::AutomationScheduleDecision::nominal_local)
                .collect::<HashSet<_>>()
        } else {
            HashSet::new()
        };

        let mut active_count = 0_usize;
        let mut queued_count = 0_usize;
        for occurrence in state
            .automation_occurrences
            .values()
            .filter(|occurrence| occurrence.automation_id == automation.id)
        {
            AutomationOccurrence::restore(occurrence.clone())
                .map_err(|_| invalid_automation_scheduler_journal())?;
            if automation_occurrence_is_active(occurrence.state) {
                active_count = active_count.saturating_add(1);
            } else if occurrence.state == AutomationOccurrenceState::QueuedOverlap {
                queued_count = queued_count.saturating_add(1);
            }
        }
        if active_count > 1 || queued_count > 1 || (active_count == 0 && queued_count != 0) {
            return Err(invalid_automation_scheduler_journal());
        }

        let mut staged_occurrences = Vec::with_capacity(evaluation.occurrences.len());
        let mut proposed_slots = HashSet::with_capacity(evaluation.occurrences.len());
        let mut prior_local = None;
        for proposed in evaluation.occurrences {
            validate_new_automation_occurrence(
                &proposed,
                &automation,
                &snapshot,
                &evaluation.cursor,
                evaluation.observed_at,
            )?;
            if prior_local.is_some_and(|prior| prior >= proposed.nominal_local)
                || !allowed_slots.contains(&proposed.nominal_local)
                || !proposed_slots.insert(proposed.slot())
                || state.automation_occurrences.contains_key(&proposed.id)
                || state.automation_occurrences.values().any(|stored| {
                    stored.automation_id == proposed.automation_id
                        && stored.slot() == proposed.slot()
                })
            {
                return Err(StoreError::Conflict);
            }
            prior_local = Some(proposed.nominal_local);
            let mut actual = proposed;
            if actual.state == AutomationOccurrenceState::Pending {
                if active_count == 0 {
                    active_count = 1;
                } else {
                    match actual.snapshot.overlap_policy {
                        OverlapPolicy::QueueOne if queued_count == 0 => {
                            actual
                                .queue_overlap(evaluation.observed_at)
                                .map_err(|_| StoreError::Conflict)?;
                            queued_count = 1;
                        }
                        OverlapPolicy::QueueOne | OverlapPolicy::Skip => {
                            actual
                                .skip_overlap(evaluation.observed_at)
                                .map_err(|_| StoreError::Conflict)?;
                        }
                    }
                }
            }
            staged_occurrences.push(actual);
        }

        let mut staged_history = state
            .automation_history
            .get(&automation.id)
            .cloned()
            .unwrap_or_default();
        validate_automation_history_sequence(&staged_history)?;
        for occurrence in &staged_occurrences {
            stage_scheduler_history(&mut staged_history, occurrence, evaluation.observed_at)?;
        }
        let result = AutomationScheduleEvaluationResult {
            cursor: evaluation.cursor.clone(),
            occurrences: staged_occurrences.clone(),
        };
        state
            .automation_schedule_cursors
            .insert(automation.id.clone(), evaluation.cursor);
        for occurrence in staged_occurrences {
            state
                .automation_occurrences
                .insert(occurrence.id.clone(), occurrence);
        }
        if !staged_history.is_empty() {
            state
                .automation_history
                .insert(automation.id.clone(), staged_history);
        }
        state.automation_schedule_evaluation_commands.insert(
            command_key,
            AutomationScheduleEvaluationCommandRecord {
                fingerprint: evaluation.command.fingerprint,
                result: result.clone(),
            },
        );
        Ok(result)
    }

    async fn get_automation_occurrence(
        &self,
        id: &AutomationOccurrenceId,
    ) -> Result<AutomationOccurrence, StoreError> {
        let occurrence = self
            .state
            .lock()
            .await
            .automation_occurrences
            .get(id)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        AutomationOccurrence::restore(occurrence)
            .map_err(|_| invalid_automation_scheduler_journal())
    }

    async fn list_automation_occurrences(
        &self,
        automation_id: &AutomationId,
        after: Option<&AutomationOccurrenceId>,
        limit: usize,
    ) -> Result<Vec<AutomationOccurrence>, StoreError> {
        if !(1..=MAX_AUTOMATION_OCCURRENCE_PAGE_SIZE).contains(&limit) {
            return Err(StoreError::Conflict);
        }
        let state = self.state.lock().await;
        if !state.automations.contains_key(automation_id) {
            return Err(StoreError::NotFound);
        }
        let mut occurrences = state
            .automation_occurrences
            .values()
            .filter(|occurrence| &occurrence.automation_id == automation_id)
            .cloned()
            .collect::<Vec<_>>();
        for occurrence in &occurrences {
            AutomationOccurrence::restore(occurrence.clone())
                .map_err(|_| invalid_automation_scheduler_journal())?;
        }
        occurrences.sort_by(|left, right| {
            left.snapshot
                .definition_revision
                .cmp(&right.snapshot.definition_revision)
                .then_with(|| left.nominal_local.cmp(&right.nominal_local))
                .then_with(|| left.id.as_str().cmp(right.id.as_str()))
        });
        after_position(
            occurrences,
            after.map(AutomationOccurrenceId::as_str),
            limit,
            |occurrence| occurrence.id.as_str(),
        )
    }

    async fn claim_automation_occurrence(
        &self,
        claim: ClaimAutomationOccurrence,
    ) -> Result<AutomationOccurrence, StoreError> {
        let mut state = self.state.lock().await;
        validate_scheduler_command(&claim.command, AUTOMATION_CLAIM_SCOPE)?;
        let command_key = (claim.command.scope.clone(), claim.command.key.clone());
        if let Some(record) = state.automation_occurrence_claim_commands.get(&command_key) {
            return if record.fingerprint == claim.command.fingerprint
                && record.occurrence_id == claim.occurrence_id
            {
                Ok(record.result.clone())
            } else {
                Err(StoreError::Conflict)
            };
        }
        let lease = require_automation_scheduler_lease(&state, &claim.lease, claim.claimed_at)?;
        let claim_duration = claim.expires_at.checked_sub(claim.claimed_at);
        if claim_duration
            .is_none_or(|duration| duration == 0 || duration > MAX_AUTOMATION_SCHEDULER_LEASE_MS)
            || claim.expires_at > lease.expires_at
        {
            return Err(StoreError::Conflict);
        }
        let current = state
            .automation_occurrences
            .get(&claim.occurrence_id)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        AutomationOccurrence::restore(current.clone())
            .map_err(|_| invalid_automation_scheduler_journal())?;
        if current.revision != claim.expected_revision
            || current.state != AutomationOccurrenceState::Pending
        {
            return Err(StoreError::Conflict);
        }
        let attempts = state
            .automation_occurrence_claim_attempts
            .get(&claim.occurrence_id)
            .cloned()
            .unwrap_or_default();
        validate_claim_attempt_sequence(&current, &attempts)?;
        if attempts
            .iter()
            .any(|attempt| attempt.completed_at.is_none())
        {
            return Err(invalid_automation_scheduler_journal());
        }
        let mut claimed = current;
        claimed
            .claim(&claim.lease, claim.claimed_at, claim.expires_at)
            .map_err(|_| StoreError::Conflict)?;
        let sequence = claimed.claim_attempt_count;
        if usize::try_from(sequence).ok() != Some(attempts.len().saturating_add(1)) {
            return Err(invalid_automation_scheduler_journal());
        }
        let attempt = AutomationOccurrenceClaimAttempt {
            occurrence_id: claimed.id.clone(),
            sequence,
            owner_id: claim.lease.owner_id.clone(),
            fence: claim.lease.fence,
            claimed_at: claim.claimed_at,
            expires_at: claim.expires_at,
            completed_at: None,
            completion: None,
            request_fingerprint: claim.command.fingerprint,
        };
        let mut staged_attempts = attempts;
        staged_attempts.push(attempt);
        state
            .automation_occurrences
            .insert(claimed.id.clone(), claimed.clone());
        state
            .automation_occurrence_claim_attempts
            .insert(claimed.id.clone(), staged_attempts);
        state.automation_occurrence_claim_commands.insert(
            command_key,
            AutomationOccurrenceClaimCommandRecord {
                fingerprint: claim.command.fingerprint,
                occurrence_id: claimed.id.clone(),
                result: claimed.clone(),
            },
        );
        Ok(claimed)
    }

    async fn recover_automation_occurrence_claims(
        &self,
        lease: &AutomationSchedulerLeaseToken,
        now: UnixMillis,
        limit: usize,
    ) -> Result<AutomationSchedulerRecoverySummary, StoreError> {
        if !(1..=MAX_AUTOMATION_SCHEDULER_RECOVERY_BATCH).contains(&limit) {
            return Err(StoreError::Conflict);
        }
        let mut state = self.state.lock().await;
        require_automation_scheduler_lease(&state, lease, now)?;
        let mut expired = state
            .automation_occurrences
            .values()
            .filter(|occurrence| {
                matches!(
                    occurrence.state,
                    AutomationOccurrenceState::Claimed | AutomationOccurrenceState::RunLinked
                ) && occurrence
                    .claim
                    .as_ref()
                    .is_some_and(|claim| claim.expires_at <= now)
            })
            .cloned()
            .collect::<Vec<_>>();
        expired.sort_by(|left, right| {
            left.claim
                .as_ref()
                .map(|claim| claim.expires_at)
                .cmp(&right.claim.as_ref().map(|claim| claim.expires_at))
                .then_with(|| left.id.as_str().cmp(right.id.as_str()))
        });
        let truncated = expired.len() > limit;
        expired.truncate(limit);

        let mut staged_occurrences = HashMap::new();
        let mut staged_attempts = HashMap::new();
        let mut summary = AutomationSchedulerRecoverySummary {
            truncated,
            ..AutomationSchedulerRecoverySummary::default()
        };
        for current in expired {
            AutomationOccurrence::restore(current.clone())
                .map_err(|_| invalid_automation_scheduler_journal())?;
            let mut occurrence = current.clone();
            let completion = match current.state {
                AutomationOccurrenceState::Claimed => {
                    occurrence
                        .release_expired_claim(now)
                        .map_err(|_| invalid_automation_scheduler_journal())?;
                    if occurrence.claim_attempt_count == MAX_AUTOMATION_OCCURRENCE_CLAIM_ATTEMPTS {
                        occurrence
                            .mark_claims_exhausted(now)
                            .map_err(|_| invalid_automation_scheduler_journal())?;
                        summary.attempts_exhausted = summary.attempts_exhausted.saturating_add(1);
                        AutomationOccurrenceClaimCompletion::AttemptsExhausted
                    } else {
                        summary.released_unlinked = summary.released_unlinked.saturating_add(1);
                        AutomationOccurrenceClaimCompletion::ExpiredUnlinked
                    }
                }
                AutomationOccurrenceState::RunLinked => {
                    let run_id = occurrence
                        .run_id
                        .clone()
                        .ok_or_else(invalid_automation_scheduler_journal)?;
                    occurrence
                        .interrupt(&run_id, now)
                        .map_err(|_| invalid_automation_scheduler_journal())?;
                    summary.interrupted_linked = summary.interrupted_linked.saturating_add(1);
                    AutomationOccurrenceClaimCompletion::RunLinked
                }
                _ => return Err(invalid_automation_scheduler_journal()),
            };
            let attempts = state
                .automation_occurrence_claim_attempts
                .get(&current.id)
                .cloned()
                .ok_or_else(invalid_automation_scheduler_journal)?;
            let attempts = complete_claim_attempt(&current, attempts, completion, now)?;
            if matches!(
                occurrence.state,
                AutomationOccurrenceState::InterruptedNeedsReview
            ) && let Some(queued) = sole_queued_occurrence(&state, &occurrence.automation_id)?
            {
                let mut queued = queued;
                queued
                    .promote_queued(now)
                    .map_err(|_| invalid_automation_scheduler_journal())?;
                staged_occurrences.insert(queued.id.clone(), queued);
            }
            staged_attempts.insert(current.id.clone(), attempts);
            staged_occurrences.insert(current.id, occurrence);
        }
        for (id, occurrence) in staged_occurrences {
            state.automation_occurrences.insert(id, occurrence);
        }
        for (id, attempts) in staged_attempts {
            state
                .automation_occurrence_claim_attempts
                .insert(id, attempts);
        }
        Ok(summary)
    }

    async fn automation_scheduler_journal_status(
        &self,
    ) -> Result<AutomationSchedulerJournalStatus, StoreError> {
        let state = self.state.lock().await;
        let lease = state
            .automation_scheduler_lease
            .clone()
            .map(AutomationSchedulerLease::restore)
            .transpose()
            .map_err(|_| invalid_automation_scheduler_journal())?;
        for cursor in state.automation_schedule_cursors.values() {
            AutomationScheduleCursor::restore(cursor.clone())
                .map_err(|_| invalid_automation_scheduler_journal())?;
        }
        let mut status = AutomationSchedulerJournalStatus {
            lease,
            cursor_count: u64::try_from(state.automation_schedule_cursors.len())
                .map_err(|_| invalid_automation_scheduler_journal())?,
            ..AutomationSchedulerJournalStatus::default()
        };
        for occurrence in state.automation_occurrences.values() {
            AutomationOccurrence::restore(occurrence.clone())
                .map_err(|_| invalid_automation_scheduler_journal())?;
            validate_claim_attempt_sequence(
                occurrence,
                state
                    .automation_occurrence_claim_attempts
                    .get(&occurrence.id)
                    .map_or(&[], Vec::as_slice),
            )?;
            match occurrence.state {
                AutomationOccurrenceState::Pending => {
                    status.pending_count = status.pending_count.saturating_add(1);
                }
                AutomationOccurrenceState::QueuedOverlap => {
                    status.queued_overlap_count = status.queued_overlap_count.saturating_add(1);
                }
                AutomationOccurrenceState::Claimed => {
                    status.claimed_count = status.claimed_count.saturating_add(1);
                }
                AutomationOccurrenceState::RunLinked => {
                    status.run_linked_count = status.run_linked_count.saturating_add(1);
                }
                AutomationOccurrenceState::InterruptedNeedsReview => {
                    status.needs_review_count = status.needs_review_count.saturating_add(1);
                }
                AutomationOccurrenceState::Succeeded
                | AutomationOccurrenceState::Failed
                | AutomationOccurrenceState::SkippedMissed
                | AutomationOccurrenceState::SkippedOverlap
                | AutomationOccurrenceState::SkippedInvalidLocalTime
                | AutomationOccurrenceState::Cancelled => {}
            }
        }
        Ok(status)
    }
}

fn validate_scheduler_command(
    command: &MutationCommand,
    expected_scope: &str,
) -> Result<(), StoreError> {
    if command.scope != expected_scope
        || command.key.is_empty()
        || command.key.len() > 128
        || command.key.chars().any(char::is_control)
    {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

fn require_automation_scheduler_lease<'a>(
    state: &'a State,
    token: &AutomationSchedulerLeaseToken,
    now: UnixMillis,
) -> Result<&'a AutomationSchedulerLease, StoreError> {
    let lease = state
        .automation_scheduler_lease
        .as_ref()
        .ok_or(StoreError::Conflict)?;
    AutomationSchedulerLease::restore(lease.clone())
        .map_err(|_| invalid_automation_scheduler_journal())?;
    if lease.owner_id != token.owner_id
        || lease.fence != token.fence
        || now < lease.renewed_at
        || !lease.is_valid_at(now)
    {
        return Err(StoreError::Conflict);
    }
    Ok(lease)
}

fn automation_scheduler_snapshot(
    automation: &Automation,
) -> Result<AutomationExecutionSnapshot, StoreError> {
    Automation::restore(automation.clone()).map_err(|_| invalid_automation_scheduler_journal())?;
    AutomationExecutionSnapshot::new(
        automation.revision,
        automation.project_id.clone(),
        automation.title.clone(),
        automation.prompt.clone(),
        automation.schedule.clone(),
        automation.timezone.clone(),
        automation.missed_run_policy,
        automation.overlap_policy,
    )
    .map_err(|_| invalid_automation_scheduler_journal())
}

fn automation_scheduler_durable_floor(state: &State) -> Option<UnixMillis> {
    state
        .automation_scheduler_lease
        .as_ref()
        .map(|lease| lease.renewed_at)
        .into_iter()
        .chain(
            state
                .automation_schedule_cursors
                .values()
                .flat_map(|cursor| [cursor.evaluated_through, cursor.updated_at]),
        )
        .chain(
            state
                .automation_occurrences
                .values()
                .map(|occurrence| occurrence.updated_at),
        )
        .chain(
            state
                .automation_occurrence_claim_attempts
                .values()
                .flatten()
                .flat_map(|attempt| [Some(attempt.claimed_at), attempt.completed_at])
                .flatten(),
        )
        .max()
}

fn automation_schedule_cursor_is_valid(
    cursor: &AutomationScheduleCursor,
    snapshot: &AutomationExecutionSnapshot,
) -> bool {
    let expected_next = snapshot
        .schedule
        .next_decision_after(&snapshot.timezone, cursor.evaluated_through)
        .ok();
    AutomationScheduleCursor::restore(cursor.clone()).is_ok()
        && cursor.definition_revision == snapshot.definition_revision
        && cursor.schedule_fingerprint == snapshot.schedule_fingerprint
        && cursor.calculator_version == snapshot.calculator_version
        && cursor.next_decision == expected_next
}

fn matching_automation_schedule_cursor(
    state: &State,
    automation: &Automation,
    snapshot: &AutomationExecutionSnapshot,
) -> Result<Option<AutomationScheduleCursor>, StoreError> {
    let Some(cursor) = state
        .automation_schedule_cursors
        .get(&automation.id)
        .cloned()
    else {
        return Ok(None);
    };
    if cursor.automation_id != automation.id
        || cursor.definition_revision != automation.revision
        || cursor.schedule_fingerprint != snapshot.schedule_fingerprint
        || cursor.calculator_version != snapshot.calculator_version
    {
        return Err(invalid_automation_scheduler_journal());
    }
    if !automation_schedule_cursor_is_valid(&cursor, snapshot) {
        return Err(invalid_automation_scheduler_journal());
    }
    Ok(Some(cursor))
}

fn validate_new_automation_occurrence(
    occurrence: &AutomationOccurrence,
    automation: &Automation,
    snapshot: &AutomationExecutionSnapshot,
    cursor: &AutomationScheduleCursor,
    observed_at: UnixMillis,
) -> Result<(), StoreError> {
    AutomationOccurrence::restore(occurrence.clone()).map_err(|_| StoreError::Conflict)?;
    let valid_initial_state = match occurrence.state {
        AutomationOccurrenceState::Pending | AutomationOccurrenceState::SkippedInvalidLocalTime => {
            occurrence.revision == 0
        }
        AutomationOccurrenceState::SkippedMissed => {
            occurrence.revision == 1 && snapshot.missed_run_policy == MissedRunPolicy::Skip
        }
        AutomationOccurrenceState::QueuedOverlap
        | AutomationOccurrenceState::Claimed
        | AutomationOccurrenceState::RunLinked
        | AutomationOccurrenceState::Succeeded
        | AutomationOccurrenceState::Failed
        | AutomationOccurrenceState::SkippedOverlap
        | AutomationOccurrenceState::InterruptedNeedsReview
        | AutomationOccurrenceState::Cancelled => false,
    };
    if !valid_initial_state
        || occurrence.id.as_str().is_empty()
        || occurrence.automation_id != automation.id
        || occurrence.snapshot != *snapshot
        || occurrence.snapshot.definition_revision != automation.revision
        || occurrence.created_at != observed_at
        || occurrence.updated_at != observed_at
        || occurrence.claim_attempt_count != 0
        || occurrence.occurrence_count == 0
        || usize::try_from(occurrence.occurrence_count)
            .map_or(true, |count| count > MAX_AUTOMATION_SCHEDULE_DECISIONS)
        || occurrence
            .scheduled_for
            .is_some_and(|scheduled_for| scheduled_for > cursor.evaluated_through)
        || (occurrence.occurrence_count > 1
            && snapshot.missed_run_policy == MissedRunPolicy::Skip
            && occurrence.state != AutomationOccurrenceState::SkippedMissed)
    {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

fn validate_automation_history_sequence(
    history: &[AutomationHistoryEntry],
) -> Result<(), StoreError> {
    for (index, entry) in history.iter().enumerate() {
        if entry.sequence != u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1) {
            return Err(invalid_automation_scheduler_journal());
        }
    }
    Ok(())
}

fn stage_scheduler_history(
    history: &mut Vec<AutomationHistoryEntry>,
    occurrence: &AutomationOccurrence,
    recorded_at: UnixMillis,
) -> Result<(), StoreError> {
    let (status, summary) = match occurrence.state {
        AutomationOccurrenceState::SkippedMissed => (
            AutomationHistoryStatus::SkippedMissed,
            AUTOMATION_SKIPPED_MISSED_SUMMARY,
        ),
        AutomationOccurrenceState::SkippedOverlap => (
            AutomationHistoryStatus::SkippedOverlap,
            AUTOMATION_SKIPPED_OVERLAP_SUMMARY,
        ),
        AutomationOccurrenceState::Pending
        | AutomationOccurrenceState::QueuedOverlap
        | AutomationOccurrenceState::Claimed
        | AutomationOccurrenceState::RunLinked
        | AutomationOccurrenceState::Succeeded
        | AutomationOccurrenceState::Failed
        | AutomationOccurrenceState::SkippedInvalidLocalTime
        | AutomationOccurrenceState::InterruptedNeedsReview
        | AutomationOccurrenceState::Cancelled => return Ok(()),
    };
    let scheduled_for = occurrence
        .scheduled_for
        .ok_or_else(invalid_automation_scheduler_journal)?;
    if let Some(existing) = history
        .iter()
        .find(|entry| entry.scheduled_for == scheduled_for)
    {
        return if existing.automation_id == occurrence.automation_id
            && existing.recorded_at == recorded_at
            && existing.status == status
            && existing.summary == summary
        {
            Ok(())
        } else {
            Err(StoreError::Conflict)
        };
    }
    let mut entry = AutomationHistoryEntry::new(
        occurrence.automation_id.clone(),
        scheduled_for,
        recorded_at,
        status,
        summary.into(),
    )
    .map_err(|_| StoreError::Conflict)?;
    entry.sequence = u64::try_from(history.len())
        .map_err(|_| invalid_automation_scheduler_journal())?
        .checked_add(1)
        .ok_or_else(invalid_automation_scheduler_journal)?;
    history.push(entry);
    Ok(())
}

fn validate_claim_attempt_sequence(
    occurrence: &AutomationOccurrence,
    attempts: &[AutomationOccurrenceClaimAttempt],
) -> Result<(), StoreError> {
    if attempts.len()
        != usize::try_from(occurrence.claim_attempt_count)
            .map_err(|_| invalid_automation_scheduler_journal())?
    {
        return Err(invalid_automation_scheduler_journal());
    }
    let mut open_count = 0_usize;
    for (index, attempt) in attempts.iter().enumerate() {
        let sequence = u32::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .ok_or_else(invalid_automation_scheduler_journal)?;
        if attempt.occurrence_id != occurrence.id
            || attempt.sequence != sequence
            || attempt.sequence > MAX_AUTOMATION_OCCURRENCE_CLAIM_ATTEMPTS
            || attempt.fence == 0
            || attempt.expires_at <= attempt.claimed_at
            || attempt.expires_at - attempt.claimed_at > MAX_AUTOMATION_SCHEDULER_LEASE_MS
            || (attempt.completed_at.is_none() != attempt.completion.is_none())
            || attempt
                .completed_at
                .is_some_and(|completed_at| completed_at < attempt.claimed_at)
        {
            return Err(invalid_automation_scheduler_journal());
        }
        if attempt.completed_at.is_none() {
            open_count = open_count.saturating_add(1);
            let Some(claim) = occurrence.claim.as_ref() else {
                return Err(invalid_automation_scheduler_journal());
            };
            if attempt.sequence != occurrence.claim_attempt_count
                || attempt.owner_id != claim.owner_id
                || attempt.fence != claim.fence
                || attempt.claimed_at != claim.claimed_at
                || attempt.expires_at != claim.expires_at
            {
                return Err(invalid_automation_scheduler_journal());
            }
        }
    }
    if open_count > 1
        || (matches!(
            occurrence.state,
            AutomationOccurrenceState::Claimed | AutomationOccurrenceState::RunLinked
        ) != (open_count == 1))
    {
        return Err(invalid_automation_scheduler_journal());
    }
    Ok(())
}

fn complete_claim_attempt(
    occurrence: &AutomationOccurrence,
    mut attempts: Vec<AutomationOccurrenceClaimAttempt>,
    completion: AutomationOccurrenceClaimCompletion,
    completed_at: UnixMillis,
) -> Result<Vec<AutomationOccurrenceClaimAttempt>, StoreError> {
    validate_claim_attempt_sequence(occurrence, &attempts)?;
    let attempt = attempts
        .last_mut()
        .ok_or_else(invalid_automation_scheduler_journal)?;
    if attempt.completed_at.is_some() || attempt.completion.is_some() {
        return Err(invalid_automation_scheduler_journal());
    }
    attempt.completed_at = Some(completed_at);
    attempt.completion = Some(completion);
    Ok(attempts)
}

fn sole_queued_occurrence(
    state: &State,
    automation_id: &AutomationId,
) -> Result<Option<AutomationOccurrence>, StoreError> {
    let mut queued = state
        .automation_occurrences
        .values()
        .filter(|occurrence| {
            &occurrence.automation_id == automation_id
                && occurrence.state == AutomationOccurrenceState::QueuedOverlap
        })
        .cloned();
    let first = queued.next();
    if queued.next().is_some() {
        return Err(invalid_automation_scheduler_journal());
    }
    Ok(first)
}

fn invalid_automation_scheduler_journal() -> StoreError {
    StoreError::Internal("invalid durable automation scheduler journal".into())
}

#[async_trait]
#[allow(clippy::too_many_lines)]
impl WorkspaceStore for InMemoryExecutionStore {
    async fn resolve_mutation(
        &self,
        scope: &str,
        command: &MutationCommand,
    ) -> Result<Option<String>, StoreError> {
        prior_command(&*self.state.lock().await, scope, command)
    }

    async fn create_project(
        &self,
        project: Project,
        command: &MutationCommand,
    ) -> Result<Project, StoreError> {
        let mut state = self.state.lock().await;
        if let Some(id) = prior_command(&state, "create_project", command)? {
            return state
                .projects
                .get(&ProjectId::new(id).map_err(|error| invalid_stored_id(&error))?)
                .cloned()
                .ok_or_else(missing_idempotent_result);
        }
        if state.projects.contains_key(&project.id) {
            return Err(StoreError::Conflict);
        }
        state.projects.insert(project.id.clone(), project.clone());
        record_command(&mut state, command, project.id.as_str());
        Ok(project)
    }

    async fn get_project(&self, id: &ProjectId) -> Result<Project, StoreError> {
        self.state
            .lock()
            .await
            .projects
            .get(id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn save_project(
        &self,
        project: Project,
        expected_revision: u64,
        command: &MutationCommand,
    ) -> Result<(), StoreError> {
        let mut state = self.state.lock().await;
        if prior_command(&state, &command.scope, command)?.is_some() {
            return Ok(());
        }
        revisioned_replace(
            &mut state.projects,
            project.id.clone(),
            project.clone(),
            expected_revision,
            |project| project.revision,
        )?;
        record_command(&mut state, command, project.id.as_str());
        Ok(())
    }

    async fn list_projects(
        &self,
        after: Option<&ProjectId>,
        limit: usize,
    ) -> Result<Vec<Project>, StoreError> {
        let state = self.state.lock().await;
        recent_page(
            state.projects.values().cloned().collect(),
            after.map(ProjectId::as_str),
            limit,
            |project| project.updated_at,
            |project| project.id.as_str(),
        )
    }

    async fn create_thread(
        &self,
        thread: Thread,
        command: &MutationCommand,
    ) -> Result<Thread, StoreError> {
        let mut state = self.state.lock().await;
        if let Some(id) = prior_command(&state, "create_thread", command)? {
            return state
                .threads
                .get(&ThreadId::new(id).map_err(|error| invalid_stored_id(&error))?)
                .cloned()
                .ok_or_else(missing_idempotent_result);
        }
        let project = state
            .projects
            .get(&thread.project_id)
            .ok_or(StoreError::NotFound)?;
        if project.state != ProjectState::Active
            || state.threads.contains_key(&thread.id)
            || Thread::restore(thread.clone()).is_err()
            || !matches!(thread.lineage.origin, ConversationThreadOrigin::Original)
        {
            return Err(StoreError::Conflict);
        }
        state.threads.insert(thread.id.clone(), thread.clone());
        record_command(&mut state, command, thread.id.as_str());
        Ok(thread)
    }

    async fn get_thread(&self, id: &ThreadId) -> Result<Thread, StoreError> {
        self.state
            .lock()
            .await
            .threads
            .get(id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn save_thread(
        &self,
        thread: Thread,
        expected_revision: u64,
        command: &MutationCommand,
    ) -> Result<(), StoreError> {
        let mut state = self.state.lock().await;
        if prior_command(&state, &command.scope, command)?.is_some() {
            return Ok(());
        }
        if command.scope == "update_thread"
            && state
                .projects
                .get(&thread.project_id)
                .is_none_or(|project| project.state != ProjectState::Active)
        {
            return Err(StoreError::Conflict);
        }
        let current = state.threads.get(&thread.id).ok_or(StoreError::NotFound)?;
        if current.project_id != thread.project_id || current.lineage != thread.lineage {
            return Err(StoreError::Conflict);
        }
        revisioned_replace(
            &mut state.threads,
            thread.id.clone(),
            thread.clone(),
            expected_revision,
            |thread| thread.revision,
        )?;
        record_command(&mut state, command, thread.id.as_str());
        Ok(())
    }

    async fn list_threads(
        &self,
        project_id: &ProjectId,
        after: Option<&ThreadId>,
        limit: usize,
    ) -> Result<Vec<Thread>, StoreError> {
        let state = self.state.lock().await;
        if !state.projects.contains_key(project_id) {
            return Err(StoreError::NotFound);
        }
        recent_page(
            state
                .threads
                .values()
                .filter(|thread| &thread.project_id == project_id)
                .cloned()
                .collect(),
            after.map(ThreadId::as_str),
            limit,
            |thread| thread.updated_at,
            |thread| thread.id.as_str(),
        )
    }

    async fn create_message(
        &self,
        mut message: Message,
        command: &MutationCommand,
    ) -> Result<Message, StoreError> {
        let mut state = self.state.lock().await;
        if let Some(id) = prior_command(&state, "create_message", command)? {
            return state
                .messages
                .get(&MessageId::new(id).map_err(|error| invalid_stored_id(&error))?)
                .cloned()
                .ok_or_else(missing_idempotent_result);
        }
        let thread = state
            .threads
            .get(&message.thread_id)
            .ok_or(StoreError::NotFound)?;
        let project = state
            .projects
            .get(&thread.project_id)
            .ok_or(StoreError::NotFound)?;
        if thread.state != ThreadState::Open
            || project.state != ProjectState::Active
            || state.messages.contains_key(&message.id)
            || !message.derivation.is_original()
            || state
                .conversation_turns
                .values()
                .any(|turn| turn.thread_id == message.thread_id && !turn.state.is_terminal())
        {
            return Err(StoreError::Conflict);
        }
        message.sequence = state
            .messages
            .values()
            .filter(|stored| stored.thread_id == message.thread_id)
            .map(|stored| stored.sequence)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| StoreError::Internal("message sequence exhausted".into()))?;
        state.messages.insert(message.id.clone(), message.clone());
        record_command(&mut state, command, message.id.as_str());
        Ok(message)
    }

    async fn get_message(&self, id: &MessageId) -> Result<Message, StoreError> {
        self.state
            .lock()
            .await
            .messages
            .get(id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn save_message(
        &self,
        message: Message,
        expected_revision: u64,
        command: &MutationCommand,
    ) -> Result<(), StoreError> {
        let mut state = self.state.lock().await;
        if prior_command(&state, &command.scope, command)?.is_some() {
            return Ok(());
        }
        if !message.derivation.is_original()
            || state.conversation_turns.values().any(|turn| {
                turn.user_message_id == message.id
                    || turn.assistant_message_id.as_ref() == Some(&message.id)
            })
            || state
                .conversation_contexts
                .values()
                .any(|context| context.iter().any(|item| item.id == message.id))
            || state
                .messages
                .get(&message.id)
                .is_some_and(|stored| !stored.derivation.is_original())
        {
            return Err(StoreError::Conflict);
        }
        if command.scope == "update_message" {
            let thread = state
                .threads
                .get(&message.thread_id)
                .ok_or(StoreError::NotFound)?;
            if thread.state != ThreadState::Open
                || state
                    .projects
                    .get(&thread.project_id)
                    .is_none_or(|project| project.state != ProjectState::Active)
            {
                return Err(StoreError::Conflict);
            }
        }
        revisioned_replace(
            &mut state.messages,
            message.id.clone(),
            message.clone(),
            expected_revision,
            |message| message.revision,
        )?;
        record_command(&mut state, command, message.id.as_str());
        Ok(())
    }

    async fn list_messages(
        &self,
        thread_id: &ThreadId,
        after: Option<&MessageId>,
        limit: usize,
    ) -> Result<Vec<Message>, StoreError> {
        let state = self.state.lock().await;
        if !state.threads.contains_key(thread_id) {
            return Err(StoreError::NotFound);
        }
        let mut messages = state
            .messages
            .values()
            .filter(|message| &message.thread_id == thread_id)
            .cloned()
            .collect::<Vec<_>>();
        messages.sort_by_key(|message| message.sequence);
        after_position(messages, after.map(MessageId::as_str), limit, |message| {
            message.id.as_str()
        })
    }

    async fn create_automation(
        &self,
        automation: Automation,
        command: &MutationCommand,
    ) -> Result<Automation, StoreError> {
        let mut state = self.state.lock().await;
        if let Some(id) = prior_command(&state, "create_automation", command)? {
            return state
                .automations
                .get(&AutomationId::new(id).map_err(|error| invalid_stored_id(&error))?)
                .cloned()
                .ok_or_else(missing_idempotent_result);
        }
        let project = state
            .projects
            .get(&automation.project_id)
            .ok_or(StoreError::NotFound)?;
        if project.state != ProjectState::Active || state.automations.contains_key(&automation.id) {
            return Err(StoreError::Conflict);
        }
        state
            .automations
            .insert(automation.id.clone(), automation.clone());
        record_command(&mut state, command, automation.id.as_str());
        Ok(automation)
    }

    async fn get_automation(&self, id: &AutomationId) -> Result<Automation, StoreError> {
        self.state
            .lock()
            .await
            .automations
            .get(id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn save_automation(
        &self,
        automation: Automation,
        expected_revision: u64,
        command: &MutationCommand,
    ) -> Result<(), StoreError> {
        let mut state = self.state.lock().await;
        if prior_command(&state, &command.scope, command)?.is_some() {
            return Ok(());
        }
        if command.scope == "update_automation"
            && state
                .projects
                .get(&automation.project_id)
                .is_none_or(|project| project.state != ProjectState::Active)
        {
            return Err(StoreError::Conflict);
        }
        if state
            .automation_schedule_cursors
            .contains_key(&automation.id)
        {
            return Err(StoreError::Conflict);
        }
        revisioned_replace(
            &mut state.automations,
            automation.id.clone(),
            automation.clone(),
            expected_revision,
            |automation| automation.revision,
        )?;
        record_command(&mut state, command, automation.id.as_str());
        Ok(())
    }

    async fn list_automations(
        &self,
        project_id: &ProjectId,
        after: Option<&AutomationId>,
        limit: usize,
    ) -> Result<Vec<Automation>, StoreError> {
        let state = self.state.lock().await;
        if !state.projects.contains_key(project_id) {
            return Err(StoreError::NotFound);
        }
        recent_page(
            state
                .automations
                .values()
                .filter(|automation| &automation.project_id == project_id)
                .cloned()
                .collect(),
            after.map(AutomationId::as_str),
            limit,
            |automation| automation.updated_at,
            |automation| automation.id.as_str(),
        )
    }

    async fn record_automation_history(
        &self,
        mut entry: AutomationHistoryEntry,
    ) -> Result<AutomationHistoryEntry, StoreError> {
        let mut state = self.state.lock().await;
        if !state.automations.contains_key(&entry.automation_id) {
            return Err(StoreError::NotFound);
        }
        let history = state
            .automation_history
            .entry(entry.automation_id.clone())
            .or_default();
        if let Some(existing) = history
            .iter()
            .find(|existing| existing.scheduled_for == entry.scheduled_for)
        {
            return Ok(existing.clone());
        }
        entry.sequence = u64::try_from(history.len())
            .unwrap_or(u64::MAX)
            .checked_add(1)
            .ok_or_else(|| StoreError::Internal("automation history exhausted".into()))?;
        history.push(entry.clone());
        Ok(entry)
    }

    async fn automation_history(
        &self,
        automation_id: &AutomationId,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<AutomationHistoryEntry>, StoreError> {
        let state = self.state.lock().await;
        if !state.automations.contains_key(automation_id) {
            return Err(StoreError::NotFound);
        }
        Ok(state
            .automation_history
            .get(automation_id)
            .into_iter()
            .flatten()
            .filter(|entry| entry.sequence > after_sequence)
            .take(limit)
            .cloned()
            .collect())
    }

    async fn search(
        &self,
        project_id: Option<&ProjectId>,
        query: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<WorkspaceSearchHit>, StoreError> {
        let state = self.state.lock().await;
        let phrases = query
            .split_whitespace()
            .map(search_tokens)
            .filter(|phrase| !phrase.is_empty())
            .collect::<Vec<_>>();
        if phrases.is_empty() {
            return Ok(Vec::new());
        }
        let mut hits = search_candidates(&state)
            .into_iter()
            .filter(|candidate| project_id.is_none_or(|id| &candidate.hit.project_id == id))
            .filter_map(|candidate| {
                search_relevance(&candidate, &phrases).map(|relevance| (candidate.hit, relevance))
            })
            .collect::<Vec<_>>();
        hits.sort_by(|left, right| {
            right
                .1
                .cmp(&left.1)
                .then_with(|| right.0.updated_at.cmp(&left.0.updated_at))
                .then_with(|| left.0.id.cmp(&right.0.id))
        });
        Ok(hits
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|(hit, _)| hit)
            .collect())
    }
}

#[async_trait]
impl ArtifactStore for InMemoryExecutionStore {
    async fn resolve_import(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<ArtifactImportPlan>, StoreError> {
        let state = self.state.lock().await;
        resolve_artifact_import(&state, command)
    }

    async fn reserve_import(
        &self,
        artifact: Artifact,
        command: &MutationCommand,
    ) -> Result<ArtifactImportReservation, StoreError> {
        let mut state = self.state.lock().await;
        if let Some(plan) = resolve_artifact_import(&state, command)? {
            return Ok(ArtifactImportReservation::ExactReplay(plan));
        }
        ensure_artifact_command_scope(command, "import_artifact")?;
        let plan = ArtifactImportPlan::prepared(artifact.clone())
            .map_err(|_| invalid_artifact_journal())?;
        ensure_active_artifact_owner(&state, &artifact)?;
        if state.active_artifact_import.is_some()
            || state.artifacts.contains_key(&artifact.id)
            || state.artifact_import_artifacts.contains_key(&artifact.id)
            || project_artifact_count(&state, &artifact.project_id)? >= MAX_PROJECT_ARTIFACT_COUNT
        {
            return Err(StoreError::Conflict);
        }
        let key = artifact_command_key(command);
        state
            .artifacts
            .insert(artifact.id.clone(), artifact.clone());
        state
            .artifact_import_artifacts
            .insert(artifact.id, key.clone());
        state.artifact_import_commands.insert(
            key.clone(),
            ArtifactImportCommandRecord {
                fingerprint: command.fingerprint,
                plan: plan.clone(),
            },
        );
        state.active_artifact_import = Some(key);
        Ok(ArtifactImportReservation::NewlyPrepared(plan))
    }

    async fn mark_content_ready(
        &self,
        artifact_id: &ArtifactId,
        expected_revision: u64,
        content: ArtifactVersion,
        now: UnixMillis,
    ) -> Result<ArtifactContentReadyResult, StoreError> {
        let mut state = self.state.lock().await;
        let key = active_import_key(&state, artifact_id)?;
        let mut plan = artifact_import_plan(&state, &key)?;
        if plan.revision != expected_revision
            || plan.state != ArtifactImportState::Prepared
            || content.artifact_id != *artifact_id
            || content.version != 1
        {
            return Err(StoreError::Conflict);
        }
        ArtifactVersion::restore(content.clone()).map_err(|_| invalid_artifact_journal())?;
        if let Some(failure) = artifact_content_quota_failure(&state, &plan, &content)? {
            return Ok(ArtifactContentReadyResult::QuotaExceeded { plan, failure });
        }
        plan.record_content_ready(content, now)
            .map_err(|_| StoreError::Conflict)?;
        replace_artifact_import_plan(&mut state, &key, plan.clone())?;
        Ok(ArtifactContentReadyResult::ContentReady(plan))
    }

    async fn commit_import(
        &self,
        artifact: Artifact,
        expected_artifact_revision: u64,
        expected_import_revision: u64,
        content: ArtifactVersion,
        now: UnixMillis,
    ) -> Result<ArtifactImportPlan, StoreError> {
        let mut state = self.state.lock().await;
        let key = active_import_key(&state, &artifact.id)?;
        let mut plan = artifact_import_plan(&state, &key)?;
        let stored_artifact = state
            .artifacts
            .get(&artifact.id)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        if plan.state != ArtifactImportState::ContentReady
            || plan.revision != expected_import_revision
            || stored_artifact.revision != expected_artifact_revision
            || plan.content.as_ref() != Some(&content)
            || artifact.updated_at != now
            || artifact.project_id != stored_artifact.project_id
            || artifact.thread_id != stored_artifact.thread_id
            || artifact.name != stored_artifact.name
            || artifact.created_at != stored_artifact.created_at
            || state
                .artifact_versions
                .contains_key(&(artifact.id.clone(), content.version))
            || state
                .artifact_retention
                .contains_key(&(artifact.id.clone(), content.version))
        {
            return Err(StoreError::Conflict);
        }
        Artifact::restore(artifact.clone()).map_err(|_| invalid_artifact_journal())?;
        ArtifactVersion::restore(content.clone()).map_err(|_| invalid_artifact_journal())?;
        plan.commit(artifact.clone(), now)
            .map_err(|_| StoreError::Conflict)?;
        ensure_artifact_reserved_quota(&state, &artifact.project_id)?;

        let retention = ArtifactRetentionRecord::retained(content.clone())
            .map_err(|_| invalid_artifact_journal())?;
        state
            .artifact_versions
            .insert((artifact.id.clone(), content.version), content);
        state
            .artifact_retention
            .insert((artifact.id.clone(), retention.content.version), retention);
        state.artifacts.insert(artifact.id.clone(), artifact);
        replace_artifact_import_plan(&mut state, &key, plan.clone())?;
        state.active_artifact_import = None;
        Ok(plan)
    }

    async fn fail_import(
        &self,
        artifact_id: &ArtifactId,
        expected_revision: u64,
        failure: ArtifactImportFailureCode,
        now: UnixMillis,
    ) -> Result<ArtifactImportPlan, StoreError> {
        let mut state = self.state.lock().await;
        let key = active_import_key(&state, artifact_id)?;
        let mut plan = artifact_import_plan(&state, &key)?;
        if plan.revision != expected_revision {
            return Err(StoreError::Conflict);
        }
        plan.fail(failure, now).map_err(|_| StoreError::Conflict)?;
        replace_artifact_import_plan(&mut state, &key, plan.clone())?;
        state.active_artifact_import = None;
        Ok(plan)
    }

    async fn list_incomplete_imports(
        &self,
        limit: usize,
    ) -> Result<Vec<ArtifactImportPlan>, StoreError> {
        let state = self.state.lock().await;
        let mut plans = state
            .artifact_import_commands
            .values()
            .filter(|record| {
                matches!(
                    record.plan.state,
                    ArtifactImportState::Prepared | ArtifactImportState::ContentReady
                )
            })
            .map(|record| validate_artifact_import_record(&state, record))
            .collect::<Result<Vec<_>, _>>()?;
        plans.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.artifact.id.cmp(&right.artifact.id))
        });
        plans.truncate(limit);
        Ok(plans)
    }

    async fn get_artifact(&self, id: &ArtifactId) -> Result<Artifact, StoreError> {
        let artifact = self
            .state
            .lock()
            .await
            .artifacts
            .get(id)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        Artifact::restore(artifact).map_err(|_| invalid_artifact_journal())
    }

    async fn list_artifacts(
        &self,
        project_id: &ProjectId,
        after: Option<&ArtifactId>,
        limit: usize,
    ) -> Result<Vec<Artifact>, StoreError> {
        let state = self.state.lock().await;
        if !state.projects.contains_key(project_id) {
            return Err(StoreError::NotFound);
        }
        let artifacts = state
            .artifacts
            .values()
            .filter(|artifact| &artifact.project_id == project_id)
            .cloned()
            .map(|artifact| Artifact::restore(artifact).map_err(|_| invalid_artifact_journal()))
            .collect::<Result<Vec<_>, _>>()?;
        recent_page(
            artifacts,
            after.map(ArtifactId::as_str),
            limit,
            |artifact| artifact.updated_at,
            |artifact| artifact.id.as_str(),
        )
    }

    async fn get_artifact_version(
        &self,
        artifact_id: &ArtifactId,
        version: u32,
    ) -> Result<ArtifactVersion, StoreError> {
        let state = self.state.lock().await;
        if !state.artifacts.contains_key(artifact_id) {
            return Err(StoreError::NotFound);
        }
        let content = state
            .artifact_versions
            .get(&(artifact_id.clone(), version))
            .cloned()
            .ok_or(StoreError::NotFound)?;
        ArtifactVersion::restore(content).map_err(|_| invalid_artifact_journal())
    }

    async fn quota_usage(&self, project_id: &ProjectId) -> Result<ArtifactQuotaUsage, StoreError> {
        let state = self.state.lock().await;
        artifact_quota_usage(&state, project_id)
    }

    async fn resolve_open(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<ArtifactOpenPlan>, StoreError> {
        let state = self.state.lock().await;
        ensure_artifact_command_scope(command, "open_artifact")?;
        let Some(record) = state
            .artifact_open_commands
            .get(&artifact_command_key(command))
        else {
            return Ok(None);
        };
        if record.fingerprint != command.fingerprint {
            return Err(StoreError::Conflict);
        }
        Ok(Some(validate_artifact_open_record(&state, record)?))
    }

    async fn prepare_open(
        &self,
        content: ArtifactVersion,
        command: &MutationCommand,
        now: UnixMillis,
    ) -> Result<ArtifactOpenReservation, StoreError> {
        let mut state = self.state.lock().await;
        ensure_artifact_command_scope(command, "open_artifact")?;
        let key = artifact_command_key(command);
        if let Some(record) = state.artifact_open_commands.get(&key) {
            if record.fingerprint != command.fingerprint {
                return Err(StoreError::Conflict);
            }
            let plan = validate_artifact_open_record(&state, record)?;
            return Ok(ArtifactOpenReservation::ExactReplay(plan));
        }
        ArtifactVersion::restore(content.clone()).map_err(|_| invalid_artifact_journal())?;
        let canonical = state
            .artifact_versions
            .get(&(content.artifact_id.clone(), content.version))
            .ok_or(StoreError::NotFound)?;
        let artifact = state
            .artifacts
            .get(&content.artifact_id)
            .ok_or(StoreError::NotFound)?;
        let retention = state
            .artifact_retention
            .get(&(content.artifact_id.clone(), content.version))
            .ok_or_else(invalid_artifact_journal)?;
        if canonical != &content
            || artifact.state != ArtifactState::Available
            || artifact.content.as_ref() != Some(&content.summary())
            || retention.state != ArtifactRetentionState::Retained
            || state
                .artifact_removal_artifacts
                .contains_key(&content.artifact_id)
            || state.active_artifact_open.is_some()
        {
            return Err(StoreError::Conflict);
        }
        let plan =
            ArtifactOpenPlan::prepared(content, now).map_err(|_| invalid_artifact_journal())?;
        state.artifact_open_commands.insert(
            key.clone(),
            ArtifactOpenCommandRecord {
                fingerprint: command.fingerprint,
                plan: plan.clone(),
            },
        );
        state.active_artifact_open = Some(key);
        Ok(ArtifactOpenReservation::NewlyPrepared(plan))
    }

    async fn mark_open_dispatching(
        &self,
        artifact_id: &ArtifactId,
        content_version: u32,
        expected_revision: u64,
        now: UnixMillis,
    ) -> Result<ArtifactOpenPlan, StoreError> {
        transition_artifact_open(
            self,
            artifact_id,
            content_version,
            expected_revision,
            now,
            MemoryOpenTransition::Dispatch,
        )
        .await
    }

    async fn complete_open(
        &self,
        artifact_id: &ArtifactId,
        content_version: u32,
        expected_revision: u64,
        now: UnixMillis,
    ) -> Result<ArtifactOpenPlan, StoreError> {
        transition_artifact_open(
            self,
            artifact_id,
            content_version,
            expected_revision,
            now,
            MemoryOpenTransition::Complete,
        )
        .await
    }

    async fn fail_open(
        &self,
        artifact_id: &ArtifactId,
        content_version: u32,
        expected_revision: u64,
        failure: ArtifactOpenFailureCode,
        now: UnixMillis,
    ) -> Result<ArtifactOpenPlan, StoreError> {
        transition_artifact_open(
            self,
            artifact_id,
            content_version,
            expected_revision,
            now,
            MemoryOpenTransition::Fail(failure),
        )
        .await
    }

    async fn interrupt_open(
        &self,
        artifact_id: &ArtifactId,
        content_version: u32,
        expected_revision: u64,
        now: UnixMillis,
    ) -> Result<ArtifactOpenPlan, StoreError> {
        transition_artifact_open(
            self,
            artifact_id,
            content_version,
            expected_revision,
            now,
            MemoryOpenTransition::Interrupt,
        )
        .await
    }

    async fn list_incomplete_opens(
        &self,
        limit: usize,
    ) -> Result<Vec<ArtifactOpenPlan>, StoreError> {
        let state = self.state.lock().await;
        let mut plans = state
            .artifact_open_commands
            .values()
            .filter(|record| {
                matches!(
                    record.plan.state,
                    ArtifactOpenState::Prepared | ArtifactOpenState::Dispatching
                )
            })
            .map(|record| validate_artifact_open_record(&state, record))
            .collect::<Result<Vec<_>, _>>()?;
        plans.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.content.artifact_id.cmp(&right.content.artifact_id))
                .then_with(|| left.content.version.cmp(&right.content.version))
        });
        plans.truncate(limit);
        Ok(plans)
    }

    async fn resolve_removal(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<ArtifactRemovalPlan>, StoreError> {
        let state = self.state.lock().await;
        resolve_artifact_removal(&state, command)
    }

    async fn reserve_removal(
        &self,
        artifact_id: &ArtifactId,
        expected_revision: u64,
        expected_content_version: u32,
        command: &MutationCommand,
        now: UnixMillis,
    ) -> Result<ArtifactRemovalReservation, StoreError> {
        let mut state = self.state.lock().await;
        if let Some(plan) = resolve_artifact_removal(&state, command)? {
            return Ok(ArtifactRemovalReservation::ExactReplay(plan));
        }
        ensure_artifact_command_scope(command, "remove_artifact")?;
        if state.active_artifact_removal.is_some()
            || state.artifact_removal_artifacts.contains_key(artifact_id)
            || state.active_artifact_open.is_some()
        {
            return Err(StoreError::Conflict);
        }
        let mut artifact = state
            .artifacts
            .get(artifact_id)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        if artifact.state != ArtifactState::Available
            || artifact.revision != expected_revision
            || artifact
                .content
                .as_ref()
                .is_none_or(|content| content.content_version != expected_content_version)
        {
            return Err(StoreError::Conflict);
        }
        let mut retention = state
            .artifact_retention
            .iter()
            .filter(|((candidate, _), _)| candidate == artifact_id)
            .map(|(key, record)| {
                ArtifactRetentionRecord::restore(record.clone())
                    .map(|record| (key.clone(), record))
                    .map_err(|_| invalid_artifact_journal())
            })
            .collect::<Result<Vec<_>, _>>()?;
        retention.sort_by_key(|((_, version), _)| *version);
        let version_count = state
            .artifact_versions
            .keys()
            .filter(|(candidate, _)| candidate == artifact_id)
            .count();
        if retention.is_empty()
            || retention.len() != version_count
            || retention
                .iter()
                .any(|(_, record)| record.state != ArtifactRetentionState::Retained)
            || retention
                .iter()
                .any(|(key, record)| state.artifact_versions.get(key) != Some(&record.content))
            || !retention
                .iter()
                .any(|((_, version), _)| *version == expected_content_version)
        {
            return Err(StoreError::Conflict);
        }
        artifact.remove(now).map_err(|_| StoreError::Conflict)?;
        let plan = ArtifactRemovalPlan::pending(artifact.clone())
            .map_err(|_| invalid_artifact_journal())?;
        for (_, record) in &mut retention {
            record.begin_purge(now).map_err(|_| StoreError::Conflict)?;
        }
        let key = artifact_command_key(command);
        state.artifacts.insert(artifact_id.clone(), artifact);
        for (retention_key, record) in retention {
            state.artifact_retention.insert(retention_key, record);
        }
        state
            .artifact_removal_artifacts
            .insert(artifact_id.clone(), key.clone());
        state.artifact_removal_commands.insert(
            key.clone(),
            ArtifactRemovalCommandRecord {
                fingerprint: command.fingerprint,
                plan: plan.clone(),
            },
        );
        state.active_artifact_removal = Some(key);
        Ok(ArtifactRemovalReservation::NewlyPending(plan))
    }

    async fn list_pending_removal_versions(
        &self,
        artifact_id: &ArtifactId,
        limit: usize,
    ) -> Result<Vec<ArtifactRetentionRecord>, StoreError> {
        let state = self.state.lock().await;
        let key = state
            .artifact_removal_artifacts
            .get(artifact_id)
            .ok_or(StoreError::NotFound)?;
        let plan = artifact_removal_plan(&state, key)?;
        if plan.state != ArtifactRemovalState::Pending {
            return Err(StoreError::Conflict);
        }
        let mut records = state
            .artifact_retention
            .iter()
            .filter(|((candidate, _), record)| {
                candidate == artifact_id && record.state == ArtifactRetentionState::PurgePending
            })
            .map(|(key, record)| {
                let record = ArtifactRetentionRecord::restore(record.clone())
                    .map_err(|_| invalid_artifact_journal())?;
                if state.artifact_versions.get(key) != Some(&record.content) {
                    return Err(invalid_artifact_journal());
                }
                Ok(record)
            })
            .collect::<Result<Vec<_>, _>>()?;
        records.sort_by_key(|record| record.content.version);
        records.truncate(limit);
        Ok(records)
    }

    async fn mark_content_purged(
        &self,
        artifact_id: &ArtifactId,
        content_version: u32,
        expected_revision: u64,
        now: UnixMillis,
    ) -> Result<ArtifactRetentionRecord, StoreError> {
        let mut state = self.state.lock().await;
        let removal_key = state
            .artifact_removal_artifacts
            .get(artifact_id)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        let plan = artifact_removal_plan(&state, &removal_key)?;
        if plan.state != ArtifactRemovalState::Pending
            || state.active_artifact_removal.as_ref() != Some(&removal_key)
        {
            return Err(StoreError::Conflict);
        }
        let key = (artifact_id.clone(), content_version);
        let mut record = state
            .artifact_retention
            .get(&key)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        if record.revision != expected_revision
            || state.artifact_versions.get(&key) != Some(&record.content)
        {
            return Err(StoreError::Conflict);
        }
        record
            .record_purged(now)
            .map_err(|_| StoreError::Conflict)?;
        state.artifact_retention.insert(key, record.clone());
        Ok(record)
    }

    async fn commit_removal(
        &self,
        artifact_id: &ArtifactId,
        expected_revision: u64,
        now: UnixMillis,
    ) -> Result<ArtifactRemovalPlan, StoreError> {
        let mut state = self.state.lock().await;
        let key = state
            .artifact_removal_artifacts
            .get(artifact_id)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        if state.active_artifact_removal.as_ref() != Some(&key) {
            return Err(StoreError::Conflict);
        }
        let mut plan = artifact_removal_plan(&state, &key)?;
        if plan.revision != expected_revision
            || plan.state != ArtifactRemovalState::Pending
            || state
                .artifact_versions
                .iter()
                .filter(|((candidate, _), _)| candidate == artifact_id)
                .any(|(key, content)| {
                    state.artifact_retention.get(key).is_none_or(|record| {
                        record.content != *content || record.state != ArtifactRetentionState::Purged
                    })
                })
            || state
                .artifact_retention
                .keys()
                .filter(|(candidate, _)| candidate == artifact_id)
                .any(|key| !state.artifact_versions.contains_key(key))
        {
            return Err(StoreError::Conflict);
        }
        plan.commit(now).map_err(|_| StoreError::Conflict)?;
        let record = state
            .artifact_removal_commands
            .get_mut(&key)
            .ok_or_else(invalid_artifact_journal)?;
        record.plan = plan.clone();
        state.active_artifact_removal = None;
        Ok(plan)
    }

    async fn list_incomplete_removals(
        &self,
        limit: usize,
    ) -> Result<Vec<ArtifactRemovalPlan>, StoreError> {
        let state = self.state.lock().await;
        let mut plans = state
            .artifact_removal_commands
            .values()
            .filter(|record| record.plan.state == ArtifactRemovalState::Pending)
            .map(|record| validate_artifact_removal_record(&state, record))
            .collect::<Result<Vec<_>, _>>()?;
        plans.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.artifact.id.cmp(&right.artifact.id))
        });
        plans.truncate(limit);
        Ok(plans)
    }
}

enum MemoryOpenTransition {
    Dispatch,
    Complete,
    Fail(ArtifactOpenFailureCode),
    Interrupt,
}

async fn transition_artifact_open(
    store: &InMemoryExecutionStore,
    artifact_id: &ArtifactId,
    content_version: u32,
    expected_revision: u64,
    now: UnixMillis,
    transition: MemoryOpenTransition,
) -> Result<ArtifactOpenPlan, StoreError> {
    let mut state = store.state.lock().await;
    let key = state
        .active_artifact_open
        .clone()
        .ok_or(StoreError::NotFound)?;
    let mut plan = state
        .artifact_open_commands
        .get(&key)
        .map(|record| validate_artifact_open_record(&state, record))
        .transpose()?
        .ok_or_else(invalid_artifact_journal)?;
    if plan.content.artifact_id != *artifact_id
        || plan.content.version != content_version
        || plan.revision != expected_revision
    {
        return Err(StoreError::Conflict);
    }
    match transition {
        MemoryOpenTransition::Dispatch => plan.begin_dispatch(now),
        MemoryOpenTransition::Complete => plan.complete(now),
        MemoryOpenTransition::Fail(failure) => plan.fail(failure, now),
        MemoryOpenTransition::Interrupt => plan.interrupt(now),
    }
    .map_err(|_| StoreError::Conflict)?;
    let terminal = matches!(
        plan.state,
        ArtifactOpenState::Opened
            | ArtifactOpenState::Failed
            | ArtifactOpenState::InterruptedNeedsReview
    );
    let record = state
        .artifact_open_commands
        .get_mut(&key)
        .ok_or_else(invalid_artifact_journal)?;
    record.plan = plan.clone();
    if terminal {
        state.active_artifact_open = None;
    }
    Ok(plan)
}

fn resolve_artifact_import(
    state: &State,
    command: &MutationCommand,
) -> Result<Option<ArtifactImportPlan>, StoreError> {
    ensure_artifact_command_scope(command, "import_artifact")?;
    let Some(record) = state
        .artifact_import_commands
        .get(&artifact_command_key(command))
    else {
        return Ok(None);
    };
    if record.fingerprint != command.fingerprint {
        return Err(StoreError::Conflict);
    }
    Ok(Some(validate_artifact_import_record(state, record)?))
}

fn validate_artifact_import_record(
    state: &State,
    record: &ArtifactImportCommandRecord,
) -> Result<ArtifactImportPlan, StoreError> {
    let plan =
        ArtifactImportPlan::restore(record.plan.clone()).map_err(|_| invalid_artifact_journal())?;
    if state.artifacts.get(&plan.artifact.id) != Some(&plan.artifact) {
        return Err(invalid_artifact_journal());
    }
    Ok(plan)
}

fn artifact_import_plan(
    state: &State,
    key: &(String, String),
) -> Result<ArtifactImportPlan, StoreError> {
    state
        .artifact_import_commands
        .get(key)
        .map(|record| validate_artifact_import_record(state, record))
        .transpose()?
        .ok_or_else(invalid_artifact_journal)
}

fn replace_artifact_import_plan(
    state: &mut State,
    key: &(String, String),
    plan: ArtifactImportPlan,
) -> Result<(), StoreError> {
    ArtifactImportPlan::restore(plan.clone()).map_err(|_| invalid_artifact_journal())?;
    let record = state
        .artifact_import_commands
        .get_mut(key)
        .ok_or_else(invalid_artifact_journal)?;
    record.plan = plan;
    Ok(())
}

fn active_import_key(
    state: &State,
    artifact_id: &ArtifactId,
) -> Result<(String, String), StoreError> {
    let key = state
        .artifact_import_artifacts
        .get(artifact_id)
        .cloned()
        .ok_or(StoreError::NotFound)?;
    if state.active_artifact_import.as_ref() != Some(&key) {
        return Err(StoreError::Conflict);
    }
    Ok(key)
}

fn validate_artifact_open_record(
    state: &State,
    record: &ArtifactOpenCommandRecord,
) -> Result<ArtifactOpenPlan, StoreError> {
    let plan =
        ArtifactOpenPlan::restore(record.plan.clone()).map_err(|_| invalid_artifact_journal())?;
    if state
        .artifact_versions
        .get(&(plan.content.artifact_id.clone(), plan.content.version))
        != Some(&plan.content)
    {
        return Err(invalid_artifact_journal());
    }
    Ok(plan)
}

fn resolve_artifact_removal(
    state: &State,
    command: &MutationCommand,
) -> Result<Option<ArtifactRemovalPlan>, StoreError> {
    ensure_artifact_command_scope(command, "remove_artifact")?;
    let Some(record) = state
        .artifact_removal_commands
        .get(&artifact_command_key(command))
    else {
        return Ok(None);
    };
    if record.fingerprint != command.fingerprint {
        return Err(StoreError::Conflict);
    }
    Ok(Some(validate_artifact_removal_record(state, record)?))
}

fn validate_artifact_removal_record(
    state: &State,
    record: &ArtifactRemovalCommandRecord,
) -> Result<ArtifactRemovalPlan, StoreError> {
    let plan = ArtifactRemovalPlan::restore(record.plan.clone())
        .map_err(|_| invalid_artifact_journal())?;
    if state.artifacts.get(&plan.artifact.id) != Some(&plan.artifact) {
        return Err(invalid_artifact_journal());
    }
    let key = state
        .artifact_removal_artifacts
        .get(&plan.artifact.id)
        .ok_or_else(invalid_artifact_journal)?;
    if state
        .artifact_removal_commands
        .get(key)
        .is_none_or(|stored| stored.fingerprint != record.fingerprint || stored.plan != record.plan)
    {
        return Err(invalid_artifact_journal());
    }
    let active = state.active_artifact_removal.as_ref() == Some(key);
    if active != (plan.state == ArtifactRemovalState::Pending) {
        return Err(invalid_artifact_journal());
    }
    let mut version_count = 0_usize;
    for (retention_key, content) in state
        .artifact_versions
        .iter()
        .filter(|((artifact_id, _), _)| artifact_id == &plan.artifact.id)
    {
        version_count = version_count
            .checked_add(1)
            .ok_or_else(invalid_artifact_journal)?;
        let retention = state
            .artifact_retention
            .get(retention_key)
            .cloned()
            .ok_or_else(invalid_artifact_journal)
            .and_then(|record| {
                ArtifactRetentionRecord::restore(record).map_err(|_| invalid_artifact_journal())
            })?;
        let state_is_valid = match plan.state {
            ArtifactRemovalState::Pending => matches!(
                retention.state,
                ArtifactRetentionState::PurgePending | ArtifactRetentionState::Purged
            ),
            ArtifactRemovalState::Committed => retention.state == ArtifactRetentionState::Purged,
        };
        if retention.content != *content || !state_is_valid {
            return Err(invalid_artifact_journal());
        }
    }
    if version_count == 0
        || state
            .artifact_retention
            .keys()
            .filter(|(artifact_id, _)| artifact_id == &plan.artifact.id)
            .count()
            != version_count
    {
        return Err(invalid_artifact_journal());
    }
    Ok(plan)
}

fn artifact_removal_plan(
    state: &State,
    key: &(String, String),
) -> Result<ArtifactRemovalPlan, StoreError> {
    state
        .artifact_removal_commands
        .get(key)
        .map(|record| validate_artifact_removal_record(state, record))
        .transpose()?
        .ok_or_else(invalid_artifact_journal)
}

fn ensure_artifact_command_scope(
    command: &MutationCommand,
    expected: &str,
) -> Result<(), StoreError> {
    if command.scope != expected {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

fn artifact_command_key(command: &MutationCommand) -> (String, String) {
    (command.scope.clone(), command.key.clone())
}

fn ensure_active_artifact_owner(state: &State, artifact: &Artifact) -> Result<(), StoreError> {
    let project = state
        .projects
        .get(&artifact.project_id)
        .ok_or(StoreError::NotFound)?;
    if project.state != ProjectState::Active {
        return Err(StoreError::Conflict);
    }
    if let Some(thread_id) = &artifact.thread_id {
        let matches = state.threads.get(thread_id).is_some_and(|thread| {
            thread.project_id == artifact.project_id && thread.state == ThreadState::Open
        });
        if !matches {
            return Err(StoreError::Conflict);
        }
    }
    Ok(())
}

fn project_artifact_count(state: &State, project_id: &ProjectId) -> Result<u64, StoreError> {
    u64::try_from(
        state
            .artifacts
            .values()
            .filter(|artifact| {
                &artifact.project_id == project_id && artifact.state != ArtifactState::Deleted
            })
            .count(),
    )
    .map_err(|_| invalid_artifact_journal())
}

fn artifact_quota_usage(
    state: &State,
    project_id: &ProjectId,
) -> Result<ArtifactQuotaUsage, StoreError> {
    if !state.projects.contains_key(project_id) {
        return Err(StoreError::NotFound);
    }
    Ok(ArtifactQuotaUsage {
        project_artifact_count: project_artifact_count(state, project_id)?,
        project_bytes: artifact_committed_bytes(state, Some(project_id))?,
        global_bytes: artifact_committed_bytes(state, None)?,
    })
}

fn artifact_content_quota_failure(
    state: &State,
    plan: &ArtifactImportPlan,
    content: &ArtifactVersion,
) -> Result<Option<ArtifactImportFailureCode>, StoreError> {
    if content.byte_size > MAX_ARTIFACT_FILE_BYTES {
        return Ok(Some(ArtifactImportFailureCode::FileTooLarge));
    }
    let project_total = artifact_committed_bytes(state, Some(&plan.artifact.project_id))?
        .checked_add(artifact_reserved_bytes(
            state,
            Some(&plan.artifact.project_id),
        )?)
        .and_then(|value| value.checked_add(content.byte_size))
        .ok_or_else(invalid_artifact_journal)?;
    if project_total > MAX_PROJECT_ARTIFACT_BYTES {
        return Ok(Some(ArtifactImportFailureCode::ProjectByteQuotaExceeded));
    }
    let global_total = artifact_committed_bytes(state, None)?
        .checked_add(artifact_reserved_bytes(state, None)?)
        .and_then(|value| value.checked_add(content.byte_size))
        .ok_or_else(invalid_artifact_journal)?;
    if global_total > MAX_GLOBAL_ARTIFACT_BYTES {
        return Ok(Some(ArtifactImportFailureCode::GlobalByteQuotaExceeded));
    }
    Ok(None)
}

fn ensure_artifact_reserved_quota(state: &State, project_id: &ProjectId) -> Result<(), StoreError> {
    let project_total = artifact_committed_bytes(state, Some(project_id))?
        .checked_add(artifact_reserved_bytes(state, Some(project_id))?)
        .ok_or_else(invalid_artifact_journal)?;
    let global_total = artifact_committed_bytes(state, None)?
        .checked_add(artifact_reserved_bytes(state, None)?)
        .ok_or_else(invalid_artifact_journal)?;
    if project_total > MAX_PROJECT_ARTIFACT_BYTES || global_total > MAX_GLOBAL_ARTIFACT_BYTES {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

fn artifact_committed_bytes(
    state: &State,
    project_id: Option<&ProjectId>,
) -> Result<u64, StoreError> {
    state
        .artifact_versions
        .values()
        .try_fold(0_u64, |total, content| {
            let retention = state
                .artifact_retention
                .get(&(content.artifact_id.clone(), content.version))
                .cloned()
                .ok_or_else(invalid_artifact_journal)
                .and_then(|record| {
                    ArtifactRetentionRecord::restore(record).map_err(|_| invalid_artifact_journal())
                })?;
            if retention.content != *content {
                return Err(invalid_artifact_journal());
            }
            if retention.state == ArtifactRetentionState::Purged {
                return Ok(total);
            }
            let artifact = state
                .artifacts
                .get(&content.artifact_id)
                .ok_or_else(invalid_artifact_journal)?;
            if project_id.is_some_and(|project_id| &artifact.project_id != project_id) {
                return Ok(total);
            }
            total
                .checked_add(content.byte_size)
                .ok_or_else(invalid_artifact_journal)
        })
}

fn artifact_reserved_bytes(
    state: &State,
    project_id: Option<&ProjectId>,
) -> Result<u64, StoreError> {
    state
        .artifact_import_commands
        .values()
        .filter(|record| record.plan.state == ArtifactImportState::ContentReady)
        .try_fold(0_u64, |total, record| {
            let plan = validate_artifact_import_record(state, record)?;
            if project_id.is_some_and(|project_id| &plan.artifact.project_id != project_id) {
                return Ok(total);
            }
            total
                .checked_add(
                    plan.content
                        .as_ref()
                        .ok_or_else(invalid_artifact_journal)?
                        .byte_size,
                )
                .ok_or_else(invalid_artifact_journal)
        })
}

fn invalid_artifact_journal() -> StoreError {
    StoreError::Internal("invalid durable artifact journal".into())
}

fn prior_command(
    state: &State,
    scope: &str,
    command: &MutationCommand,
) -> Result<Option<String>, StoreError> {
    if scope != command.scope {
        return Err(StoreError::Conflict);
    }
    let record = state
        .workspace_commands
        .get(&(scope.to_owned(), command.key.clone()));
    match record {
        Some(record) if record.fingerprint == command.fingerprint => {
            Ok(Some(record.entity_id.clone()))
        }
        Some(_) => Err(StoreError::Conflict),
        None => Ok(None),
    }
}

fn prior_execution_command(
    state: &State,
    command: &MutationCommand,
) -> Result<Option<ExecutionMutationOutcome>, StoreError> {
    let record = state
        .execution_commands
        .get(&(command.scope.clone(), command.key.clone()));
    match record {
        Some(record) if record.fingerprint == command.fingerprint => {
            Ok(Some(record.outcome.clone()))
        }
        Some(_) => Err(StoreError::Conflict),
        None => Ok(None),
    }
}

fn record_execution_command(
    state: &mut State,
    command: &MutationCommand,
    outcome: ExecutionMutationOutcome,
) {
    state.execution_commands.insert(
        (command.scope.clone(), command.key.clone()),
        ExecutionCommandRecord {
            fingerprint: command.fingerprint,
            outcome,
        },
    );
}

fn run_outcome(outcome: ExecutionMutationOutcome) -> Result<Run, StoreError> {
    match outcome {
        ExecutionMutationOutcome::Run(run) => Ok(run),
        ExecutionMutationOutcome::Approval(_) => Err(missing_idempotent_result()),
    }
}

fn approval_outcome(outcome: ExecutionMutationOutcome) -> Result<Approval, StoreError> {
    match outcome {
        ExecutionMutationOutcome::Approval(approval) => Ok(approval),
        ExecutionMutationOutcome::Run(_) => Err(missing_idempotent_result()),
    }
}

fn record_command(state: &mut State, command: &MutationCommand, entity_id: &str) {
    state.workspace_commands.insert(
        (command.scope.clone(), command.key.clone()),
        CommandRecord {
            fingerprint: command.fingerprint,
            entity_id: entity_id.to_owned(),
        },
    );
}

fn missing_idempotent_result() -> StoreError {
    StoreError::Internal("idempotency result is missing".into())
}

fn invalid_stored_id(error: &grok_domain::IdError) -> StoreError {
    let reason = match error {
        grok_domain::IdError::Empty => "empty",
        grok_domain::IdError::TooLong => "too long",
        grok_domain::IdError::ControlCharacter => "control character",
    };
    StoreError::Internal(format!("invalid stored id: {reason}"))
}

fn revisioned_replace<K, V>(
    values: &mut HashMap<K, V>,
    id: K,
    value: V,
    expected_revision: u64,
    revision: impl Fn(&V) -> u64,
) -> Result<(), StoreError>
where
    K: Eq + std::hash::Hash,
{
    let current = values.get(&id).ok_or(StoreError::NotFound)?;
    let next_revision = expected_revision
        .checked_add(1)
        .ok_or(StoreError::Conflict)?;
    if revision(current) != expected_revision || revision(&value) != next_revision {
        return Err(StoreError::Conflict);
    }
    values.insert(id, value);
    Ok(())
}

fn recent_page<T>(
    mut values: Vec<T>,
    after: Option<&str>,
    limit: usize,
    updated_at: impl Fn(&T) -> UnixMillis,
    id: impl Fn(&T) -> &str,
) -> Result<Vec<T>, StoreError> {
    values.sort_by(|left, right| {
        updated_at(right)
            .cmp(&updated_at(left))
            .then_with(|| id(left).cmp(id(right)))
    });
    after_position(values, after, limit, id)
}

fn after_position<T>(
    values: Vec<T>,
    after: Option<&str>,
    limit: usize,
    id: impl Fn(&T) -> &str,
) -> Result<Vec<T>, StoreError> {
    let start = match after {
        Some(cursor) => values
            .iter()
            .position(|value| id(value) == cursor)
            .ok_or(StoreError::NotFound)?
            .saturating_add(1),
        None => 0,
    };
    Ok(values.into_iter().skip(start).take(limit).collect())
}

struct SearchCandidate {
    hit: WorkspaceSearchHit,
    title_tokens: Vec<String>,
    body_tokens: Vec<String>,
}

fn search_candidates(state: &State) -> Vec<SearchCandidate> {
    let mut hits = Vec::new();
    hits.extend(state.projects.values().map(|project| {
        search_candidate(
            WorkspaceSearchHit {
                id: project.id.as_str().into(),
                project_id: project.id.clone(),
                thread_id: None,
                kind: WorkspaceSearchKind::Project,
                title: project.name.clone(),
                snippet: snippet(&project.description),
                updated_at: project.updated_at,
            },
            &project.description,
        )
    }));
    hits.extend(state.threads.values().filter_map(|thread| {
        state.projects.get(&thread.project_id)?;
        Some(search_candidate(
            WorkspaceSearchHit {
                id: thread.id.as_str().into(),
                project_id: thread.project_id.clone(),
                thread_id: Some(thread.id.clone()),
                kind: WorkspaceSearchKind::Thread,
                title: thread.title.clone(),
                snippet: String::new(),
                updated_at: thread.updated_at,
            },
            "",
        ))
    }));
    hits.extend(state.messages.values().filter_map(|message| {
        if message.state == MessageState::Deleted {
            return None;
        }
        let thread = state.threads.get(&message.thread_id)?;
        state.projects.get(&thread.project_id)?;
        Some(search_candidate(
            WorkspaceSearchHit {
                id: message.id.as_str().into(),
                project_id: thread.project_id.clone(),
                thread_id: Some(message.thread_id.clone()),
                kind: WorkspaceSearchKind::Message,
                title: thread.title.clone(),
                snippet: snippet(&message.content),
                updated_at: message.updated_at,
            },
            &message.content,
        ))
    }));
    hits.extend(
        state
            .artifacts
            .values()
            .filter(|artifact| artifact.state == ArtifactState::Available)
            .filter_map(|artifact| {
                state.projects.get(&artifact.project_id)?;
                if let Some(thread_id) = &artifact.thread_id {
                    let thread = state.threads.get(thread_id)?;
                    if thread.project_id != artifact.project_id {
                        return None;
                    }
                }
                Some(search_candidate(
                    WorkspaceSearchHit {
                        id: artifact.id.as_str().into(),
                        project_id: artifact.project_id.clone(),
                        thread_id: artifact.thread_id.clone(),
                        kind: WorkspaceSearchKind::Artifact,
                        title: artifact.name.clone(),
                        snippet: String::new(),
                        updated_at: artifact.updated_at,
                    },
                    "",
                ))
            }),
    );
    hits.extend(state.automations.values().filter_map(|automation| {
        state.projects.get(&automation.project_id)?;
        Some(search_candidate(
            WorkspaceSearchHit {
                id: automation.id.as_str().into(),
                project_id: automation.project_id.clone(),
                thread_id: None,
                kind: WorkspaceSearchKind::Automation,
                title: automation.title.clone(),
                snippet: snippet(&automation.prompt),
                updated_at: automation.updated_at,
            },
            &automation.prompt,
        ))
    }));
    hits
}

fn search_candidate(hit: WorkspaceSearchHit, body: &str) -> SearchCandidate {
    SearchCandidate {
        title_tokens: search_tokens(&hit.title),
        body_tokens: search_tokens(body),
        hit,
    }
}

fn search_relevance(candidate: &SearchCandidate, phrases: &[Vec<String>]) -> Option<usize> {
    phrases.iter().try_fold(0_usize, |relevance, phrase| {
        let occurrences = phrase_occurrences(&candidate.title_tokens, phrase)
            .saturating_add(phrase_occurrences(&candidate.body_tokens, phrase));
        (occurrences > 0).then(|| relevance.saturating_add(occurrences))
    })
}

fn phrase_occurrences(tokens: &[String], phrase: &[String]) -> usize {
    if phrase.is_empty() || phrase.len() > tokens.len() {
        return 0;
    }
    tokens
        .windows(phrase.len())
        .filter(|window| *window == phrase)
        .count()
}

fn search_tokens(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    for character in value.chars() {
        if is_combining_mark(character) {
            continue;
        }
        let character = fold_latin_diacritic(character).unwrap_or(character);
        for lowercase in character.to_lowercase() {
            if lowercase.is_alphanumeric() {
                token.push(lowercase);
            } else if !token.is_empty() {
                tokens.push(std::mem::take(&mut token));
            }
        }
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    tokens
}

const fn is_combining_mark(value: char) -> bool {
    matches!(
        value,
        '\u{0300}'..='\u{036f}'
            | '\u{1ab0}'..='\u{1aff}'
            | '\u{1dc0}'..='\u{1dff}'
            | '\u{20d0}'..='\u{20ff}'
            | '\u{fe20}'..='\u{fe2f}'
    )
}

const fn fold_latin_diacritic(value: char) -> Option<char> {
    match value {
        'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' | 'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'Ā' | 'ā' | 'Ă'
        | 'ă' | 'Ą' | 'ą' | 'Ǎ' | 'ǎ' => Some('a'),
        'Ç' | 'ç' | 'Ć' | 'ć' | 'Ĉ' | 'ĉ' | 'Ċ' | 'ċ' | 'Č' | 'č' => Some('c'),
        'Ď' | 'ď' => Some('d'),
        'È' | 'É' | 'Ê' | 'Ë' | 'è' | 'é' | 'ê' | 'ë' | 'Ē' | 'ē' | 'Ĕ' | 'ĕ' | 'Ė' | 'ė' | 'Ę'
        | 'ę' | 'Ě' | 'ě' => Some('e'),
        'Ĝ' | 'ĝ' | 'Ğ' | 'ğ' | 'Ġ' | 'ġ' | 'Ģ' | 'ģ' => Some('g'),
        'Ĥ' | 'ĥ' => Some('h'),
        'Ì' | 'Í' | 'Î' | 'Ï' | 'ì' | 'í' | 'î' | 'ï' | 'Ĩ' | 'ĩ' | 'Ī' | 'ī' | 'Ĭ' | 'ĭ' | 'Į'
        | 'į' | 'İ' | 'Ǐ' | 'ǐ' => Some('i'),
        'Ĵ' | 'ĵ' => Some('j'),
        'Ķ' | 'ķ' => Some('k'),
        'Ĺ' | 'ĺ' | 'Ļ' | 'ļ' | 'Ľ' | 'ľ' => Some('l'),
        'Ñ' | 'ñ' | 'Ń' | 'ń' | 'Ņ' | 'ņ' | 'Ň' | 'ň' => Some('n'),
        'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'Ō' | 'ō' | 'Ŏ' | 'ŏ' | 'Ő'
        | 'ő' | 'Ǒ' | 'ǒ' => Some('o'),
        'Ŕ' | 'ŕ' | 'Ŗ' | 'ŗ' | 'Ř' | 'ř' => Some('r'),
        'Ś' | 'ś' | 'Ŝ' | 'ŝ' | 'Ş' | 'ş' | 'Š' | 'š' => Some('s'),
        'Ţ' | 'ţ' | 'Ť' | 'ť' => Some('t'),
        'Ù' | 'Ú' | 'Û' | 'Ü' | 'ù' | 'ú' | 'û' | 'ü' | 'Ũ' | 'ũ' | 'Ū' | 'ū' | 'Ŭ' | 'ŭ' | 'Ů'
        | 'ů' | 'Ű' | 'ű' | 'Ų' | 'ų' | 'Ǔ' | 'ǔ' => Some('u'),
        'Ŵ' | 'ŵ' => Some('w'),
        'Ý' | 'ý' | 'ÿ' | 'Ŷ' | 'ŷ' | 'Ÿ' => Some('y'),
        'Ź' | 'ź' | 'Ż' | 'ż' | 'Ž' | 'ž' => Some('z'),
        _ => None,
    }
}

fn snippet(value: &str) -> String {
    let boundary = value.floor_char_boundary(value.len().min(240));
    value[..boundary].to_owned()
}

/// Production wall clock based on `SystemTime`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> UnixMillis {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        u64::try_from(millis).unwrap_or(u64::MAX)
    }
}

/// UUID v4 identifier source for production composition roots.
#[derive(Debug, Default, Clone, Copy)]
pub struct UuidGenerator;

impl IdGenerator for UuidGenerator {
    fn generate(&self, prefix: &str) -> String {
        format!("{prefix}-{}", uuid::Uuid::new_v4())
    }
}

/// Controllable clock for use-case and protocol tests.
#[derive(Debug)]
pub struct FixedClock(AtomicU64);

impl FixedClock {
    /// Creates a clock at the supplied timestamp.
    #[must_use]
    pub const fn new(now: UnixMillis) -> Self {
        Self(AtomicU64::new(now))
    }

    /// Changes the current timestamp.
    pub fn set(&self, now: UnixMillis) {
        self.0.store(now, Ordering::SeqCst);
    }
}

impl Clock for FixedClock {
    fn now(&self) -> UnixMillis {
        self.0.load(Ordering::SeqCst)
    }
}

/// Predictable identifier source for tests.
#[derive(Debug)]
pub struct SequentialIdGenerator(AtomicU64);

impl SequentialIdGenerator {
    /// Starts a sequence at one.
    #[must_use]
    pub const fn new() -> Self {
        Self(AtomicU64::new(1))
    }
}

impl Default for SequentialIdGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl IdGenerator for SequentialIdGenerator {
    fn generate(&self, prefix: &str) -> String {
        let sequence = self.0.fetch_add(1, Ordering::SeqCst);
        format!("{prefix}-{sequence}")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Condvar,
        atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering},
    };
    use tokio::sync::Barrier;

    use grok_application::{
        AcknowledgeConversationForkDelivery, ApplicationError, ApprovalService,
        AutomationSchedulerService, AutomationSchedulerTickStatus, BeginPrivilegedDispatch,
        BranchConversationThread, ChatModelService, ContentPart, ConversationForkTurnPlan,
        ConversationModel, ConversationModelFactory, ConversationRequest, ConversationRole,
        ConversationService, ConversationStream, CreateAutomation, CreateMessage, CreateProject,
        CreateRun, CreateThread, CredentialService, DesktopPreferencesService,
        EditAndBranchConversationTurn, ExecuteConversationTurn, ModelDescriptor, ModelError,
        ModelErrorKind, ModelFailureCertainty, PrepareEffect, PreparePrivilegedOperation,
        PrivilegedOperationService, PrivilegedOperationStore, RegenerateConversationTurn,
        RequestApproval, RetryConversationTurn, RunService, SelectChatModel, SideEffectService,
        UpdateAutomation, UpdateDesktopPreferences, UpdateMessage, WorkspaceService,
        WorkspaceStore, XaiApiKeyValidation, XaiApiKeyValidationError, XaiApiKeyValidator,
    };
    use grok_domain::{
        ApprovalDecision, ApprovalRisk, ApprovalScope, AuthorityGrantId, AutomationState,
        EffectKind, EffectState, Idempotency, MissedRunPolicy, OverlapPolicy, PayloadDigest,
        PrivilegedAuthority, PrivilegedIdempotency, PrivilegedIdempotencyKey,
        PrivilegedOperationIntent, PrivilegedOperationKind, PrivilegedOperationLinks,
        PrivilegedOperationState, PrivilegedOperationTarget, PrivilegedResourceId, RequestDigest,
        RequestedAction, RunState,
    };

    use super::*;

    const TEST_CREDENTIAL_BINDING: &str =
        "xai-binding-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OTHER_TEST_CREDENTIAL_BINDING: &str =
        "xai-binding-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn original_test_lineage() -> ConversationTurnLineage {
        ConversationTurnLineage::original(TEST_CREDENTIAL_BINDING.into()).expect("test lineage")
    }

    fn seed_test_xai_credential(vault: &InMemorySecretVault) {
        seed_test_xai_credential_with_binding(vault, &format!("xai-binding-{}", "1".repeat(64)));
    }

    fn seed_test_xai_credential_with_binding(
        vault: &InMemorySecretVault,
        credential_binding_id: &str,
    ) {
        vault
            .set(
                &SecretName::new("xai.api-key.primary").expect("secret name"),
                &SecretValue::new(b"xai-user-key".to_vec()).expect("secret"),
            )
            .expect("configured key");
        vault
            .set(
                &SecretName::new("xai.api-key.local-binding").expect("binding name"),
                &SecretValue::new(credential_binding_id.as_bytes().to_vec()).expect("binding"),
            )
            .expect("configured binding");
    }

    #[derive(Debug)]
    struct AcceptXaiKey;

    #[async_trait]
    impl XaiApiKeyValidator for AcceptXaiKey {
        async fn validate(
            &self,
            _api_key: &SecretValue,
        ) -> Result<XaiApiKeyValidation, XaiApiKeyValidationError> {
            Ok(XaiApiKeyValidation::CapabilitiesResolved)
        }
    }

    #[derive(Debug)]
    struct RejectXaiKey;

    #[async_trait]
    impl XaiApiKeyValidator for RejectXaiKey {
        async fn validate(
            &self,
            _api_key: &SecretValue,
        ) -> Result<XaiApiKeyValidation, XaiApiKeyValidationError> {
            Err(XaiApiKeyValidationError::Rejected)
        }
    }

    #[derive(Debug)]
    struct CatalogModel {
        models: Vec<ModelDescriptor>,
        calls: Arc<AtomicUsize>,
        stream_calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ConversationModel for CatalogModel {
        async fn list_models(&self) -> Result<Vec<ModelDescriptor>, ModelError> {
            self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(self.models.clone())
        }

        async fn stream(
            &self,
            _request: ConversationRequest,
        ) -> Result<ConversationStream, ModelError> {
            self.stream_calls.fetch_add(1, AtomicOrdering::SeqCst);
            Err(ModelError {
                kind: ModelErrorKind::Unavailable,
                message: "test model does not execute conversations".into(),
                retryable: false,
                certainty: ModelFailureCertainty::KnownFailure,
            })
        }
    }

    #[derive(Debug)]
    struct CatalogFactory(Arc<CatalogModel>);

    impl ConversationModelFactory for CatalogFactory {
        fn create(&self, _api_key: SecretValue) -> Result<Arc<dyn ConversationModel>, ModelError> {
            Ok(self.0.clone())
        }
    }

    #[derive(Debug)]
    struct RetryableFailureModel {
        list_calls: Arc<AtomicUsize>,
        stream_calls: Arc<AtomicUsize>,
        requests: StdMutex<Vec<ConversationRequest>>,
    }

    #[async_trait]
    impl ConversationModel for RetryableFailureModel {
        async fn list_models(&self) -> Result<Vec<ModelDescriptor>, ModelError> {
            self.list_calls.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(vec![ModelDescriptor {
                id: "grok-4.3".into(),
                aliases: Vec::new(),
                input_modalities: vec!["text".into()],
                output_modalities: vec!["text".into()],
            }])
        }

        async fn stream(
            &self,
            request: ConversationRequest,
        ) -> Result<ConversationStream, ModelError> {
            self.stream_calls.fetch_add(1, AtomicOrdering::SeqCst);
            self.requests.lock().expect("request lock").push(request);
            Err(ModelError {
                kind: ModelErrorKind::Unavailable,
                message: "retryable provider failure".into(),
                retryable: true,
                certainty: ModelFailureCertainty::KnownFailure,
            })
        }
    }

    #[derive(Debug)]
    struct RetryableFailureFactory(Arc<RetryableFailureModel>);

    impl ConversationModelFactory for RetryableFailureFactory {
        fn create(&self, _api_key: SecretValue) -> Result<Arc<dyn ConversationModel>, ModelError> {
            Ok(self.0.clone())
        }
    }

    #[derive(Debug)]
    struct ForkReservationRaceModel {
        list_barrier: Arc<Barrier>,
        list_calls: Arc<AtomicUsize>,
        stream_calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ConversationModel for ForkReservationRaceModel {
        async fn list_models(&self) -> Result<Vec<ModelDescriptor>, ModelError> {
            self.list_calls.fetch_add(1, AtomicOrdering::SeqCst);
            self.list_barrier.wait().await;
            Ok(vec![ModelDescriptor {
                id: "grok-4.3".into(),
                aliases: Vec::new(),
                input_modalities: vec!["text".into()],
                output_modalities: vec!["text".into()],
            }])
        }

        async fn stream(
            &self,
            _request: ConversationRequest,
        ) -> Result<ConversationStream, ModelError> {
            self.stream_calls.fetch_add(1, AtomicOrdering::SeqCst);
            Err(ModelError {
                kind: ModelErrorKind::Unavailable,
                message: "known fork-race provider failure".into(),
                retryable: true,
                certainty: ModelFailureCertainty::KnownFailure,
            })
        }
    }

    #[derive(Debug)]
    struct ForkReservationRaceFactory(Arc<ForkReservationRaceModel>);

    impl ConversationModelFactory for ForkReservationRaceFactory {
        fn create(&self, _api_key: SecretValue) -> Result<Arc<dyn ConversationModel>, ModelError> {
            Ok(self.0.clone())
        }
    }

    #[derive(Debug)]
    struct CredentialGateProbeModel {
        entered_stream: Arc<Barrier>,
        release_stream: Arc<Barrier>,
    }

    #[async_trait]
    impl ConversationModel for CredentialGateProbeModel {
        async fn list_models(&self) -> Result<Vec<ModelDescriptor>, ModelError> {
            Ok(vec![ModelDescriptor {
                id: "grok-4.3".into(),
                aliases: Vec::new(),
                input_modalities: vec!["text".into()],
                output_modalities: vec!["text".into()],
            }])
        }

        async fn stream(
            &self,
            _request: ConversationRequest,
        ) -> Result<ConversationStream, ModelError> {
            self.entered_stream.wait().await;
            self.release_stream.wait().await;
            Err(ModelError {
                kind: ModelErrorKind::Unavailable,
                message: "credential gate probe".into(),
                retryable: true,
                certainty: ModelFailureCertainty::KnownFailure,
            })
        }
    }

    #[derive(Debug)]
    struct CredentialGateProbeFactory(Arc<CredentialGateProbeModel>);

    impl ConversationModelFactory for CredentialGateProbeFactory {
        fn create(&self, _api_key: SecretValue) -> Result<Arc<dyn ConversationModel>, ModelError> {
            Ok(self.0.clone())
        }
    }

    struct CoherentCredentialReadVault {
        inner: InMemorySecretVault,
        block_primary_read_once: AtomicBool,
        read_entered: StdMutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release_read: (StdMutex<bool>, Condvar),
    }

    impl CoherentCredentialReadVault {
        fn new() -> (Self, tokio::sync::oneshot::Receiver<()>) {
            let (read_entered, receiver) = tokio::sync::oneshot::channel();
            (
                Self {
                    inner: InMemorySecretVault::new(),
                    block_primary_read_once: AtomicBool::new(true),
                    read_entered: StdMutex::new(Some(read_entered)),
                    release_read: (StdMutex::new(false), Condvar::new()),
                },
                receiver,
            )
        }

        fn release_primary_read(&self) {
            let (released, condition) = &self.release_read;
            *released.lock().expect("release lock") = true;
            condition.notify_all();
        }
    }

    impl SecretVault for CoherentCredentialReadVault {
        fn get(&self, name: &SecretName) -> Result<SecretValue, VaultError> {
            let value = self.inner.get(name)?;
            if name.as_str() == "xai.api-key.primary"
                && self
                    .block_primary_read_once
                    .swap(false, AtomicOrdering::SeqCst)
            {
                if let Some(sender) = self
                    .read_entered
                    .lock()
                    .map_err(|_| VaultError::Internal)?
                    .take()
                {
                    let _ = sender.send(());
                }
                let (released, condition) = &self.release_read;
                let mut released = released.lock().map_err(|_| VaultError::Internal)?;
                while !*released {
                    released = condition.wait(released).map_err(|_| VaultError::Internal)?;
                }
            }
            Ok(value)
        }

        fn set(&self, name: &SecretName, value: &SecretValue) -> Result<(), VaultError> {
            self.inner.set(name, value)
        }

        fn delete(&self, name: &SecretName) -> Result<(), VaultError> {
            self.inner.delete(name)
        }
    }

    #[derive(Debug)]
    struct RecordingCredentialFactory {
        model: Arc<RetryableFailureModel>,
        keys: Arc<StdMutex<Vec<Vec<u8>>>>,
    }

    impl ConversationModelFactory for RecordingCredentialFactory {
        fn create(&self, api_key: SecretValue) -> Result<Arc<dyn ConversationModel>, ModelError> {
            self.keys
                .lock()
                .expect("recorded credential lock")
                .push(api_key.expose_secret().to_vec());
            Ok(self.model.clone())
        }
    }

    #[derive(Debug, Default)]
    struct CountingSecretVault {
        inner: InMemorySecretVault,
        key_set_calls: AtomicUsize,
    }

    impl SecretVault for CountingSecretVault {
        fn get(&self, name: &SecretName) -> Result<SecretValue, VaultError> {
            self.inner.get(name)
        }

        fn set(&self, name: &SecretName, value: &SecretValue) -> Result<(), VaultError> {
            if name.as_str() == "xai.api-key.primary" {
                self.key_set_calls.fetch_add(1, AtomicOrdering::SeqCst);
            }
            self.inner.set(name, value)
        }

        fn delete(&self, name: &SecretName) -> Result<(), VaultError> {
            self.inner.delete(name)
        }
    }

    fn privileged_intent(
        kind: PrivilegedOperationKind,
        key: &str,
        request_digest: [u8; 32],
        payload: &[u8],
    ) -> PrivilegedOperationIntent {
        let vm_id = PrivilegedResourceId::new("work-vm").expect("vm id");
        let target = match kind {
            PrivilegedOperationKind::RunnerHealth => PrivilegedOperationTarget::Runner { vm_id },
            PrivilegedOperationKind::IntegrationStart => {
                PrivilegedOperationTarget::IntegrationStart {
                    vm_id,
                    integration_id: PrivilegedResourceId::new("wisp").expect("integration id"),
                }
            }
            _ => panic!("test helper supports the two recovery classes"),
        };
        PrivilegedOperationIntent::new(
            kind,
            target,
            PayloadDigest::new(Sha256::digest(payload).into()),
            PrivilegedAuthority::new(
                AuthorityGrantId::new("authority-grant-0001").expect("grant id"),
                1_000,
            ),
            PrivilegedIdempotency::new(
                PrivilegedIdempotencyKey::new(key).expect("idempotency key"),
                RequestDigest::new(request_digest),
            ),
            PrivilegedOperationLinks::default(),
        )
    }

    #[derive(Debug)]
    struct BarrierPrivilegedOperationStore {
        inner: Arc<InMemoryExecutionStore>,
        resolution_barrier: Option<Arc<Barrier>>,
        recovery_barrier: Option<Arc<Barrier>>,
    }

    #[async_trait]
    impl PrivilegedOperationStore for BarrierPrivilegedOperationStore {
        async fn resolve_preparation(
            &self,
            intent: &PrivilegedOperationIntent,
        ) -> Result<Option<PrivilegedOperation>, StoreError> {
            let result = self.inner.resolve_preparation(intent).await;
            if let Some(barrier) = &self.resolution_barrier {
                barrier.wait().await;
            }
            result
        }

        async fn prepare_with_payload(
            &self,
            operation: PrivilegedOperation,
            payload: Vec<u8>,
        ) -> Result<PrivilegedPreparation, StoreError> {
            self.inner.prepare_with_payload(operation, payload).await
        }

        async fn get_privileged_operation(
            &self,
            id: &PrivilegedOperationId,
        ) -> Result<PrivilegedOperation, StoreError> {
            self.inner.get_privileged_operation(id).await
        }

        async fn begin_dispatch_with_attempt(
            &self,
            operation: PrivilegedOperation,
            expected_revision: u64,
            attempt: PrivilegedDispatchAttempt,
        ) -> Result<PrivilegedOperation, StoreError> {
            self.inner
                .begin_dispatch_with_attempt(operation, expected_revision, attempt)
                .await
        }

        async fn list_dispatching_for_recovery(
            &self,
            limit: usize,
        ) -> Result<Vec<PrivilegedRecoveryCandidate>, StoreError> {
            let result = self.inner.list_dispatching_for_recovery(limit).await;
            if let Some(barrier) = &self.recovery_barrier {
                barrier.wait().await;
            }
            result
        }

        async fn recover_interrupted_attempt(
            &self,
            operation: PrivilegedOperation,
            expected_revision: u64,
            attempt_sequence: u32,
            completed_at: UnixMillis,
        ) -> Result<PrivilegedOperation, StoreError> {
            self.inner
                .recover_interrupted_attempt(
                    operation,
                    expected_revision,
                    attempt_sequence,
                    completed_at,
                )
                .await
        }
    }

    async fn assert_invalid_dispatch_evidence_is_rejected(
        service: &PrivilegedOperationService,
        operation_id: &PrivilegedOperationId,
    ) {
        for (transport_operation_id, broker_boot_id, guest_boot_id) in [
            ("transport-operation-zero-broker".into(), [0; 16], [4; 16]),
            ("transport-operation-zero-guest".into(), [3; 16], [0; 16]),
            (operation_id.to_string(), [3; 16], [4; 16]),
        ] {
            assert!(matches!(
                service
                    .begin_dispatch(BeginPrivilegedDispatch {
                        operation_id: operation_id.clone(),
                        expected_revision: 0,
                        transport_operation_id,
                        wire_digest: [2; 32],
                        broker_boot_id,
                        guest_boot_id,
                        timeout_ms: 1_000,
                    })
                    .await,
                Err(ApplicationError::InvalidInput(_))
            ));
        }
    }

    async fn assert_prepare_rejected(
        store: &InMemoryExecutionStore,
        mut operation: PrivilegedOperation,
        payload: Vec<u8>,
    ) {
        operation.cancel(101).expect("cancel direct adapter input");
        assert!(
            store
                .prepare_with_payload(operation, payload)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn privileged_journal_replays_exactly_and_recovers_without_dispatch() {
        let store = Arc::new(InMemoryExecutionStore::new());
        let clock = Arc::new(FixedClock::new(100));
        let service = PrivilegedOperationService::new(
            store.clone(),
            clock.clone(),
            Arc::new(SequentialIdGenerator::new()),
        );
        let payload = b"{}".to_vec();
        let prepare = || PreparePrivilegedOperation {
            intent: privileged_intent(
                PrivilegedOperationKind::RunnerHealth,
                "runner-health-key-0001",
                [1; 32],
                &payload,
            ),
            payload: payload.clone(),
        };
        let first = service.prepare(prepare()).await.expect("prepare");
        assert!(first.created);
        assert_prepare_rejected(store.as_ref(), first.operation.clone(), payload.clone()).await;
        clock.set(2_000);
        let replay = service.prepare(prepare()).await.expect("exact replay");
        assert!(!replay.created);
        assert_eq!(replay.operation, first.operation);

        let conflicting = service
            .prepare(PreparePrivilegedOperation {
                intent: privileged_intent(
                    PrivilegedOperationKind::RunnerHealth,
                    "runner-health-key-0001",
                    [9; 32],
                    &payload,
                ),
                payload: payload.clone(),
            })
            .await;
        assert!(matches!(conflicting, Err(ApplicationError::Conflict)));

        clock.set(110);
        assert_invalid_dispatch_evidence_is_rejected(&service, &first.operation.id).await;
        let dispatching = service
            .begin_dispatch(BeginPrivilegedDispatch {
                operation_id: first.operation.id,
                expected_revision: 0,
                transport_operation_id: "transport-operation-0001".into(),
                wire_digest: [2; 32],
                broker_boot_id: [3; 16],
                guest_boot_id: [4; 16],
                timeout_ms: 1_000,
            })
            .await
            .expect("reserve dispatch");
        assert_eq!(dispatching.state, PrivilegedOperationState::Dispatching);

        clock.set(120);
        let summary = service
            .recover_interrupted(10)
            .await
            .expect("recover retry-safe operation");
        assert_eq!(summary.retry_pending, 1);
        assert_eq!(summary.interrupted_needs_review, 0);
        assert_eq!(
            service
                .recover_interrupted(10)
                .await
                .expect("recovery replay is empty")
                .recovered(),
            0
        );

        let non_idempotent = service
            .prepare(PreparePrivilegedOperation {
                intent: privileged_intent(
                    PrivilegedOperationKind::IntegrationStart,
                    "integration-start-key-0001",
                    [5; 32],
                    &payload,
                ),
                payload,
            })
            .await
            .expect("prepare non-idempotent");
        clock.set(130);
        service
            .begin_dispatch(BeginPrivilegedDispatch {
                operation_id: non_idempotent.operation.id,
                expected_revision: 0,
                transport_operation_id: "transport-operation-0002".into(),
                wire_digest: [6; 32],
                broker_boot_id: [7; 16],
                guest_boot_id: [8; 16],
                timeout_ms: 1_000,
            })
            .await
            .expect("reserve non-idempotent dispatch");
        clock.set(140);
        let summary = service
            .recover_interrupted(10)
            .await
            .expect("recover non-idempotent operation");
        assert_eq!(summary.retry_pending, 0);
        assert_eq!(summary.interrupted_needs_review, 1);
    }

    #[tokio::test]
    async fn concurrent_exact_preparations_share_one_durable_operation() {
        let inner = Arc::new(InMemoryExecutionStore::new());
        let store = Arc::new(BarrierPrivilegedOperationStore {
            inner,
            resolution_barrier: Some(Arc::new(Barrier::new(2))),
            recovery_barrier: None,
        });
        let clock = Arc::new(FixedClock::new(100));
        let service =
            PrivilegedOperationService::new(store, clock, Arc::new(SequentialIdGenerator::new()));
        let payload = b"{}".to_vec();
        let prepare = || PreparePrivilegedOperation {
            intent: privileged_intent(
                PrivilegedOperationKind::RunnerHealth,
                "concurrent-prepare-key-0001",
                [31; 32],
                &payload,
            ),
            payload: payload.clone(),
        };

        let (left, right) = tokio::join!(service.prepare(prepare()), service.prepare(prepare()));
        let left = left.expect("first concurrent preparation");
        let right = right.expect("second concurrent preparation");

        assert_ne!(left.created, right.created);
        assert_eq!(left.operation, right.operation);
    }

    #[tokio::test]
    async fn concurrent_recovery_has_one_commit_and_one_stale_conflict() {
        let inner = Arc::new(InMemoryExecutionStore::new());
        let clock = Arc::new(FixedClock::new(100));
        let setup = PrivilegedOperationService::new(
            inner.clone(),
            clock.clone(),
            Arc::new(SequentialIdGenerator::new()),
        );
        let payload = b"{}".to_vec();
        let prepared = setup
            .prepare(PreparePrivilegedOperation {
                intent: privileged_intent(
                    PrivilegedOperationKind::RunnerHealth,
                    "concurrent-recovery-key-0001",
                    [32; 32],
                    &payload,
                ),
                payload,
            })
            .await
            .expect("prepare concurrent recovery operation");
        clock.set(110);
        setup
            .begin_dispatch(BeginPrivilegedDispatch {
                operation_id: prepared.operation.id.clone(),
                expected_revision: 0,
                transport_operation_id: "concurrent-recovery-transport-0001".into(),
                wire_digest: [33; 32],
                broker_boot_id: [34; 16],
                guest_boot_id: [35; 16],
                timeout_ms: 1_000,
            })
            .await
            .expect("reserve interrupted attempt");

        let store = Arc::new(BarrierPrivilegedOperationStore {
            inner: inner.clone(),
            resolution_barrier: None,
            recovery_barrier: Some(Arc::new(Barrier::new(2))),
        });
        let first = PrivilegedOperationService::new(
            store.clone(),
            clock.clone(),
            Arc::new(SequentialIdGenerator::new()),
        );
        let second = PrivilegedOperationService::new(
            store,
            clock.clone(),
            Arc::new(SequentialIdGenerator::new()),
        );
        clock.set(120);

        let (left, right) = tokio::join!(
            first.recover_interrupted(10),
            second.recover_interrupted(10)
        );
        let successful = [&left, &right]
            .into_iter()
            .filter(|result| {
                result
                    .as_ref()
                    .is_ok_and(|summary| summary.retry_pending == 1)
            })
            .count();
        let conflicted = [&left, &right]
            .into_iter()
            .filter(|result| matches!(result, Err(ApplicationError::Conflict)))
            .count();
        assert_eq!(successful, 1);
        assert_eq!(conflicted, 1);
        assert_eq!(
            inner
                .get_privileged_operation(&prepared.operation.id)
                .await
                .expect("load recovered operation")
                .state,
            PrivilegedOperationState::RetryPending
        );
    }

    #[tokio::test]
    async fn memory_adapter_rejects_dispatch_evidence_after_authority_expiry() {
        let store = InMemoryExecutionStore::new();
        let payload = b"{}".to_vec();
        let operation = PrivilegedOperation::prepare(
            PrivilegedOperationId::new("expired-dispatch-operation-0001").expect("operation id"),
            privileged_intent(
                PrivilegedOperationKind::RunnerHealth,
                "expired-dispatch-key-0001",
                [36; 32],
                &payload,
            ),
            100,
        )
        .expect("prepared operation");
        store
            .prepare_with_payload(operation.clone(), payload)
            .await
            .expect("persist prepared operation");

        let mut corrupt_dispatch = operation.clone();
        corrupt_dispatch
            .dispatch(1_000)
            .expect("dispatch at authority boundary");
        corrupt_dispatch.updated_at = 1_001;
        let result = store
            .begin_dispatch_with_attempt(
                corrupt_dispatch,
                0,
                PrivilegedDispatchAttempt {
                    sequence: 1,
                    transport_operation_id: "expired-dispatch-transport-0001".into(),
                    wire_digest: [37; 32],
                    broker_boot_id: [38; 16],
                    guest_boot_id: [39; 16],
                    started_at: 1_001,
                    deadline_unix_ms: 2_001,
                },
            )
            .await;

        assert!(matches!(result, Err(StoreError::Internal(_))));
        assert_eq!(
            store
                .get_privileged_operation(&operation.id)
                .await
                .expect("prepared operation remains unchanged")
                .state,
            PrivilegedOperationState::Prepared
        );
    }

    async fn running_run(runs: &RunService, clock: &FixedClock) -> (Run, Run, Run) {
        let created = runs
            .create(
                CreateRun {
                    project_id: "project-1".into(),
                    thread_id: "thread-1".into(),
                },
                "create-approval-run",
            )
            .await
            .expect("create");
        clock.set(11);
        let planned = runs
            .transition(&created.id, 0, RunState::Planning, "plan-approval-run")
            .await
            .expect("plan");
        clock.set(12);
        let running = runs
            .transition(&created.id, 1, RunState::Running, "start-approval-run")
            .await
            .expect("run");
        (created, planned, running)
    }

    fn write_approval(run: &Run) -> RequestApproval {
        RequestApproval {
            run_id: run.id.clone(),
            expected_run_revision: run.revision,
            action: RequestedAction {
                action: "filesystem.write".into(),
                target: "report.md".into(),
                data_summary: "report".into(),
                risk: ApprovalRisk::Elevated,
            },
            scope: ApprovalScope::Once,
            expires_at: 100,
        }
    }

    #[tokio::test]
    async fn use_cases_keep_run_approval_and_events_consistent() {
        let store: Arc<dyn ExecutionStore> = Arc::new(InMemoryExecutionStore::new());
        let clock = Arc::new(FixedClock::new(10));
        let ids = Arc::new(SequentialIdGenerator::new());
        let runs = RunService::new(store.clone(), clock.clone(), ids.clone());
        let approvals = ApprovalService::new(store.clone(), clock.clone(), ids.clone());

        let (_, _, run) = running_run(&runs, &clock).await;
        clock.set(13);
        let approval = approvals
            .request(write_approval(&run), "request-write-approval")
            .await
            .expect("request");
        clock.set(14);
        approvals
            .decide(
                &approval.id,
                approval.revision,
                ApprovalDecision::Grant,
                "grant-write-approval",
            )
            .await
            .expect("grant");

        let stored_run = store.get_run(&run.id).await.expect("stored run");
        assert_eq!(stored_run.state, RunState::Running);
        let events = store.events_since(&run.id, 0, 100).await.expect("events");
        assert_eq!(events.len(), 6);
        assert!(
            events
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
    }

    #[tokio::test]
    async fn execution_commands_replay_canonical_outcomes() {
        let store: Arc<dyn ExecutionStore> = Arc::new(InMemoryExecutionStore::new());
        let clock = Arc::new(FixedClock::new(10));
        let ids = Arc::new(SequentialIdGenerator::new());
        let runs = RunService::new(store.clone(), clock.clone(), ids.clone());
        let approvals = ApprovalService::new(store.clone(), clock.clone(), ids);
        let (created, planned, running) = running_run(&runs, &clock).await;

        let replayed_create = runs
            .create(
                CreateRun {
                    project_id: "project-1".into(),
                    thread_id: "thread-1".into(),
                },
                "create-approval-run",
            )
            .await
            .expect("replay create");
        assert_eq!(replayed_create, created);
        let replayed_plan = runs
            .transition(&running.id, 0, RunState::Planning, "plan-approval-run")
            .await
            .expect("replay planning");
        assert_eq!(replayed_plan, planned);

        clock.set(13);
        let request = write_approval(&running);
        let pending = approvals
            .request(request.clone(), "request-write-approval")
            .await
            .expect("request");
        clock.set(14);
        let granted = approvals
            .decide(
                &pending.id,
                pending.revision,
                ApprovalDecision::Grant,
                "grant-write-approval",
            )
            .await
            .expect("grant");
        assert_eq!(
            approvals
                .request(request, "request-write-approval")
                .await
                .expect("replay request"),
            pending
        );
        assert_eq!(
            approvals
                .decide(
                    &pending.id,
                    pending.revision,
                    ApprovalDecision::Grant,
                    "grant-write-approval",
                )
                .await
                .expect("replay decision"),
            granted
        );
        assert!(matches!(
            runs.create(
                CreateRun {
                    project_id: "project-1".into(),
                    thread_id: "different-thread".into(),
                },
                "create-approval-run",
            )
            .await,
            Err(ApplicationError::Conflict)
        ));
        assert_eq!(
            store
                .events_since(&created.id, 0, 100)
                .await
                .expect("events")
                .len(),
            6
        );
    }

    #[tokio::test]
    async fn credential_commands_never_overwrite_on_conflicting_key_reuse() {
        let mutations = Arc::new(InMemoryExecutionStore::new());
        let vault = Arc::new(InMemorySecretVault::new());
        let service =
            CredentialService::new(vault.clone(), mutations.clone(), Arc::new(AcceptXaiKey));
        let first = SecretValue::new(b"xai-first-key".to_vec()).expect("first key");
        assert!(
            service
                .configure_xai_api_key(first.clone(), "configure-key-1")
                .await
                .expect("configure")
                .xai_api_key_configured
        );
        let first_binding = String::from_utf8(
            vault
                .get(&SecretName::new("xai.api-key.local-binding").expect("binding name"))
                .expect("bound credential")
                .expose_secret()
                .to_vec(),
        )
        .expect("UTF-8 binding");
        assert!(first_binding.starts_with("xai-binding-"));
        assert_eq!(first_binding.len(), "xai-binding-".len() + 64);
        let replay = CredentialService::new(vault.clone(), mutations, Arc::new(RejectXaiKey));
        assert_eq!(
            replay
                .configure_xai_api_key(first, "configure-key-1")
                .await
                .expect("completed replay does not revalidate"),
            AccountState {
                xai_api_key_configured: true,
                xai_capabilities_resolved: true,
            }
        );
        let conflicting = SecretValue::new(b"xai-different-key".to_vec()).expect("different key");
        assert!(matches!(
            service
                .configure_xai_api_key(conflicting, "configure-key-1")
                .await,
            Err(ApplicationError::Conflict)
        ));
        let stored = vault
            .get(&SecretName::new("xai.api-key.primary").expect("name"))
            .expect("stored key");
        assert_eq!(stored.expose_secret(), b"xai-first-key");
        assert_eq!(
            vault
                .get(&SecretName::new("xai.api-key.local-binding").expect("binding name"))
                .expect("replayed bound credential")
                .expose_secret(),
            first_binding.as_bytes()
        );

        assert_eq!(
            service
                .delete_xai_api_key("delete-key-1")
                .await
                .expect("delete"),
            AccountState::default()
        );
        assert_eq!(
            service
                .delete_xai_api_key("delete-key-1")
                .await
                .expect("replay delete"),
            AccountState::default()
        );
        assert_eq!(
            service.account_state().expect("state"),
            AccountState::default()
        );
        assert!(matches!(
            vault.get(&SecretName::new("xai.api-key.primary").expect("key name")),
            Err(VaultError::NotFound)
        ));
    }

    #[tokio::test]
    async fn rejected_credentials_do_not_reserve_idempotency_keys() {
        let mutations = Arc::new(InMemoryExecutionStore::new());
        let vault = Arc::new(InMemorySecretVault::new());
        let rejected =
            CredentialService::new(vault.clone(), mutations.clone(), Arc::new(RejectXaiKey));
        let rejected_key = SecretValue::new(b"xai-rejected-key".to_vec()).expect("rejected key");
        assert!(matches!(
            rejected
                .configure_xai_api_key(rejected_key, "reusable-key")
                .await,
            Err(ApplicationError::Unauthorized(_))
        ));

        let accepted = CredentialService::new(vault, mutations, Arc::new(AcceptXaiKey));
        let different_key = SecretValue::new(b"xai-accepted-key".to_vec()).expect("accepted key");
        assert!(
            accepted
                .configure_xai_api_key(different_key, "reusable-key")
                .await
                .expect("rejected validation must not reserve the key")
                .xai_api_key_configured
        );
    }

    #[tokio::test]
    async fn a_key_without_its_local_generation_binding_fails_closed() {
        let mutations = Arc::new(InMemoryExecutionStore::new());
        let vault = Arc::new(InMemorySecretVault::new());
        let service = CredentialService::new(vault.clone(), mutations, Arc::new(AcceptXaiKey));
        service
            .configure_xai_api_key(
                SecretValue::new(b"xai-bound-key".to_vec()).expect("key"),
                "binding-integrity-command",
            )
            .await
            .expect("configure");
        vault
            .delete(&SecretName::new("xai.api-key.local-binding").expect("binding name"))
            .expect("remove binding");

        assert_eq!(
            service.account_state().expect("non-secret state"),
            AccountState {
                xai_api_key_configured: true,
                xai_capabilities_resolved: false,
            }
        );
        assert!(matches!(
            service.refresh_xai_capabilities().await,
            Err(ApplicationError::Storage(message)) if message.contains("generation binding")
        ));
    }

    #[tokio::test]
    async fn concurrent_identical_credentials_execute_one_vault_write() {
        let mutations = Arc::new(InMemoryExecutionStore::new());
        let vault = Arc::new(CountingSecretVault::default());
        let service = CredentialService::new(vault.clone(), mutations, Arc::new(AcceptXaiKey));
        let first_key = SecretValue::new(b"xai-concurrent-key".to_vec()).expect("first key");
        let second_key = SecretValue::new(b"xai-concurrent-key".to_vec()).expect("second key");

        let (first, second) = tokio::join!(
            service.configure_xai_api_key(first_key, "concurrent-key"),
            service.configure_xai_api_key(second_key, "concurrent-key"),
        );
        assert!(first.expect("first configure").xai_api_key_configured);
        assert!(second.expect("second configure").xai_api_key_configured);
        assert_eq!(vault.key_set_calls.load(AtomicOrdering::SeqCst), 1);
    }

    #[tokio::test]
    async fn uncertain_effect_moves_aggregate_to_review() {
        let store: Arc<dyn ExecutionStore> = Arc::new(InMemoryExecutionStore::new());
        let clock = Arc::new(FixedClock::new(10));
        let ids = Arc::new(SequentialIdGenerator::new());
        let runs = RunService::new(store.clone(), clock.clone(), ids.clone());
        let effects = SideEffectService::new(store.clone(), clock.clone(), ids.clone());
        let mut run = runs
            .create(
                CreateRun {
                    project_id: "project-1".into(),
                    thread_id: "thread-1".into(),
                },
                "create-effect-run",
            )
            .await
            .expect("create");
        run = runs
            .transition(&run.id, run.revision, RunState::Planning, "plan-effect-run")
            .await
            .expect("plan");
        run = runs
            .transition(&run.id, run.revision, RunState::Running, "start-effect-run")
            .await
            .expect("run");
        let effect = effects
            .prepare(PrepareEffect {
                run_id: run.id.clone(),
                kind: EffectKind::ExternalMutation,
                target: "publish report".into(),
                idempotency: Idempotency::NonIdempotent,
            })
            .await
            .expect("prepare");
        let effect = effects
            .start(&effect.id, effect.revision)
            .await
            .expect("start");
        let effect = effects
            .interrupt(&effect.id, effect.revision)
            .await
            .expect("interrupt");
        assert_eq!(effect.state, EffectState::NeedsReview);
        assert_eq!(
            store.get_run(&run.id).await.expect("run").state,
            RunState::InterruptedNeedsReview
        );
    }

    #[tokio::test]
    async fn workspace_adapter_preserves_idempotency_and_message_order() {
        let store = Arc::new(InMemoryExecutionStore::new());
        let repository: Arc<dyn WorkspaceStore> = store;
        let service = WorkspaceService::new(
            repository,
            Arc::new(FixedClock::new(10)),
            Arc::new(SequentialIdGenerator::new()),
        );
        let input = CreateProject {
            name: "Research".into(),
            description: String::new(),
        };
        let project = service
            .create_project(input.clone(), "project-create")
            .await
            .expect("project");
        assert_eq!(
            service
                .create_project(input, "project-create")
                .await
                .expect("replay")
                .id,
            project.id
        );
        let thread = service
            .create_thread(
                CreateThread {
                    project_id: project.id.to_string(),
                    title: "Plan".into(),
                },
                "thread-create",
            )
            .await
            .expect("thread");
        for sequence in 1..=3 {
            service
                .create_message(
                    CreateMessage {
                        thread_id: thread.id.to_string(),
                        role: grok_domain::MessageRole::User,
                        content: format!("Message {sequence}"),
                    },
                    &format!("message-{sequence}"),
                )
                .await
                .expect("message");
        }
        let first = service
            .list_messages(&thread.id, None, 2)
            .await
            .expect("first page");
        let second = service
            .list_messages(&thread.id, first.next_cursor.as_deref(), 2)
            .await
            .expect("second page");
        assert_eq!(
            first
                .items
                .iter()
                .chain(&second.items)
                .map(|message| message.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[tokio::test]
    async fn desktop_preferences_default_and_replay_are_daemon_owned() {
        let store = Arc::new(InMemoryExecutionStore::new());
        let clock = Arc::new(FixedClock::new(10));
        let service = DesktopPreferencesService::new(store, clock.clone());
        let initial = service.get().await.expect("default preferences");
        assert!(initial.keep_running_in_notification_area);
        assert_eq!(initial.revision, 0);

        let input = UpdateDesktopPreferences {
            expected_revision: 0,
            keep_running_in_notification_area: false,
        };
        let updated = service
            .update(input, "desktop-preference-command")
            .await
            .expect("update preferences");
        assert!(!updated.keep_running_in_notification_area);
        assert_eq!(updated.revision, 1);

        clock.set(20);
        assert_eq!(
            service
                .update(input, "desktop-preference-command")
                .await
                .expect("exact replay"),
            updated
        );
        assert!(matches!(
            service
                .update(
                    UpdateDesktopPreferences {
                        keep_running_in_notification_area: true,
                        ..input
                    },
                    "desktop-preference-command",
                )
                .await,
            Err(ApplicationError::Conflict)
        ));
    }

    #[tokio::test]
    async fn chat_model_selection_canonicalizes_and_replays_before_discovery() {
        let store = Arc::new(InMemoryExecutionStore::new());
        let vault = Arc::new(InMemorySecretVault::new());
        seed_test_xai_credential(&vault);
        let credentials = Arc::new(CredentialService::new(
            vault.clone(),
            store.clone(),
            Arc::new(AcceptXaiKey),
        ));
        let calls = Arc::new(AtomicUsize::new(0));
        let model = Arc::new(CatalogModel {
            models: vec![ModelDescriptor {
                id: "grok-other".into(),
                aliases: vec!["grok-current".into()],
                input_modalities: vec!["text".into()],
                output_modalities: vec!["text".into()],
            }],
            calls: calls.clone(),
            stream_calls: Arc::new(AtomicUsize::new(0)),
        });
        let service = ChatModelService::new(
            store.clone(),
            credentials,
            Arc::new(CatalogFactory(model)),
            Arc::new(FixedClock::new(10)),
        );
        let input = SelectChatModel {
            expected_revision: 0,
            model_id: "grok-current".into(),
        };
        let selected = service
            .select(input.clone(), "select-command")
            .await
            .expect("selection");
        assert_eq!(selected.selected_model_id, "grok-other");
        assert_eq!(selected.revision, 1);
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);

        assert_eq!(
            service
                .select(input, "select-command")
                .await
                .expect("exact replay"),
            selected
        );
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);

        assert!(matches!(
            service
                .select(
                    SelectChatModel {
                        expected_revision: 0,
                        model_id: "different-input".into(),
                    },
                    "select-command",
                )
                .await,
            Err(ApplicationError::Conflict)
        ));
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);

        assert!(matches!(
            service
                .select(
                    SelectChatModel {
                        expected_revision: 1,
                        model_id: "missing-model".into(),
                    },
                    "missing-command",
                )
                .await,
            Err(ApplicationError::Unavailable(_))
        ));
        assert_eq!(service.preference().await.expect("preference"), selected);
    }

    #[tokio::test]
    #[allow(clippy::large_futures)]
    #[allow(clippy::too_many_lines)]
    async fn conversation_retry_after_model_change_conflicts_before_provider_dispatch() {
        let store = Arc::new(InMemoryExecutionStore::new());
        let vault = Arc::new(InMemorySecretVault::new());
        seed_test_xai_credential(&vault);
        let credentials = Arc::new(CredentialService::new(
            vault.clone(),
            store.clone(),
            Arc::new(AcceptXaiKey),
        ));
        let clock = Arc::new(FixedClock::new(10));
        let ids = Arc::new(SequentialIdGenerator::new());
        let workspace = Arc::new(WorkspaceService::new(
            store.clone(),
            clock.clone(),
            ids.clone(),
        ));
        let project = workspace
            .create_project(
                CreateProject {
                    name: "Chat".into(),
                    description: String::new(),
                },
                "model-change-project",
            )
            .await
            .expect("project");
        let thread = workspace
            .create_thread(
                CreateThread {
                    project_id: project.id.to_string(),
                    title: "Model change".into(),
                },
                "model-change-thread",
            )
            .await
            .expect("thread");
        let list_calls = Arc::new(AtomicUsize::new(0));
        let stream_calls = Arc::new(AtomicUsize::new(0));
        let model = Arc::new(CatalogModel {
            models: vec![
                ModelDescriptor {
                    id: "grok-4.3".into(),
                    aliases: Vec::new(),
                    input_modalities: vec!["text".into()],
                    output_modalities: vec!["text".into()],
                },
                ModelDescriptor {
                    id: "grok-other".into(),
                    aliases: Vec::new(),
                    input_modalities: vec!["text".into()],
                    output_modalities: vec!["text".into()],
                },
            ],
            calls: list_calls.clone(),
            stream_calls: stream_calls.clone(),
        });
        let conversation = ConversationService::new(
            store.clone(),
            workspace,
            credentials,
            Arc::new(CatalogFactory(model)),
            clock,
            ids,
            store.clone(),
        );
        let input = ExecuteConversationTurn {
            thread_id: thread.id.to_string(),
            content: "Hello".into(),
        };
        let first = conversation
            .execute(
                input.clone(),
                "model-change-turn",
                Box::pin(std::future::pending()),
            )
            .await
            .expect("known provider failure is durable");
        assert_eq!(first.turn.state, ConversationTurnState::Failed);
        assert_eq!(list_calls.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(stream_calls.load(AtomicOrdering::SeqCst), 1);

        let mut preference = store.get_chat_model_preference().await.expect("preference");
        preference
            .select_model("grok-other".into(), 10)
            .expect("new selection");
        store
            .save_chat_model_preference(
                preference,
                0,
                &MutationCommand {
                    scope: "select_chat_model".into(),
                    key: "model-change-preference".into(),
                    fingerprint: [9; 32],
                },
            )
            .await
            .expect("saved selection");

        assert!(matches!(
            conversation
                .execute(input, "model-change-turn", Box::pin(std::future::pending()),)
                .await,
            Err(ApplicationError::Conflict)
        ));
        assert_eq!(list_calls.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(stream_calls.load(AtomicOrdering::SeqCst), 1);
    }

    #[tokio::test]
    #[allow(clippy::large_futures)]
    #[allow(clippy::too_many_lines)]
    async fn safe_retry_reuses_frozen_context_model_and_exact_command() {
        let store = Arc::new(InMemoryExecutionStore::new());
        let vault = Arc::new(InMemorySecretVault::new());
        seed_test_xai_credential(&vault);
        let credentials = Arc::new(CredentialService::new(
            vault.clone(),
            store.clone(),
            Arc::new(AcceptXaiKey),
        ));
        let clock = Arc::new(FixedClock::new(10));
        let ids = Arc::new(SequentialIdGenerator::new());
        let workspace = Arc::new(WorkspaceService::new(
            store.clone(),
            clock.clone(),
            ids.clone(),
        ));
        let project = workspace
            .create_project(
                CreateProject {
                    name: "Retry lineage".into(),
                    description: String::new(),
                },
                "retry-project",
            )
            .await
            .expect("project");
        let thread = workspace
            .create_thread(
                CreateThread {
                    project_id: project.id.to_string(),
                    title: "Retry context".into(),
                },
                "retry-thread",
            )
            .await
            .expect("thread");
        let list_calls = Arc::new(AtomicUsize::new(0));
        let stream_calls = Arc::new(AtomicUsize::new(0));
        let model = Arc::new(RetryableFailureModel {
            list_calls: list_calls.clone(),
            stream_calls: stream_calls.clone(),
            requests: StdMutex::new(Vec::new()),
        });
        let conversation = ConversationService::new(
            store.clone(),
            workspace.clone(),
            credentials.clone(),
            Arc::new(RetryableFailureFactory(model.clone())),
            clock,
            ids,
            store.clone(),
        );

        let source = conversation
            .execute(
                ExecuteConversationTurn {
                    thread_id: thread.id.to_string(),
                    content: "Retry this exact prompt".into(),
                },
                "retry-source-command",
                Box::pin(std::future::pending()),
            )
            .await
            .expect("retryable source failure");
        assert_eq!(source.turn.state, ConversationTurnState::Failed);
        assert!(
            source
                .turn
                .failure
                .as_ref()
                .is_some_and(|value| value.retryable)
        );
        let source_context = store
            .load_turn_context(&source.turn.id)
            .await
            .expect("source context");
        let original_binding = format!("xai-binding-{}", "1".repeat(64));
        assert_eq!(
            store
                .thread_credential_binding(&thread.id)
                .await
                .expect("thread binding"),
            ConversationThreadCredentialBinding::Bound(original_binding.clone())
        );

        let input = RetryConversationTurn {
            source_turn_id: source.turn.id.to_string(),
            expected_revision: source.turn.revision,
        };
        assert!(
            conversation
                .retry_source_is_latest(&source.turn.id)
                .await
                .expect("latest source")
        );
        vault
            .set(
                &SecretName::new("xai.api-key.local-binding").expect("binding name"),
                &SecretValue::new(format!("xai-binding-{}", "2".repeat(64)).into_bytes())
                    .expect("replacement binding"),
            )
            .expect("replace credential binding");
        assert!(matches!(
            conversation
                .execute(
                    ExecuteConversationTurn {
                        thread_id: thread.id.to_string(),
                        content: "A new prompt must not switch credential generations".into(),
                    },
                    "credential-mismatch-start",
                    Box::pin(std::future::pending()),
                )
                .await,
            Err(ApplicationError::InvalidState(message))
                if message.contains("credential changed")
        ));
        assert!(matches!(
            conversation
                .retry(
                    input.clone(),
                    "credential-mismatch-retry",
                    Box::pin(std::future::pending()),
                )
                .await,
            Err(ApplicationError::InvalidState(message))
                if message.contains("credential changed")
        ));
        assert_eq!(list_calls.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(stream_calls.load(AtomicOrdering::SeqCst), 1);
        seed_test_xai_credential(&vault);

        let removed_binding = store
            .state
            .lock()
            .await
            .conversation_thread_bindings
            .remove(&thread.id)
            .expect("bound thread");
        assert_eq!(removed_binding, original_binding);
        assert_eq!(
            store
                .thread_credential_binding(&thread.id)
                .await
                .expect("legacy thread binding"),
            ConversationThreadCredentialBinding::LegacyUnbound
        );
        assert!(
            !conversation
                .retry_source_account_available(&source)
                .await
                .expect("legacy source availability")
        );
        assert!(matches!(
            conversation
                .execute(
                    ExecuteConversationTurn {
                        thread_id: thread.id.to_string(),
                        content: "Legacy history must remain read-only".into(),
                    },
                    "legacy-unbound-start",
                    Box::pin(std::future::pending()),
                )
                .await,
            Err(ApplicationError::InvalidState(message))
                if message.contains("legacy conversation thread")
        ));
        assert_eq!(list_calls.load(AtomicOrdering::SeqCst), 1);
        store
            .state
            .lock()
            .await
            .conversation_thread_bindings
            .insert(thread.id.clone(), removed_binding);

        let started = conversation
            .retry(
                input.clone(),
                "retry-command",
                Box::pin(std::future::pending()),
            )
            .await
            .expect("retry reservation");
        let reserved = started.snapshot.clone();
        assert!(
            !conversation
                .retry_source_is_latest(&source.turn.id)
                .await
                .expect("source now has a child")
        );
        assert_eq!(reserved.turn.state, ConversationTurnState::Reserved);
        assert_eq!(reserved.turn.model_id, source.turn.model_id);
        assert_eq!(reserved.user_message.content, source.user_message.content);
        assert_ne!(reserved.user_message.id, source.user_message.id);
        assert_eq!(reserved.lineage.retry_depth, 1);
        assert!(matches!(
            &reserved.lineage.origin,
            ConversationTurnOrigin::Retry { source_turn_id }
                if source_turn_id == &source.turn.id
        ));
        let retry_context = store
            .load_turn_context(&reserved.turn.id)
            .await
            .expect("retry context");
        assert_eq!(
            source_context
                .iter()
                .map(|message| (message.role, message.content.as_str()))
                .collect::<Vec<_>>(),
            retry_context
                .iter()
                .map(|message| (message.role, message.content.as_str()))
                .collect::<Vec<_>>(),
        );
        assert_ne!(
            source_context.last().map(|value| &value.id),
            retry_context.last().map(|value| &value.id)
        );

        assert_eq!(
            conversation
                .replay_retry(&input, "retry-command")
                .await
                .expect("retry replay")
                .expect("reserved retry")
                .turn
                .id,
            reserved.turn.id
        );
        assert_eq!(list_calls.load(AtomicOrdering::SeqCst), 2);
        let retry = conversation
            .dispatch(
                started.dispatch.expect("retry dispatch"),
                Box::pin(std::future::pending()),
            )
            .await
            .expect("retryable retry failure");
        assert_eq!(retry.turn.state, ConversationTurnState::Failed);
        assert!(
            conversation
                .retry_source_is_latest(&retry.turn.id)
                .await
                .expect("retry is the latest source")
        );
        assert_eq!(stream_calls.load(AtomicOrdering::SeqCst), 2);
        {
            let requests = model.requests.lock().expect("requests");
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[0], requests[1]);
        }
        store
            .state
            .lock()
            .await
            .conversation_lineages
            .get_mut(&retry.turn.id)
            .expect("retry lineage")
            .retry_depth = 64;
        assert!(matches!(
            conversation
                .retry(
                    RetryConversationTurn {
                        source_turn_id: retry.turn.id.to_string(),
                        expected_revision: retry.turn.revision,
                    },
                    "depth-exhausted-retry",
                    Box::pin(std::future::pending()),
                )
                .await,
            Err(ApplicationError::InvalidState(message))
                if message.contains("retry depth is exhausted")
        ));
        assert_eq!(list_calls.load(AtomicOrdering::SeqCst), 2);
        store
            .state
            .lock()
            .await
            .conversation_lineages
            .get_mut(&retry.turn.id)
            .expect("retry lineage")
            .retry_depth = 1;
        assert!(matches!(
            conversation
                .retry(
                    input.clone(),
                    "superseded-source-retry",
                    Box::pin(std::future::pending()),
                )
                .await,
            Err(ApplicationError::Conflict)
        ));
        assert_eq!(list_calls.load(AtomicOrdering::SeqCst), 2);
        assert_eq!(stream_calls.load(AtomicOrdering::SeqCst), 2);
        workspace
            .archive_thread(&thread.id, 0, "archive-retry-thread")
            .await
            .expect("archive retry thread");
        assert!(matches!(
            conversation
                .retry(
                    RetryConversationTurn {
                        source_turn_id: retry.turn.id.to_string(),
                        expected_revision: retry.turn.revision,
                    },
                    "archived-thread-retry",
                    Box::pin(std::future::pending()),
                )
                .await,
            Err(ApplicationError::InvalidState(message))
                if message.contains("thread is archived")
        ));
        assert_eq!(list_calls.load(AtomicOrdering::SeqCst), 2);
        assert_eq!(stream_calls.load(AtomicOrdering::SeqCst), 2);

        let revoked_thread = workspace
            .create_thread(
                CreateThread {
                    project_id: project.id.to_string(),
                    title: "Credential revocation".into(),
                },
                "revoked-dispatch-thread",
            )
            .await
            .expect("revocation thread");
        let revoked = conversation
            .start(
                ExecuteConversationTurn {
                    thread_id: revoked_thread.id.to_string(),
                    content: "Do not dispatch after credential deletion".into(),
                },
                "revoked-dispatch-turn",
                Box::pin(std::future::pending()),
            )
            .await
            .expect("reserved before revocation");
        assert_eq!(revoked.snapshot.turn.state, ConversationTurnState::Reserved);
        credentials
            .delete_xai_api_key("delete-before-provider-dispatch")
            .await
            .expect("delete credential before dispatch");
        assert!(matches!(
            conversation
                .dispatch(
                    revoked.dispatch.expect("revoked dispatch plan"),
                    Box::pin(std::future::pending()),
                )
                .await,
            Err(ApplicationError::Unauthorized(_))
        ));
        assert_eq!(list_calls.load(AtomicOrdering::SeqCst), 3);
        assert_eq!(stream_calls.load(AtomicOrdering::SeqCst), 2);
        let reconciled = conversation
            .reconcile_dispatch_exit(&revoked.snapshot.turn.id)
            .await
            .expect("reconcile revoked reservation");
        assert_eq!(reconciled.turn.state, ConversationTurnState::Cancelled);

        let replay = conversation
            .retry(input, "retry-command", Box::pin(std::future::pending()))
            .await
            .expect("terminal retry replay");
        assert_eq!(replay.snapshot.turn.id, retry.turn.id);
        assert!(replay.dispatch.is_none());
        assert_eq!(list_calls.load(AtomicOrdering::SeqCst), 3);
        assert_eq!(stream_calls.load(AtomicOrdering::SeqCst), 2);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn credential_deletion_linearizes_after_a_started_provider_request() {
        let store = Arc::new(InMemoryExecutionStore::new());
        let vault = Arc::new(InMemorySecretVault::new());
        seed_test_xai_credential(&vault);
        let credentials = Arc::new(CredentialService::new(
            vault,
            store.clone(),
            Arc::new(AcceptXaiKey),
        ));
        let clock = Arc::new(FixedClock::new(10));
        let ids = Arc::new(SequentialIdGenerator::new());
        let workspace = Arc::new(WorkspaceService::new(
            store.clone(),
            clock.clone(),
            ids.clone(),
        ));
        let project = workspace
            .create_project(
                CreateProject {
                    name: "Credential gate".into(),
                    description: String::new(),
                },
                "credential-gate-project",
            )
            .await
            .expect("project");
        let thread = workspace
            .create_thread(
                CreateThread {
                    project_id: project.id.to_string(),
                    title: "Credential gate".into(),
                },
                "credential-gate-thread",
            )
            .await
            .expect("thread");
        let entered_stream = Arc::new(Barrier::new(2));
        let release_stream = Arc::new(Barrier::new(2));
        let conversation = Arc::new(ConversationService::new(
            store.clone(),
            workspace,
            credentials.clone(),
            Arc::new(CredentialGateProbeFactory(Arc::new(
                CredentialGateProbeModel {
                    entered_stream: entered_stream.clone(),
                    release_stream: release_stream.clone(),
                },
            ))),
            clock,
            ids,
            store.clone(),
        ));
        let started = conversation
            .start(
                ExecuteConversationTurn {
                    thread_id: thread.id.to_string(),
                    content: "Linearize this provider boundary".into(),
                },
                "credential-gate-turn",
                Box::pin(std::future::pending()),
            )
            .await
            .expect("reserved turn");
        let turn_id = started.snapshot.turn.id.clone();
        let dispatch = started.dispatch.expect("dispatch plan");
        let dispatch_service = conversation.clone();
        let dispatch_task = tokio::spawn(async move {
            dispatch_service
                .dispatch(dispatch, Box::pin(std::future::pending()))
                .await
        });
        entered_stream.wait().await;
        assert_eq!(
            store
                .load_turn(&turn_id)
                .await
                .expect("load provider-started turn")
                .expect("turn")
                .turn
                .state,
            ConversationTurnState::ProviderStarted
        );

        let delete_started = Arc::new(Barrier::new(2));
        let delete_credentials = credentials.clone();
        let delete_signal = delete_started.clone();
        let mut delete_task = tokio::spawn(async move {
            delete_signal.wait().await;
            delete_credentials
                .delete_xai_api_key("credential-gate-delete")
                .await
        });
        delete_started.wait().await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut delete_task)
                .await
                .is_err(),
            "credential deletion must wait until the provider request boundary resolves"
        );

        release_stream.wait().await;
        let failed = dispatch_task
            .await
            .expect("dispatch task")
            .expect("known provider failure is durable");
        assert_eq!(failed.turn.state, ConversationTurnState::Failed);
        assert_eq!(
            delete_task
                .await
                .expect("delete task")
                .expect("delete credential"),
            AccountState::default()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::too_many_lines)]
    async fn credential_preflight_reads_key_and_binding_under_one_generation_lease() {
        let store = Arc::new(InMemoryExecutionStore::new());
        let (vault, read_entered) = CoherentCredentialReadVault::new();
        seed_test_xai_credential(&vault.inner);
        let vault = Arc::new(vault);
        let credentials = Arc::new(CredentialService::new(
            vault.clone(),
            store.clone(),
            Arc::new(AcceptXaiKey),
        ));
        let clock = Arc::new(FixedClock::new(10));
        let ids = Arc::new(SequentialIdGenerator::new());
        let workspace = Arc::new(WorkspaceService::new(
            store.clone(),
            clock.clone(),
            ids.clone(),
        ));
        let project = workspace
            .create_project(
                CreateProject {
                    name: "Coherent credential read".into(),
                    description: String::new(),
                },
                "coherent-read-project",
            )
            .await
            .expect("project");
        let thread = workspace
            .create_thread(
                CreateThread {
                    project_id: project.id.to_string(),
                    title: "Coherent credential read".into(),
                },
                "coherent-read-thread",
            )
            .await
            .expect("thread");
        let list_calls = Arc::new(AtomicUsize::new(0));
        let stream_calls = Arc::new(AtomicUsize::new(0));
        let model = Arc::new(RetryableFailureModel {
            list_calls: list_calls.clone(),
            stream_calls: stream_calls.clone(),
            requests: StdMutex::new(Vec::new()),
        });
        let recorded_keys = Arc::new(StdMutex::new(Vec::new()));
        let conversation = Arc::new(ConversationService::new(
            store.clone(),
            workspace,
            credentials.clone(),
            Arc::new(RecordingCredentialFactory {
                model,
                keys: recorded_keys.clone(),
            }),
            clock,
            ids,
            store.clone(),
        ));

        let start_service = conversation.clone();
        let start_task = tokio::spawn(async move {
            start_service
                .start(
                    ExecuteConversationTurn {
                        thread_id: thread.id.to_string(),
                        content: "Keep this generation coherent".into(),
                    },
                    "coherent-read-turn",
                    Box::pin(std::future::pending()),
                )
                .await
        });
        read_entered.await.expect("primary-key read entered");

        let replacement_credentials = credentials.clone();
        let mut replacement = tokio::spawn(async move {
            replacement_credentials
                .configure_xai_api_key(
                    SecretValue::new(b"xai-replacement-key".to_vec()).expect("replacement key"),
                    "coherent-read-replacement",
                )
                .await
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut replacement)
                .await
                .is_err(),
            "credential replacement must wait while the old key read is leased"
        );
        vault.release_primary_read();

        let started = start_task
            .await
            .expect("start task")
            .expect("coherent reservation");
        replacement
            .await
            .expect("replacement task")
            .expect("replace credential");
        assert_eq!(list_calls.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(
            recorded_keys.lock().expect("recorded keys").as_slice(),
            &[b"xai-user-key".to_vec()]
        );
        let original_binding = format!("xai-binding-{}", "1".repeat(64));
        assert_eq!(
            started.snapshot.lineage.credential_binding_id.as_deref(),
            Some(original_binding.as_str())
        );
        assert!(matches!(
            conversation
                .dispatch(
                    started.dispatch.expect("dispatch plan"),
                    Box::pin(std::future::pending()),
                )
                .await,
            Err(ApplicationError::InvalidState(message))
                if message.contains("credential changed")
        ));
        assert_eq!(stream_calls.load(AtomicOrdering::SeqCst), 0);
    }

    #[tokio::test]
    #[allow(clippy::large_futures)]
    #[allow(clippy::too_many_lines)]
    async fn conversation_rejects_ambiguous_catalog_before_stream_dispatch() {
        let store = Arc::new(InMemoryExecutionStore::new());
        let vault = Arc::new(InMemorySecretVault::new());
        seed_test_xai_credential(&vault);
        let credentials = Arc::new(CredentialService::new(
            vault,
            store.clone(),
            Arc::new(AcceptXaiKey),
        ));
        let clock = Arc::new(FixedClock::new(10));
        let ids = Arc::new(SequentialIdGenerator::new());
        let workspace = Arc::new(WorkspaceService::new(
            store.clone(),
            clock.clone(),
            ids.clone(),
        ));
        let project = workspace
            .create_project(
                CreateProject {
                    name: "Chat".into(),
                    description: String::new(),
                },
                "ambiguous-project",
            )
            .await
            .expect("project");
        let thread = workspace
            .create_thread(
                CreateThread {
                    project_id: project.id.to_string(),
                    title: "Ambiguous catalog".into(),
                },
                "ambiguous-thread",
            )
            .await
            .expect("thread");
        let list_calls = Arc::new(AtomicUsize::new(0));
        let stream_calls = Arc::new(AtomicUsize::new(0));
        let model = Arc::new(CatalogModel {
            models: vec![
                ModelDescriptor {
                    id: "grok-4.3".into(),
                    aliases: vec!["shared-alias".into()],
                    input_modalities: vec!["text".into()],
                    output_modalities: vec!["text".into()],
                },
                ModelDescriptor {
                    id: "grok-other".into(),
                    aliases: vec!["shared-alias".into()],
                    input_modalities: vec!["text".into()],
                    output_modalities: vec!["text".into()],
                },
            ],
            calls: list_calls.clone(),
            stream_calls: stream_calls.clone(),
        });
        let conversation = ConversationService::new(
            store.clone(),
            workspace,
            credentials,
            Arc::new(CatalogFactory(model)),
            clock,
            ids,
            store,
        );

        assert!(matches!(
            conversation
                .execute(
                    ExecuteConversationTurn {
                        thread_id: thread.id.to_string(),
                        content: "Hello".into(),
                    },
                    "ambiguous-turn",
                    Box::pin(std::future::pending()),
                )
                .await,
            Err(ApplicationError::Unavailable(message)) if message.contains("ambiguous")
        ));
        assert_eq!(list_calls.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(stream_calls.load(AtomicOrdering::SeqCst), 0);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn scheduler_recovery_releases_unlinked_and_reviews_linked_without_run_replay() {
        fn claimed_occurrence(
            occurrence_id: &str,
            automation_id: &str,
            snapshot: AutomationExecutionSnapshot,
            decision: grok_domain::AutomationScheduleDecision,
            token: &AutomationSchedulerLeaseToken,
            run_id: Option<RunId>,
            scheduled_for: UnixMillis,
        ) -> (AutomationOccurrence, AutomationOccurrenceClaimAttempt) {
            let mut occurrence = AutomationOccurrence::pending(
                AutomationOccurrenceId::new(occurrence_id).expect("occurrence ID"),
                AutomationId::new(automation_id).expect("automation ID"),
                snapshot,
                decision,
                1,
                scheduled_for,
            )
            .expect("pending occurrence");
            occurrence
                .claim(token, scheduled_for + 1, scheduled_for + 10)
                .expect("claim occurrence");
            if let Some(run_id) = run_id {
                occurrence
                    .link_run(token, run_id, scheduled_for + 2)
                    .expect("link run");
            }
            let claim = occurrence.claim.as_ref().expect("open claim");
            let attempt = AutomationOccurrenceClaimAttempt {
                occurrence_id: occurrence.id.clone(),
                sequence: 1,
                owner_id: claim.owner_id.clone(),
                fence: claim.fence,
                claimed_at: claim.claimed_at,
                expires_at: claim.expires_at,
                completed_at: None,
                completion: None,
                request_fingerprint: [17; 32],
            };
            (occurrence, attempt)
        }

        let store = Arc::new(InMemoryExecutionStore::new());
        let snapshot = AutomationExecutionSnapshot::new(
            0,
            ProjectId::new("scheduler-project").expect("project ID"),
            "Daily brief".into(),
            "Summarize without dispatching.".into(),
            "v1;daily;09:00".into(),
            "UTC".into(),
            MissedRunPolicy::Skip,
            OverlapPolicy::Skip,
        )
        .expect("snapshot");
        let nominal = grok_domain::AutomationLocalDateTime::new(2026, 7, 13, 9, 0)
            .expect("nominal local time");
        let decision = snapshot
            .schedule
            .decision_for_nominal(&snapshot.timezone, nominal)
            .expect("schedule decision");
        let scheduled_for = decision.scheduled_for().expect("scheduled UTC instant");
        let old_owner = AutomationSchedulerOwnerId::new("daemon-old").expect("old owner");
        let old_lease =
            AutomationSchedulerLease::acquire(old_owner, 1, scheduled_for, 100).expect("old lease");
        let old_token = old_lease.token();
        let linked_run_id = RunId::new("automation-run-linked").expect("run ID");
        let (unlinked, unlinked_attempt) = claimed_occurrence(
            "occurrence-unlinked",
            "automation-unlinked",
            snapshot.clone(),
            decision,
            &old_token,
            None,
            scheduled_for,
        );
        let (linked, linked_attempt) = claimed_occurrence(
            "occurrence-linked",
            "automation-linked",
            snapshot,
            decision,
            &old_token,
            Some(linked_run_id.clone()),
            scheduled_for,
        );
        let linked_run = Run::queued(
            linked_run_id.clone(),
            ProjectId::new("scheduler-project").expect("project ID"),
            ThreadId::new("scheduler-thread").expect("thread ID"),
            scheduled_for + 2,
        );
        {
            let mut state = store.state.lock().await;
            state.automation_scheduler_lease = Some(old_lease);
            state
                .automation_occurrence_claim_attempts
                .insert(unlinked.id.clone(), vec![unlinked_attempt]);
            state
                .automation_occurrence_claim_attempts
                .insert(linked.id.clone(), vec![linked_attempt]);
            state
                .automation_occurrences
                .insert(unlinked.id.clone(), unlinked.clone());
            state
                .automation_occurrences
                .insert(linked.id.clone(), linked.clone());
            state.runs.insert(linked_run_id.clone(), linked_run.clone());
        }

        let recovery_at = scheduled_for + 101;
        let scheduler = AutomationSchedulerService::new(
            store.clone(),
            Arc::new(FixedClock::new(recovery_at)),
            Arc::new(SequentialIdGenerator::new()),
        );
        let summary = scheduler
            .recover_expired_claims(
                &AutomationSchedulerOwnerId::new("daemon-new").expect("new owner"),
                MAX_AUTOMATION_SCHEDULER_RECOVERY_BATCH,
            )
            .await
            .expect("bounded recovery");
        assert_eq!(summary.released_unlinked, 1);
        assert_eq!(summary.interrupted_linked, 1);
        assert_eq!(summary.attempts_exhausted, 0);
        assert!(!summary.truncated);

        let released = store
            .get_automation_occurrence(&unlinked.id)
            .await
            .expect("released occurrence");
        assert_eq!(released.state, AutomationOccurrenceState::Pending);
        assert!(released.claim.is_none());
        let reviewed = store
            .get_automation_occurrence(&linked.id)
            .await
            .expect("reviewed occurrence");
        assert_eq!(
            reviewed.state,
            AutomationOccurrenceState::InterruptedNeedsReview
        );
        assert_eq!(reviewed.run_id.as_ref(), Some(&linked_run_id));
        assert!(reviewed.claim.is_none());

        let state = store.state.lock().await;
        assert_eq!(state.runs.len(), 1);
        assert_eq!(state.runs.get(&linked_run_id), Some(&linked_run));
        assert_eq!(
            state.automation_occurrence_claim_attempts[&unlinked.id][0].completion,
            Some(AutomationOccurrenceClaimCompletion::ExpiredUnlinked)
        );
        assert_eq!(
            state.automation_occurrence_claim_attempts[&linked.id][0].completion,
            Some(AutomationOccurrenceClaimCompletion::RunLinked)
        );
    }

    #[tokio::test]
    async fn automation_definitions_cannot_be_enabled_without_a_scheduler() {
        let store = Arc::new(InMemoryExecutionStore::new());
        let repository: Arc<dyn WorkspaceStore> = store;
        let service = WorkspaceService::new(
            repository,
            Arc::new(FixedClock::new(10)),
            Arc::new(SequentialIdGenerator::new()),
        );
        let project = service
            .create_project(
                CreateProject {
                    name: "Automation policy".into(),
                    description: String::new(),
                },
                "automation-policy-project",
            )
            .await
            .expect("project");

        let result = service
            .create_automation(
                CreateAutomation {
                    project_id: project.id.to_string(),
                    title: "Daily brief".into(),
                    prompt: "Summarize readiness".into(),
                    schedule: "0 9 * * *".into(),
                    timezone: "UTC".into(),
                    missed_run_policy: MissedRunPolicy::Skip,
                    overlap_policy: OverlapPolicy::Skip,
                    enabled: true,
                },
                "enabled-automation",
            )
            .await;

        assert!(matches!(result, Err(ApplicationError::Unavailable(_))));
        assert!(
            service
                .list_automations(&project.id, None, 10)
                .await
                .expect("automations")
                .items
                .is_empty()
        );

        let disabled = service
            .create_automation(
                CreateAutomation {
                    project_id: project.id.to_string(),
                    title: "Daily brief".into(),
                    prompt: "Summarize readiness".into(),
                    schedule: "0 9 * * *".into(),
                    timezone: "UTC".into(),
                    missed_run_policy: MissedRunPolicy::Skip,
                    overlap_policy: OverlapPolicy::Skip,
                    enabled: false,
                },
                "disabled-automation",
            )
            .await
            .expect("disabled definition");
        let result = service
            .update_automation(
                UpdateAutomation {
                    id: disabled.id.to_string(),
                    expected_revision: 0,
                    title: disabled.title.clone(),
                    prompt: disabled.prompt.clone(),
                    schedule: disabled.schedule.clone(),
                    timezone: disabled.timezone.clone(),
                    missed_run_policy: disabled.missed_run_policy,
                    overlap_policy: disabled.overlap_policy,
                    enabled: true,
                },
                "enable-automation",
            )
            .await;
        assert!(matches!(result, Err(ApplicationError::Unavailable(_))));
        assert_eq!(
            service
                .get_automation(&disabled.id)
                .await
                .expect("unchanged definition")
                .state,
            AutomationState::Disabled
        );
    }

    const SCHEDULER_TEST_DAY_MS: UnixMillis = 86_400_000;

    fn scheduler_test_command(scope: &str, key: impl Into<String>, byte: u8) -> MutationCommand {
        MutationCommand {
            scope: scope.into(),
            key: key.into(),
            fingerprint: [byte; 32],
        }
    }

    async fn seed_scheduler_automation(
        store: &InMemoryExecutionStore,
        id: &str,
        missed_run_policy: MissedRunPolicy,
        overlap_policy: OverlapPolicy,
        now: UnixMillis,
    ) -> Automation {
        seed_scheduler_automation_with_schedule(
            store,
            id,
            missed_run_policy,
            overlap_policy,
            "v1;daily;00:00",
            "UTC",
            now,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn seed_scheduler_automation_with_schedule(
        store: &InMemoryExecutionStore,
        id: &str,
        missed_run_policy: MissedRunPolicy,
        overlap_policy: OverlapPolicy,
        schedule: &str,
        timezone: &str,
        now: UnixMillis,
    ) -> Automation {
        let project_id = ProjectId::new("scheduler-project").expect("project id");
        let automation = Automation::new(
            AutomationId::new(id).expect("automation id"),
            project_id.clone(),
            format!("Schedule {id}"),
            "Do the bounded test work".into(),
            schedule.into(),
            timezone.into(),
            missed_run_policy,
            overlap_policy,
            true,
            now,
        )
        .expect("automation");
        let mut state = store.state.lock().await;
        state.projects.entry(project_id.clone()).or_insert_with(|| {
            Project::new(project_id, "Scheduler project".into(), String::new(), now)
                .expect("project")
        });
        state
            .automations
            .insert(automation.id.clone(), automation.clone());
        automation
    }

    fn scheduler_test_snapshot(automation: &Automation) -> AutomationExecutionSnapshot {
        automation_scheduler_snapshot(automation).expect("snapshot")
    }

    async fn acquire_scheduler_test_lease(
        store: &InMemoryExecutionStore,
        owner: &str,
        now: UnixMillis,
    ) -> AutomationSchedulerLeaseToken {
        let owner = AutomationSchedulerOwnerId::new(owner).expect("owner");
        match store
            .acquire_automation_scheduler_lease(&owner, now, MAX_AUTOMATION_SCHEDULER_LEASE_MS)
            .await
            .expect("lease")
        {
            AutomationSchedulerLeaseAcquisition::Acquired { lease, .. } => lease.token(),
            acquisition => panic!("unexpected acquisition: {acquisition:?}"),
        }
    }

    async fn initialize_scheduler_test_cursor(
        store: &InMemoryExecutionStore,
        automation: &Automation,
        lease: &AutomationSchedulerLeaseToken,
        observed_at: UnixMillis,
        key: &str,
        byte: u8,
    ) -> AutomationScheduleCursor {
        let snapshot = scheduler_test_snapshot(automation);
        let next = snapshot
            .schedule
            .next_decision_after(&snapshot.timezone, automation.updated_at)
            .expect("next decision");
        let cursor = AutomationScheduleCursor::new(
            automation.id.clone(),
            &snapshot,
            automation.updated_at,
            Some(next),
            observed_at,
        )
        .expect("initial cursor");
        let result = store
            .commit_automation_schedule_evaluation(AutomationScheduleEvaluationCommit {
                lease: lease.clone(),
                expected_automation_revision: automation.revision,
                expected_cursor_revision: None,
                cursor: cursor.clone(),
                occurrences: Vec::new(),
                observed_at,
                command: scheduler_test_command(AUTOMATION_EVALUATION_SCOPE, key, byte),
            })
            .await
            .expect("initialize cursor");
        assert!(result.occurrences.is_empty());
        cursor
    }

    fn scheduler_test_advancement(
        cursor: &AutomationScheduleCursor,
        snapshot: &AutomationExecutionSnapshot,
        through: UnixMillis,
        observed_at: UnixMillis,
    ) -> AutomationScheduleCursor {
        let next = snapshot
            .schedule
            .next_decision_after(&snapshot.timezone, through)
            .expect("next decision");
        let mut advanced = cursor.clone();
        advanced
            .advance(through, Some(next), observed_at)
            .expect("advance cursor");
        advanced
    }

    fn scheduler_test_pending_occurrences(
        automation: &Automation,
        snapshot: &AutomationExecutionSnapshot,
        after: UnixMillis,
        through: UnixMillis,
        observed_at: UnixMillis,
        id_prefix: &str,
    ) -> Vec<AutomationOccurrence> {
        snapshot
            .schedule
            .decisions_between(
                &snapshot.timezone,
                after,
                through,
                MAX_AUTOMATION_SCHEDULE_DECISIONS,
            )
            .expect("schedule decisions")
            .decisions
            .into_iter()
            .enumerate()
            .map(|(index, decision)| {
                AutomationOccurrence::pending(
                    AutomationOccurrenceId::new(format!("{id_prefix}-{index}"))
                        .expect("occurrence id"),
                    automation.id.clone(),
                    snapshot.clone(),
                    decision,
                    1,
                    observed_at,
                )
                .expect("pending occurrence")
            })
            .collect()
    }

    #[tokio::test]
    async fn automation_scheduler_lease_is_fenced_and_uses_the_durable_clock_floor() {
        let store = InMemoryExecutionStore::new();
        let first_owner = AutomationSchedulerOwnerId::new("scheduler-a").expect("owner");
        let other_owner = AutomationSchedulerOwnerId::new("scheduler-b").expect("owner");
        let first = store
            .acquire_automation_scheduler_lease(&first_owner, 100, 100)
            .await
            .expect("initial lease");
        assert!(matches!(
            first,
            AutomationSchedulerLeaseAcquisition::Acquired {
                ref lease,
                continuous: false,
                continuity_started_at: 100,
            } if lease.fence == 1 && lease.owner_id == first_owner
        ));
        let renewed = store
            .acquire_automation_scheduler_lease(&first_owner, 110, 100)
            .await
            .expect("renew lease");
        assert!(matches!(
            renewed,
            AutomationSchedulerLeaseAcquisition::Acquired {
                ref lease,
                continuous: true,
                continuity_started_at: 100,
            } if lease.fence == 1 && lease.renewed_at == 110 && lease.expires_at == 210
        ));
        assert!(matches!(
            store
                .acquire_automation_scheduler_lease(&other_owner, 120, 100)
                .await
                .expect("busy lease"),
            AutomationSchedulerLeaseAcquisition::Busy { lease }
                if lease.owner_id == first_owner && lease.fence == 1
        ));
        let durable_before = store.state.lock().await.automation_scheduler_lease.clone();
        assert_eq!(
            store
                .acquire_automation_scheduler_lease(&other_owner, 109, 100)
                .await
                .expect("clock regression"),
            AutomationSchedulerLeaseAcquisition::ClockRegressed { durable_floor: 110 }
        );
        assert_eq!(
            store.state.lock().await.automation_scheduler_lease,
            durable_before,
            "a regressed clock must not mutate the lease"
        );
        assert!(matches!(
            store
                .acquire_automation_scheduler_lease(&other_owner, 210, 100)
                .await
                .expect("takeover"),
            AutomationSchedulerLeaseAcquisition::Acquired {
                lease,
                continuous: false,
                continuity_started_at: 210,
            } if lease.owner_id == other_owner && lease.fence == 2
        ));

        let automation = seed_scheduler_automation(
            &store,
            "clock-floor",
            MissedRunPolicy::RunOnce,
            OverlapPolicy::Skip,
            0,
        )
        .await;
        let snapshot = scheduler_test_snapshot(&automation);
        let next = snapshot
            .schedule
            .next_decision_after(&snapshot.timezone, 0)
            .expect("next");
        let cursor =
            AutomationScheduleCursor::new(automation.id.clone(), &snapshot, 0, Some(next), 500)
                .expect("cursor");
        {
            let mut state = store.state.lock().await;
            state.automation_scheduler_lease = None;
            state
                .automation_schedule_cursors
                .insert(automation.id.clone(), cursor);
        }
        assert_eq!(
            store
                .acquire_automation_scheduler_lease(&first_owner, 499, 100)
                .await
                .expect("durable cursor floor"),
            AutomationSchedulerLeaseAcquisition::ClockRegressed { durable_floor: 500 }
        );
        assert!(
            store
                .state
                .lock()
                .await
                .automation_scheduler_lease
                .is_none()
        );
    }

    #[tokio::test]
    async fn automation_scheduler_pages_all_enabled_definitions_without_starvation() {
        let store = Arc::new(InMemoryExecutionStore::new());
        let project_id = ProjectId::new("scheduler-page-project").expect("project id");
        {
            let mut state = store.state.lock().await;
            state.projects.insert(
                project_id.clone(),
                Project::new(
                    project_id.clone(),
                    "Scheduler paging".into(),
                    String::new(),
                    0,
                )
                .expect("project"),
            );
            for index in 0..205 {
                let automation = Automation::new(
                    AutomationId::new(format!("paged-{index:03}")).expect("id"),
                    project_id.clone(),
                    format!("Paged {index:03}"),
                    "Test stable scheduler paging".into(),
                    "v1;daily;00:00".into(),
                    "UTC".into(),
                    MissedRunPolicy::RunOnce,
                    OverlapPolicy::Skip,
                    true,
                    0,
                )
                .expect("automation");
                state.automations.insert(automation.id.clone(), automation);
            }
        }
        let service = AutomationSchedulerService::new(
            store.clone(),
            Arc::new(FixedClock::new(10)),
            Arc::new(SequentialIdGenerator::new()),
        );
        let owner = AutomationSchedulerOwnerId::new("paging-owner").expect("owner");
        let first = service.tick(&owner, None, 100).await.expect("first page");
        assert_eq!(first.status, AutomationSchedulerTickStatus::Completed);
        assert_eq!(first.definitions_evaluated, 100);
        assert_eq!(first.cursors_initialized, 100);
        assert_eq!(
            first
                .next_definition_cursor
                .as_ref()
                .map(AutomationId::as_str),
            Some("paged-099")
        );
        let second = service
            .tick(&owner, first.next_definition_cursor.as_ref(), 100)
            .await
            .expect("second page");
        assert_eq!(second.definitions_evaluated, 100);
        assert_eq!(
            second
                .next_definition_cursor
                .as_ref()
                .map(AutomationId::as_str),
            Some("paged-199")
        );
        let third = service
            .tick(&owner, second.next_definition_cursor.as_ref(), 100)
            .await
            .expect("third page");
        assert_eq!(third.definitions_evaluated, 5);
        assert_eq!(third.cursors_initialized, 5);
        assert!(third.next_definition_cursor.is_none());
        assert_eq!(
            store
                .automation_scheduler_journal_status()
                .await
                .expect("status")
                .cursor_count,
            205
        );
        assert!(matches!(
            store.list_automation_schedule_candidates(None, 102).await,
            Err(StoreError::Conflict)
        ));
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn automation_scheduler_service_materializes_time_policy_and_dst_edges() {
        let store = Arc::new(InMemoryExecutionStore::new());
        let run_once = seed_scheduler_automation(
            &store,
            "service-run-once",
            MissedRunPolicy::RunOnce,
            OverlapPolicy::Skip,
            0,
        )
        .await;
        let skip = seed_scheduler_automation(
            &store,
            "service-skip",
            MissedRunPolicy::Skip,
            OverlapPolicy::Skip,
            0,
        )
        .await;
        let clock = Arc::new(FixedClock::new(0));
        let service = AutomationSchedulerService::new(
            store.clone(),
            clock.clone(),
            Arc::new(SequentialIdGenerator::new()),
        );
        let owner = AutomationSchedulerOwnerId::new("service-policy-owner").expect("owner");
        let initialized = service.tick(&owner, None, 10).await.expect("initialize");
        assert_eq!(initialized.status, AutomationSchedulerTickStatus::Completed);
        assert_eq!(initialized.cursors_initialized, 2);
        assert_eq!(initialized.occurrences_materialized, 0);
        let forward = 3 * SCHEDULER_TEST_DAY_MS + 1;
        clock.set(forward);
        let jumped = service.tick(&owner, None, 10).await.expect("forward jump");
        assert_eq!(jumped.cursors_initialized, 0);
        assert_eq!(jumped.occurrences_materialized, 2);
        let run_once_occurrences = store
            .list_automation_occurrences(&run_once.id, None, 10)
            .await
            .expect("run-once occurrences");
        assert_eq!(run_once_occurrences.len(), 1);
        assert_eq!(
            run_once_occurrences[0].state,
            AutomationOccurrenceState::Pending
        );
        assert_eq!(run_once_occurrences[0].occurrence_count, 3);
        assert_eq!(
            run_once_occurrences[0].scheduled_for,
            Some(3 * SCHEDULER_TEST_DAY_MS)
        );
        let skipped_occurrences = store
            .list_automation_occurrences(&skip.id, None, 10)
            .await
            .expect("skip occurrences");
        assert_eq!(skipped_occurrences.len(), 1);
        assert_eq!(
            skipped_occurrences[0].state,
            AutomationOccurrenceState::SkippedMissed
        );
        assert_eq!(skipped_occurrences[0].occurrence_count, 3);
        let skipped_history = store
            .automation_history(&skip.id, 0, 10)
            .await
            .expect("skip history");
        assert_eq!(skipped_history.len(), 1);
        assert_eq!(
            skipped_history[0].status,
            AutomationHistoryStatus::SkippedMissed
        );

        let before_rollback = store
            .list_automation_schedule_candidates(None, 10)
            .await
            .expect("cursors before rollback");
        let status_before_rollback = store
            .automation_scheduler_journal_status()
            .await
            .expect("status before rollback");
        clock.set(forward - 1);
        let rollback = service.tick(&owner, None, 10).await.expect("rollback tick");
        assert_eq!(
            rollback.status,
            AutomationSchedulerTickStatus::ClockRegressed
        );
        assert_eq!(rollback.durable_clock_floor, Some(forward));
        assert_eq!(rollback.definitions_evaluated, 0);
        assert_eq!(
            store
                .list_automation_schedule_candidates(None, 10)
                .await
                .expect("cursors after rollback"),
            before_rollback
        );
        assert_eq!(
            store
                .automation_scheduler_journal_status()
                .await
                .expect("status after rollback"),
            status_before_rollback
        );

        let continuous_store = Arc::new(InMemoryExecutionStore::new());
        let continuous = seed_scheduler_automation_with_schedule(
            &continuous_store,
            "service-continuous",
            MissedRunPolicy::RunOnce,
            OverlapPolicy::Skip,
            "v1;daily;00:01",
            "UTC",
            50_000,
        )
        .await;
        let continuous_clock = Arc::new(FixedClock::new(50_000));
        let continuous_service = AutomationSchedulerService::new(
            continuous_store.clone(),
            continuous_clock.clone(),
            Arc::new(SequentialIdGenerator::new()),
        );
        let continuous_owner = AutomationSchedulerOwnerId::new("continuous-owner").expect("owner");
        let first = continuous_service
            .tick(&continuous_owner, None, 10)
            .await
            .expect("continuous initialization");
        assert_eq!(first.cursors_initialized, 1);
        continuous_clock.set(60_000);
        let due = continuous_service
            .tick(&continuous_owner, None, 10)
            .await
            .expect("continuous due tick");
        assert!(due.lease_continuous);
        assert_eq!(due.occurrences_materialized, 1);
        let due_occurrences = continuous_store
            .list_automation_occurrences(&continuous.id, None, 10)
            .await
            .expect("continuous occurrence");
        assert_eq!(due_occurrences.len(), 1);
        assert_eq!(due_occurrences[0].scheduled_for, Some(60_000));
        assert_eq!(due_occurrences[0].occurrence_count, 1);
        assert_eq!(due_occurrences[0].state, AutomationOccurrenceState::Pending);

        let gap_store = Arc::new(InMemoryExecutionStore::new());
        let gap_anchor = 1_774_661_400_000;
        let gap_observed = 1_774_747_800_000;
        let gap = seed_scheduler_automation_with_schedule(
            &gap_store,
            "service-dst-gap",
            MissedRunPolicy::RunOnce,
            OverlapPolicy::Skip,
            "v1;daily;02:30",
            "Europe/Paris",
            gap_anchor,
        )
        .await;
        let gap_clock = Arc::new(FixedClock::new(gap_anchor));
        let gap_service = AutomationSchedulerService::new(
            gap_store.clone(),
            gap_clock.clone(),
            Arc::new(SequentialIdGenerator::new()),
        );
        let gap_owner = AutomationSchedulerOwnerId::new("gap-owner").expect("owner");
        gap_service
            .tick(&gap_owner, None, 10)
            .await
            .expect("gap initialization");
        gap_clock.set(gap_observed);
        let gap_tick = gap_service
            .tick(&gap_owner, None, 10)
            .await
            .expect("gap tick");
        assert_eq!(gap_tick.occurrences_materialized, 1);
        let gap_occurrences = gap_store
            .list_automation_occurrences(&gap.id, None, 10)
            .await
            .expect("gap occurrence");
        assert_eq!(gap_occurrences.len(), 1);
        assert_eq!(
            gap_occurrences[0].state,
            AutomationOccurrenceState::SkippedInvalidLocalTime
        );
        assert_eq!(gap_occurrences[0].scheduled_for, None);
        assert!(
            gap_store
                .automation_history(&gap.id, 0, 10)
                .await
                .expect("gap history")
                .is_empty()
        );

        let fold_store = Arc::new(InMemoryExecutionStore::new());
        let fold_anchor = 1_792_801_800_000;
        let fold_earlier_utc = 1_792_888_200_000;
        let fold_later_utc = 1_792_891_800_000;
        let fold = seed_scheduler_automation_with_schedule(
            &fold_store,
            "service-dst-fold",
            MissedRunPolicy::RunOnce,
            OverlapPolicy::Skip,
            "v1;daily;02:30",
            "Europe/Paris",
            fold_anchor,
        )
        .await;
        let fold_clock = Arc::new(FixedClock::new(fold_anchor));
        let fold_service = AutomationSchedulerService::new(
            fold_store.clone(),
            fold_clock.clone(),
            Arc::new(SequentialIdGenerator::new()),
        );
        let fold_owner = AutomationSchedulerOwnerId::new("fold-owner").expect("owner");
        fold_service
            .tick(&fold_owner, None, 10)
            .await
            .expect("fold initialization");
        fold_clock.set(fold_earlier_utc);
        let fold_tick = fold_service
            .tick(&fold_owner, None, 10)
            .await
            .expect("fold tick");
        assert_eq!(fold_tick.occurrences_materialized, 1);
        let fold_occurrences = fold_store
            .list_automation_occurrences(&fold.id, None, 10)
            .await
            .expect("fold occurrence");
        assert_eq!(fold_occurrences.len(), 1);
        assert_eq!(fold_occurrences[0].scheduled_for, Some(fold_earlier_utc));
        assert_ne!(fold_occurrences[0].scheduled_for, Some(fold_later_utc));
        fold_clock.set(fold_earlier_utc + 1);
        assert_eq!(
            fold_service
                .tick(&fold_owner, None, 10)
                .await
                .expect("post-fold tick")
                .occurrences_materialized,
            0
        );
        assert_eq!(
            fold_store
                .list_automation_occurrences(&fold.id, None, 10)
                .await
                .expect("one fold occurrence")
                .len(),
            1
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn automation_scheduler_rejects_noncanonical_initialization_and_oversized_batches() {
        let store = InMemoryExecutionStore::new();
        let observed_at = 4 * SCHEDULER_TEST_DAY_MS;
        let lease = acquire_scheduler_test_lease(&store, "bounds-owner", observed_at).await;
        let automation = seed_scheduler_automation(
            &store,
            "scheduler-bounds",
            MissedRunPolicy::RunOnce,
            OverlapPolicy::Skip,
            0,
        )
        .await;
        let snapshot = scheduler_test_snapshot(&automation);
        let skipped_next = snapshot
            .schedule
            .next_decision_after(&snapshot.timezone, SCHEDULER_TEST_DAY_MS)
            .expect("later decision");
        let invalid_anchor = AutomationScheduleCursor::new(
            automation.id.clone(),
            &snapshot,
            SCHEDULER_TEST_DAY_MS,
            Some(skipped_next),
            observed_at,
        )
        .expect("structurally valid cursor");
        assert!(matches!(
            store
                .commit_automation_schedule_evaluation(AutomationScheduleEvaluationCommit {
                    lease: lease.clone(),
                    expected_automation_revision: automation.revision,
                    expected_cursor_revision: None,
                    cursor: invalid_anchor,
                    occurrences: Vec::new(),
                    observed_at,
                    command: scheduler_test_command(
                        AUTOMATION_EVALUATION_SCOPE,
                        "invalid-anchor",
                        10,
                    ),
                })
                .await,
            Err(StoreError::Conflict)
        ));
        assert!(
            store
                .list_automation_schedule_candidates(None, 1)
                .await
                .expect("candidate remains readable")[0]
                .cursor
                .is_none()
        );

        let forged_cursor = AutomationScheduleCursor::new(
            automation.id.clone(),
            &snapshot,
            automation.updated_at,
            Some(skipped_next),
            observed_at,
        )
        .expect("structurally valid forged next decision");
        assert!(matches!(
            store
                .commit_automation_schedule_evaluation(AutomationScheduleEvaluationCommit {
                    lease: lease.clone(),
                    expected_automation_revision: automation.revision,
                    expected_cursor_revision: None,
                    cursor: forged_cursor,
                    occurrences: Vec::new(),
                    observed_at,
                    command: scheduler_test_command(
                        AUTOMATION_EVALUATION_SCOPE,
                        "forged-next",
                        11,
                    ),
                })
                .await,
            Err(StoreError::Conflict)
        ));

        let initial = initialize_scheduler_test_cursor(
            &store,
            &automation,
            &lease,
            observed_at,
            "bounds-init",
            12,
        )
        .await;
        let advanced = scheduler_test_advancement(&initial, &snapshot, observed_at, observed_at);
        let four_occurrences = scheduler_test_pending_occurrences(
            &automation,
            &snapshot,
            0,
            observed_at,
            observed_at,
            "oversized-slot",
        );
        assert_eq!(four_occurrences.len(), 4);
        assert!(matches!(
            store
                .commit_automation_schedule_evaluation(AutomationScheduleEvaluationCommit {
                    lease: lease.clone(),
                    expected_automation_revision: automation.revision,
                    expected_cursor_revision: Some(initial.revision),
                    cursor: advanced,
                    occurrences: four_occurrences,
                    observed_at,
                    command: scheduler_test_command(
                        AUTOMATION_EVALUATION_SCOPE,
                        "oversized-evaluation",
                        13,
                    ),
                })
                .await,
            Err(StoreError::Conflict)
        ));
        let candidate = store
            .list_automation_schedule_candidates(None, 1)
            .await
            .expect("candidate after rollback")
            .pop()
            .expect("candidate");
        assert_eq!(candidate.cursor, Some(initial.clone()));
        assert!(
            store
                .list_automation_occurrences(&automation.id, None, 10)
                .await
                .expect("no partial occurrences")
                .is_empty()
        );

        for invalid_limit in [0, MAX_AUTOMATION_SCHEDULER_RECOVERY_BATCH + 1] {
            assert!(matches!(
                store
                    .recover_automation_occurrence_claims(&lease, observed_at, invalid_limit)
                    .await,
                Err(StoreError::Conflict)
            ));
        }
        for invalid_limit in [0, MAX_AUTOMATION_OCCURRENCE_PAGE_SIZE + 1] {
            assert!(matches!(
                store
                    .list_automation_occurrences(&automation.id, None, invalid_limit)
                    .await,
                Err(StoreError::Conflict)
            ));
        }

        let mut updated = automation.clone();
        updated
            .update(
                updated.title.clone(),
                updated.prompt.clone(),
                updated.schedule.clone(),
                updated.timezone.clone(),
                updated.missed_run_policy,
                updated.overlap_policy,
                true,
                observed_at + 1,
            )
            .expect("definition update");
        assert!(matches!(
            store
                .save_automation(
                    updated,
                    automation.revision,
                    &scheduler_test_command("update_automation", "cursor-rebase", 14),
                )
                .await,
            Err(StoreError::Conflict)
        ));

        {
            let mut state = store.state.lock().await;
            state
                .automation_schedule_cursors
                .get_mut(&automation.id)
                .expect("stored cursor")
                .next_decision = Some(skipped_next);
        }
        assert!(matches!(
            store.list_automation_schedule_candidates(None, 1).await,
            Err(StoreError::Internal(_))
        ));
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn automation_schedule_evaluation_is_atomic_replay_safe_and_applies_policies() {
        let store = InMemoryExecutionStore::new();
        let observed_at = 3 * SCHEDULER_TEST_DAY_MS;
        let lease = acquire_scheduler_test_lease(&store, "evaluation-owner", observed_at).await;
        let automation = seed_scheduler_automation(
            &store,
            "overlap-evaluation",
            MissedRunPolicy::RunOnce,
            OverlapPolicy::QueueOne,
            0,
        )
        .await;
        let snapshot = scheduler_test_snapshot(&automation);
        let initial = initialize_scheduler_test_cursor(
            &store,
            &automation,
            &lease,
            observed_at,
            "overlap-init",
            1,
        )
        .await;
        let advanced = scheduler_test_advancement(&initial, &snapshot, observed_at, observed_at);
        let proposed = scheduler_test_pending_occurrences(
            &automation,
            &snapshot,
            initial.evaluated_through,
            observed_at,
            observed_at,
            "overlap-slot",
        );
        assert_eq!(proposed.len(), 3);
        let evaluation = AutomationScheduleEvaluationCommit {
            lease: lease.clone(),
            expected_automation_revision: automation.revision,
            expected_cursor_revision: Some(initial.revision),
            cursor: advanced.clone(),
            occurrences: proposed,
            observed_at,
            command: scheduler_test_command(AUTOMATION_EVALUATION_SCOPE, "overlap-evaluation", 2),
        };
        let committed = store
            .commit_automation_schedule_evaluation(evaluation.clone())
            .await
            .expect("commit overlap evaluation");
        assert_eq!(
            committed
                .occurrences
                .iter()
                .map(|occurrence| occurrence.state)
                .collect::<Vec<_>>(),
            vec![
                AutomationOccurrenceState::Pending,
                AutomationOccurrenceState::QueuedOverlap,
                AutomationOccurrenceState::SkippedOverlap,
            ]
        );
        assert_eq!(
            store
                .commit_automation_schedule_evaluation(evaluation.clone())
                .await
                .expect("exact evaluation replay"),
            committed
        );
        let mut conflicting_replay = evaluation;
        conflicting_replay.command.fingerprint = [3; 32];
        assert!(matches!(
            store
                .commit_automation_schedule_evaluation(conflicting_replay)
                .await,
            Err(StoreError::Conflict)
        ));
        let history = store
            .automation_history(&automation.id, 0, 10)
            .await
            .expect("overlap history");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].status, AutomationHistoryStatus::SkippedOverlap);
        assert_eq!(history[0].recorded_at, observed_at);

        let missed = seed_scheduler_automation(
            &store,
            "missed-evaluation",
            MissedRunPolicy::Skip,
            OverlapPolicy::Skip,
            0,
        )
        .await;
        let missed_snapshot = scheduler_test_snapshot(&missed);
        let missed_initial = initialize_scheduler_test_cursor(
            &store,
            &missed,
            &lease,
            observed_at,
            "missed-init",
            4,
        )
        .await;
        let missed_through = 2 * SCHEDULER_TEST_DAY_MS;
        let missed_decisions = missed_snapshot
            .schedule
            .decisions_between(
                &missed_snapshot.timezone,
                0,
                missed_through,
                MAX_AUTOMATION_SCHEDULE_DECISIONS,
            )
            .expect("missed decisions")
            .decisions;
        let mut skipped = AutomationOccurrence::pending(
            AutomationOccurrenceId::new("missed-collapsed").expect("id"),
            missed.id.clone(),
            missed_snapshot.clone(),
            *missed_decisions.last().expect("latest missed"),
            u32::try_from(missed_decisions.len()).expect("count"),
            observed_at,
        )
        .expect("missed occurrence");
        skipped.skip_missed(observed_at).expect("skip missed");
        let missed_cursor = scheduler_test_advancement(
            &missed_initial,
            &missed_snapshot,
            missed_through,
            observed_at,
        );
        let missed_result = store
            .commit_automation_schedule_evaluation(AutomationScheduleEvaluationCommit {
                lease: lease.clone(),
                expected_automation_revision: missed.revision,
                expected_cursor_revision: Some(missed_initial.revision),
                cursor: missed_cursor,
                occurrences: vec![skipped],
                observed_at,
                command: scheduler_test_command(
                    AUTOMATION_EVALUATION_SCOPE,
                    "missed-evaluation",
                    5,
                ),
            })
            .await
            .expect("commit missed evaluation");
        assert_eq!(
            missed_result.occurrences[0].state,
            AutomationOccurrenceState::SkippedMissed
        );
        let missed_history = store
            .automation_history(&missed.id, 0, 10)
            .await
            .expect("missed history");
        assert_eq!(missed_history.len(), 1);
        assert_eq!(
            missed_history[0].status,
            AutomationHistoryStatus::SkippedMissed
        );

        let raced = seed_scheduler_automation(
            &store,
            "raced-evaluation",
            MissedRunPolicy::RunOnce,
            OverlapPolicy::Skip,
            0,
        )
        .await;
        let raced_snapshot = scheduler_test_snapshot(&raced);
        let raced_initial =
            initialize_scheduler_test_cursor(&store, &raced, &lease, observed_at, "raced-init", 6)
                .await;
        let raced_through = SCHEDULER_TEST_DAY_MS;
        let raced_cursor =
            scheduler_test_advancement(&raced_initial, &raced_snapshot, raced_through, observed_at);
        let make_race = |key: &str, prefix: &str, byte| AutomationScheduleEvaluationCommit {
            lease: lease.clone(),
            expected_automation_revision: raced.revision,
            expected_cursor_revision: Some(raced_initial.revision),
            cursor: raced_cursor.clone(),
            occurrences: scheduler_test_pending_occurrences(
                &raced,
                &raced_snapshot,
                0,
                raced_through,
                observed_at,
                prefix,
            ),
            observed_at,
            command: scheduler_test_command(AUTOMATION_EVALUATION_SCOPE, key, byte),
        };
        let (left, right) = tokio::join!(
            store.commit_automation_schedule_evaluation(make_race("race-left", "left", 7)),
            store.commit_automation_schedule_evaluation(make_race("race-right", "right", 8)),
        );
        assert!(matches!(
            (&left, &right),
            (Ok(_), Err(StoreError::Conflict)) | (Err(StoreError::Conflict), Ok(_))
        ));
        assert_eq!(
            store
                .list_automation_occurrences(&raced.id, None, 10)
                .await
                .expect("one raced occurrence")
                .len(),
            1
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn automation_occurrence_claim_recovery_is_bounded_and_never_replays_linked_work() {
        let store = InMemoryExecutionStore::new();
        let observed_at = 3 * SCHEDULER_TEST_DAY_MS;
        let lease = acquire_scheduler_test_lease(&store, "claim-owner", observed_at).await;
        let automation = seed_scheduler_automation(
            &store,
            "claim-recovery",
            MissedRunPolicy::RunOnce,
            OverlapPolicy::QueueOne,
            0,
        )
        .await;
        let snapshot = scheduler_test_snapshot(&automation);
        let initial = initialize_scheduler_test_cursor(
            &store,
            &automation,
            &lease,
            observed_at,
            "claim-init",
            20,
        )
        .await;
        let through = 2 * SCHEDULER_TEST_DAY_MS;
        let cursor = scheduler_test_advancement(&initial, &snapshot, through, observed_at);
        let materialized = store
            .commit_automation_schedule_evaluation(AutomationScheduleEvaluationCommit {
                lease: lease.clone(),
                expected_automation_revision: automation.revision,
                expected_cursor_revision: Some(initial.revision),
                cursor,
                occurrences: scheduler_test_pending_occurrences(
                    &automation,
                    &snapshot,
                    0,
                    through,
                    observed_at,
                    "claim-slot",
                ),
                observed_at,
                command: scheduler_test_command(
                    AUTOMATION_EVALUATION_SCOPE,
                    "claim-materialize",
                    21,
                ),
            })
            .await
            .expect("materialize claims");
        let mut occurrence = materialized.occurrences[0].clone();
        assert_eq!(
            materialized.occurrences[1].state,
            AutomationOccurrenceState::QueuedOverlap
        );
        assert!(matches!(
            store
                .claim_automation_occurrence(ClaimAutomationOccurrence {
                    lease: lease.clone(),
                    occurrence_id: occurrence.id.clone(),
                    expected_revision: occurrence.revision,
                    claimed_at: observed_at,
                    expires_at: observed_at + MAX_AUTOMATION_SCHEDULER_LEASE_MS + 1,
                    command: scheduler_test_command(AUTOMATION_CLAIM_SCOPE, "oversized-claim", 22,),
                })
                .await,
            Err(StoreError::Conflict)
        ));
        for attempt in 1..=MAX_AUTOMATION_OCCURRENCE_CLAIM_ATTEMPTS {
            let claimed_at = observed_at + u64::from(attempt) * 2;
            let expires_at = claimed_at + 1;
            occurrence = store
                .claim_automation_occurrence(ClaimAutomationOccurrence {
                    lease: lease.clone(),
                    occurrence_id: occurrence.id.clone(),
                    expected_revision: occurrence.revision,
                    claimed_at,
                    expires_at,
                    command: scheduler_test_command(
                        AUTOMATION_CLAIM_SCOPE,
                        format!("claim-attempt-{attempt}"),
                        u8::try_from(attempt).expect("bounded attempt"),
                    ),
                })
                .await
                .expect("claim occurrence");
            let early = store
                .recover_automation_occurrence_claims(&lease, claimed_at, 1)
                .await
                .expect("unexpired claim is ignored");
            assert_eq!(early, AutomationSchedulerRecoverySummary::default());
            let recovered = store
                .recover_automation_occurrence_claims(&lease, expires_at, 1)
                .await
                .expect("recover claim");
            occurrence = store
                .get_automation_occurrence(&occurrence.id)
                .await
                .expect("recovered occurrence");
            if attempt == MAX_AUTOMATION_OCCURRENCE_CLAIM_ATTEMPTS {
                assert_eq!(recovered.attempts_exhausted, 1);
                assert_eq!(
                    occurrence.state,
                    AutomationOccurrenceState::InterruptedNeedsReview
                );
            } else {
                assert_eq!(recovered.released_unlinked, 1);
                assert_eq!(occurrence.state, AutomationOccurrenceState::Pending);
            }
        }
        let queued = store
            .get_automation_occurrence(&materialized.occurrences[1].id)
            .await
            .expect("promoted queued occurrence");
        assert_eq!(queued.state, AutomationOccurrenceState::Pending);
        {
            let state = store.state.lock().await;
            let attempts = state
                .automation_occurrence_claim_attempts
                .get(&occurrence.id)
                .expect("attempt evidence");
            assert_eq!(
                attempts.len(),
                usize::try_from(MAX_AUTOMATION_OCCURRENCE_CLAIM_ATTEMPTS).expect("bound")
            );
            assert!(
                attempts
                    .iter()
                    .all(|attempt| attempt.completed_at.is_some())
            );
            assert_eq!(
                attempts.last().and_then(|attempt| attempt.completion),
                Some(AutomationOccurrenceClaimCompletion::AttemptsExhausted)
            );
        }

        let linked = seed_scheduler_automation(
            &store,
            "linked-recovery",
            MissedRunPolicy::RunOnce,
            OverlapPolicy::QueueOne,
            0,
        )
        .await;
        let linked_snapshot = scheduler_test_snapshot(&linked);
        let linked_initial = initialize_scheduler_test_cursor(
            &store,
            &linked,
            &lease,
            observed_at,
            "linked-init",
            40,
        )
        .await;
        let linked_cursor =
            scheduler_test_advancement(&linked_initial, &linked_snapshot, through, observed_at);
        let linked_materialized = store
            .commit_automation_schedule_evaluation(AutomationScheduleEvaluationCommit {
                lease: lease.clone(),
                expected_automation_revision: linked.revision,
                expected_cursor_revision: Some(linked_initial.revision),
                cursor: linked_cursor,
                occurrences: scheduler_test_pending_occurrences(
                    &linked,
                    &linked_snapshot,
                    0,
                    through,
                    observed_at,
                    "linked-slot",
                ),
                observed_at,
                command: scheduler_test_command(
                    AUTOMATION_EVALUATION_SCOPE,
                    "linked-materialize",
                    41,
                ),
            })
            .await
            .expect("materialize linked slots");
        let claim_time = observed_at + 100;
        let claim_expiry = claim_time + 10;
        let claimed = store
            .claim_automation_occurrence(ClaimAutomationOccurrence {
                lease: lease.clone(),
                occurrence_id: linked_materialized.occurrences[0].id.clone(),
                expected_revision: linked_materialized.occurrences[0].revision,
                claimed_at: claim_time,
                expires_at: claim_expiry,
                command: scheduler_test_command(AUTOMATION_CLAIM_SCOPE, "linked-claim", 42),
            })
            .await
            .expect("claim linked slot");
        let run_id = RunId::new("linked-run").expect("run id");
        {
            let mut state = store.state.lock().await;
            let stored = state
                .automation_occurrences
                .get_mut(&claimed.id)
                .expect("stored claim");
            stored
                .link_run(&lease, run_id.clone(), claim_time + 1)
                .expect("link run");
        }
        let recovered = store
            .recover_automation_occurrence_claims(&lease, claim_expiry, 1)
            .await
            .expect("recover linked claim");
        assert_eq!(recovered.interrupted_linked, 1);
        let interrupted = store
            .get_automation_occurrence(&claimed.id)
            .await
            .expect("interrupted occurrence");
        assert_eq!(
            interrupted.state,
            AutomationOccurrenceState::InterruptedNeedsReview
        );
        assert_eq!(interrupted.run_id, Some(run_id));
        assert_eq!(
            store
                .get_automation_occurrence(&linked_materialized.occurrences[1].id)
                .await
                .expect("linked successor")
                .state,
            AutomationOccurrenceState::Pending
        );
        let status = store
            .automation_scheduler_journal_status()
            .await
            .expect("journal status");
        assert_eq!(status.needs_review_count, 2);
        assert_eq!(status.run_linked_count, 0);
    }

    async fn conversation_store_fixture(
        prefix: &str,
    ) -> (Arc<InMemoryExecutionStore>, ConversationTurnSnapshot) {
        let store = Arc::new(InMemoryExecutionStore::new());
        let workspace = WorkspaceService::new(
            store.clone(),
            Arc::new(FixedClock::new(10)),
            Arc::new(SequentialIdGenerator::new()),
        );
        let project = workspace
            .create_project(
                CreateProject {
                    name: format!("{prefix} project"),
                    description: String::new(),
                },
                &format!("{prefix}-project-command"),
            )
            .await
            .expect("project");
        let thread = workspace
            .create_thread(
                CreateThread {
                    project_id: project.id.to_string(),
                    title: format!("{prefix} thread"),
                },
                &format!("{prefix}-thread-command"),
            )
            .await
            .expect("thread");
        let (turn, user, run, event) =
            canonical_reservation_inputs(project.id, thread.id, prefix, 10, "User request");
        let reservation = store
            .reserve_turn(
                turn,
                original_test_lineage(),
                ConversationTurnReservationSource::CurrentThread,
                user,
                run,
                event,
                ConversationTurnEventKind::Created,
            )
            .await
            .expect("reservation");
        (store, reservation.snapshot)
    }

    async fn completed_conversation_store_fixture(
        prefix: &str,
    ) -> (Arc<InMemoryExecutionStore>, ConversationTurnSnapshot) {
        let (store, reserved) = conversation_store_fixture(prefix).await;
        let context = store
            .load_turn_context(&reserved.turn.id)
            .await
            .expect("source context");
        let mut provider_start = canonical_provider_start(&reserved, prefix);
        provider_start.turn.provider_request_fingerprint = Some(test_provider_request_fingerprint(
            &reserved.turn.model_id,
            &context,
        ));
        let started = store
            .commit_provider_start(provider_start)
            .await
            .expect("provider start");
        store
            .append_turn_text(
                &started.turn.id,
                started.turn.revision,
                0,
                "Canonical answer".into(),
            )
            .await
            .expect("assistant text");
        let completed = store
            .commit_terminal(canonical_completed_terminal(&started, prefix))
            .await
            .expect("completed source");
        (store, completed)
    }

    fn test_provider_request_fingerprint(model_id: &str, context: &[Message]) -> [u8; 32] {
        fn part(hasher: &mut Sha256, value: &[u8]) {
            hasher.update(
                u64::try_from(value.len())
                    .expect("test value length")
                    .to_be_bytes(),
            );
            hasher.update(value);
        }
        let mut hasher = Sha256::new();
        part(&mut hasher, model_id.as_bytes());
        part(&mut hasher, &[0]);
        for message in context {
            part(
                &mut hasher,
                match message.role {
                    MessageRole::System => b"system",
                    MessageRole::User => b"user",
                    MessageRole::Assistant => b"assistant",
                },
            );
            part(&mut hasher, b"text");
            part(&mut hasher, message.content.as_bytes());
        }
        hasher.finalize().into()
    }

    #[allow(clippy::too_many_lines)]
    fn canonical_fork_plan(
        state: &State,
        source: &ConversationTurnSnapshot,
        kind: ConversationForkKind,
        prefix: &str,
        now: UnixMillis,
    ) -> ConversationForkPlan {
        let parent = state
            .threads
            .get(&source.turn.thread_id)
            .expect("source parent");
        let source_context = state
            .conversation_contexts
            .get(&source.turn.id)
            .expect("source context");
        let source_message = match kind {
            ConversationForkKind::EditAndBranch => &source.user_message,
            ConversationForkKind::Branch | ConversationForkKind::Regenerate => {
                source.assistant_message.as_ref().expect("source assistant")
            }
        };
        let child = Thread::new_fork(
            ThreadId::new(format!("{prefix}-child-thread")).expect("child thread ID"),
            parent.project_id.clone(),
            parent.title.clone(),
            parent.id.clone(),
            &parent.lineage,
            source.turn.id.clone(),
            source_message.id.clone(),
            source_message.role,
            kind,
            now,
        )
        .expect("child thread");
        let copy_count = if kind == ConversationForkKind::EditAndBranch {
            source_context.len() - 1
        } else {
            source_context.len()
        };
        let mut messages = source_context
            .iter()
            .take(copy_count)
            .enumerate()
            .map(|(index, message)| {
                Message::new_derived(
                    MessageId::new(format!("{prefix}-copy-{}", index + 1)).expect("copy ID"),
                    child.id.clone(),
                    u64::try_from(index + 1).expect("sequence"),
                    message.role,
                    message.content.clone(),
                    message.id.clone(),
                    source.turn.id.clone(),
                    Some(u32::try_from(index + 1).expect("context position")),
                    ConversationMessageDerivationKind::ContextCopy,
                    now,
                )
                .expect("context copy")
            })
            .collect::<Vec<_>>();
        if kind == ConversationForkKind::EditAndBranch {
            messages.push(
                Message::new_derived(
                    MessageId::new(format!("{prefix}-edited-user")).expect("edited ID"),
                    child.id.clone(),
                    u64::try_from(messages.len() + 1).expect("sequence"),
                    MessageRole::User,
                    "Edited request".into(),
                    source.user_message.id.clone(),
                    source.turn.id.clone(),
                    Some(u32::try_from(source_context.len()).expect("context position")),
                    ConversationMessageDerivationKind::EditedUser,
                    now,
                )
                .expect("edited user"),
            );
        } else if kind == ConversationForkKind::Branch {
            let assistant = source.assistant_message.as_ref().expect("assistant");
            messages.push(
                Message::new_derived(
                    MessageId::new(format!("{prefix}-assistant-copy")).expect("assistant copy ID"),
                    child.id.clone(),
                    u64::try_from(messages.len() + 1).expect("sequence"),
                    MessageRole::Assistant,
                    assistant.content.clone(),
                    assistant.id.clone(),
                    source.turn.id.clone(),
                    None,
                    ConversationMessageDerivationKind::SourceAssistantCopy,
                    now,
                )
                .expect("assistant copy"),
            );
        }
        let (scope, kind_name) = match kind {
            ConversationForkKind::Branch => (CONVERSATION_BRANCH_COMMAND_SCOPE, "branch"),
            ConversationForkKind::EditAndBranch => {
                (CONVERSATION_EDIT_BRANCH_COMMAND_SCOPE, "edit_and_branch")
            }
            ConversationForkKind::Regenerate => {
                (CONVERSATION_REGENERATE_COMMAND_SCOPE, "regenerate")
            }
        };
        let mut command_hasher = Sha256::new();
        command_hasher.update(scope.as_bytes());
        command_hasher.update([0]);
        command_hasher.update(kind_name.as_bytes());
        command_hasher.update([0]);
        command_hasher.update(source.turn.id.as_str().as_bytes());
        command_hasher.update(source.turn.revision.to_be_bytes());
        if kind == ConversationForkKind::EditAndBranch {
            command_hasher.update(b"Edited request");
        }
        let command = MutationCommand {
            scope: scope.into(),
            key: format!("fork-{prefix}"),
            fingerprint: command_hasher.finalize().into(),
        };
        let started_turn = match kind {
            ConversationForkKind::Branch => None,
            ConversationForkKind::EditAndBranch | ConversationForkKind::Regenerate => {
                let user = messages.last().expect("fork user");
                let run = Run::queued(
                    RunId::new(format!("{prefix}-fork-run")).expect("fork run ID"),
                    child.project_id.clone(),
                    child.id.clone(),
                    now,
                );
                let turn = ConversationTurn::reserve(
                    ConversationTurnId::new(format!("{prefix}-fork-turn")).expect("fork turn ID"),
                    command.key.clone(),
                    command.fingerprint,
                    child.project_id.clone(),
                    child.id.clone(),
                    user.id.clone(),
                    run.id.clone(),
                    source.turn.model_id.clone(),
                    now,
                )
                .expect("fork turn");
                let binding = source
                    .lineage
                    .credential_binding_id
                    .clone()
                    .expect("source binding");
                let lineage = match kind {
                    ConversationForkKind::EditAndBranch => {
                        ConversationTurnLineage::edit_and_branch(source.turn.id.clone(), binding)
                            .expect("edit lineage")
                    }
                    ConversationForkKind::Regenerate => {
                        ConversationTurnLineage::regenerate(source.turn.id.clone(), binding)
                            .expect("regenerate lineage")
                    }
                    ConversationForkKind::Branch => unreachable!(),
                };
                Some(ConversationForkTurnPlan {
                    turn,
                    lineage,
                    run,
                    run_event: NewRunEvent {
                        occurred_at: now,
                        kind: RunEventKind::Created,
                    },
                    turn_event: ConversationTurnEventKind::Created,
                })
            }
        };
        ConversationForkPlan {
            command,
            source_turn_id: source.turn.id.clone(),
            expected_source_revision: source.turn.revision,
            child_thread: child,
            messages,
            started_turn,
        }
    }

    fn conversation_fork_service(
        store: Arc<InMemoryExecutionStore>,
        model: Arc<RetryableFailureModel>,
        clock: Arc<FixedClock>,
        credential_binding_id: Option<&str>,
    ) -> ConversationService {
        let vault = Arc::new(InMemorySecretVault::new());
        if let Some(binding) = credential_binding_id {
            seed_test_xai_credential_with_binding(&vault, binding);
        }
        let credentials = Arc::new(CredentialService::new(
            vault,
            store.clone(),
            Arc::new(AcceptXaiKey),
        ));
        let workspace = Arc::new(WorkspaceService::new(
            store.clone(),
            clock.clone(),
            Arc::new(UuidGenerator),
        ));
        ConversationService::new(
            store.clone(),
            workspace,
            credentials,
            Arc::new(RetryableFailureFactory(model)),
            clock,
            Arc::new(UuidGenerator),
            store,
        )
    }

    #[tokio::test]
    async fn conversation_branch_is_provider_free_exact_and_parent_immutable() {
        let (store, source) = completed_conversation_store_fixture("fork-branch").await;
        store
            .state
            .lock()
            .await
            .threads
            .get_mut(&source.turn.thread_id)
            .expect("source thread")
            .archive(15)
            .expect("archive source without mutating its history");
        let list_calls = Arc::new(AtomicUsize::new(0));
        let stream_calls = Arc::new(AtomicUsize::new(0));
        let model = Arc::new(RetryableFailureModel {
            list_calls: list_calls.clone(),
            stream_calls: stream_calls.clone(),
            requests: StdMutex::new(Vec::new()),
        });
        let service =
            conversation_fork_service(store.clone(), model, Arc::new(FixedClock::new(20)), None);
        let parent_before = store
            .list_messages(&source.turn.thread_id, None, 100)
            .await
            .expect("parent history");
        let input = BranchConversationThread {
            source_turn_id: source.turn.id.to_string(),
            expected_revision: source.turn.revision,
        };
        let first = service
            .branch(input.clone(), "branch-command")
            .await
            .expect("branch");
        assert!(first.dispatch.is_none());
        assert!(!first.reconciled_pending_delivery);
        assert!(first.snapshot.started_turn.is_none());
        assert_eq!(
            first.snapshot.delivery,
            ConversationForkDelivery {
                child_thread_id: first.snapshot.child_thread.id.clone(),
                state: ConversationForkDeliveryState::Pending,
                revision: 0,
            }
        );
        assert_eq!(first.snapshot.messages.len(), 2);
        assert_eq!(
            first.snapshot.messages[0].content,
            source.user_message.content
        );
        assert_eq!(first.snapshot.messages[1].content, "Canonical answer");
        assert_eq!(list_calls.load(AtomicOrdering::SeqCst), 0);
        assert_eq!(stream_calls.load(AtomicOrdering::SeqCst), 0);

        let replay = service
            .branch(input.clone(), "branch-command")
            .await
            .expect("branch replay");
        assert_eq!(replay.snapshot, first.snapshot);
        assert_eq!(list_calls.load(AtomicOrdering::SeqCst), 0);
        assert!(matches!(
            service
                .branch(
                    BranchConversationThread {
                        expected_revision: input.expected_revision.saturating_sub(1),
                        ..input
                    },
                    "branch-command",
                )
                .await,
            Err(ApplicationError::Conflict)
        ));
        assert_eq!(
            store
                .list_messages(&source.turn.thread_id, None, 100)
                .await
                .expect("unchanged parent"),
            parent_before
        );
        let metadata = service
            .fork_metadata(&first.snapshot.child_thread.id)
            .await
            .expect("fork metadata");
        assert_eq!(metadata.family_threads.len(), 2);
        assert_eq!(metadata.inherited_assistant_outcomes.len(), 1);
        assert_eq!(
            metadata.inherited_assistant_outcomes[0].source_turn_id,
            source.turn.id
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn conversation_fork_delivery_coalesces_until_exact_ack_then_allows_new_intent() {
        let (store, source) = completed_conversation_store_fixture("fork-delivery").await;
        let list_calls = Arc::new(AtomicUsize::new(0));
        let stream_calls = Arc::new(AtomicUsize::new(0));
        let model = Arc::new(RetryableFailureModel {
            list_calls: list_calls.clone(),
            stream_calls: stream_calls.clone(),
            requests: StdMutex::new(Vec::new()),
        });
        let service =
            conversation_fork_service(store.clone(), model, Arc::new(FixedClock::new(20)), None);
        let input = BranchConversationThread {
            source_turn_id: source.turn.id.to_string(),
            expected_revision: source.turn.revision,
        };

        let first = service
            .branch(input.clone(), "fork-delivery-first")
            .await
            .expect("first pending branch");
        let coalesced = service
            .branch(input.clone(), "fork-delivery-second")
            .await
            .expect("coalesced pending branch");
        assert!(coalesced.reconciled_pending_delivery);
        assert_eq!(coalesced.snapshot, first.snapshot);
        {
            let state = store.state.lock().await;
            assert_eq!(state.conversation_fork_commands.len(), 2);
            assert_eq!(
                state
                    .conversation_fork_commands
                    .values()
                    .filter(|record| record.canonical)
                    .count(),
                1
            );
            assert_eq!(state.conversation_fork_deliveries.len(), 1);
            assert!(state.conversation_fork_delivery_ack_commands.is_empty());
        }

        let ack_input = AcknowledgeConversationForkDelivery {
            child_thread_id: first.snapshot.child_thread.id.to_string(),
            expected_revision: 0,
        };
        let acknowledged = service
            .acknowledge_fork_delivery(ack_input.clone(), "fork-delivery-ack")
            .await
            .expect("acknowledge pending delivery");
        assert_eq!(
            acknowledged.state,
            ConversationForkDeliveryState::Acknowledged
        );
        assert_eq!(acknowledged.revision, 1);
        assert_eq!(
            service
                .acknowledge_fork_delivery(ack_input.clone(), "fork-delivery-ack")
                .await
                .expect("exact acknowledgement replay"),
            acknowledged
        );
        assert!(matches!(
            service
                .acknowledge_fork_delivery(ack_input.clone(), "fork-delivery-new-ack")
                .await,
            Err(ApplicationError::Conflict)
        ));
        assert!(matches!(
            service
                .acknowledge_fork_delivery(
                    AcknowledgeConversationForkDelivery {
                        child_thread_id: source.turn.thread_id.to_string(),
                        expected_revision: 0,
                    },
                    "fork-delivery-ack",
                )
                .await,
            Err(ApplicationError::Conflict)
        ));
        assert!(matches!(
            service
                .acknowledge_fork_delivery(
                    AcknowledgeConversationForkDelivery {
                        expected_revision: 1,
                        ..ack_input
                    },
                    "fork-delivery-ack",
                )
                .await,
            Err(ApplicationError::Conflict)
        ));
        {
            let state = store.state.lock().await;
            assert_eq!(state.conversation_fork_delivery_ack_commands.len(), 1);
        }

        let exact_alias = service
            .branch(input.clone(), "fork-delivery-second")
            .await
            .expect("alias remains exact after acknowledgement");
        assert!(!exact_alias.reconciled_pending_delivery);
        assert_eq!(
            exact_alias.snapshot.child_thread.id,
            first.snapshot.child_thread.id
        );
        assert_eq!(
            exact_alias.snapshot.delivery.state,
            ConversationForkDeliveryState::Acknowledged
        );

        let new_intent = service
            .branch(input, "fork-delivery-third")
            .await
            .expect("acknowledged request permits a new key");
        assert!(!new_intent.reconciled_pending_delivery);
        assert_ne!(
            new_intent.snapshot.child_thread.id,
            first.snapshot.child_thread.id
        );
        assert_eq!(
            new_intent.snapshot.delivery.state,
            ConversationForkDeliveryState::Pending
        );
        let state = store.state.lock().await;
        assert_eq!(state.conversation_fork_deliveries.len(), 2);
        assert_eq!(list_calls.load(AtomicOrdering::SeqCst), 0);
        assert_eq!(stream_calls.load(AtomicOrdering::SeqCst), 0);
    }

    #[tokio::test]
    async fn conversation_dispatching_fork_reconciles_pending_key_before_model_io() {
        let (store, source) = completed_conversation_store_fixture("fork-delivery-model").await;
        let list_calls = Arc::new(AtomicUsize::new(0));
        let stream_calls = Arc::new(AtomicUsize::new(0));
        let model = Arc::new(RetryableFailureModel {
            list_calls: list_calls.clone(),
            stream_calls: stream_calls.clone(),
            requests: StdMutex::new(Vec::new()),
        });
        let service = conversation_fork_service(
            store.clone(),
            model.clone(),
            Arc::new(FixedClock::new(20)),
            Some(TEST_CREDENTIAL_BINDING),
        );
        let input = RegenerateConversationTurn {
            source_turn_id: source.turn.id.to_string(),
            expected_revision: source.turn.revision,
        };
        let first = service
            .regenerate(
                input.clone(),
                "fork-delivery-model-first",
                Box::pin(std::future::pending()),
            )
            .await
            .expect("first regenerate");
        assert!(first.dispatch.is_some());
        assert_eq!(list_calls.load(AtomicOrdering::SeqCst), 1);

        // Simulate restart after the response was lost and the credential was
        // removed. Pending reconciliation must win before credential/model I/O.
        let unavailable_service =
            conversation_fork_service(store, model, Arc::new(FixedClock::new(21)), None);
        let coalesced = unavailable_service
            .regenerate(
                input.clone(),
                "fork-delivery-model-second",
                Box::pin(std::future::pending()),
            )
            .await
            .expect("coalesced regenerate");
        assert!(coalesced.reconciled_pending_delivery);
        assert!(coalesced.dispatch.is_none());
        assert_eq!(
            coalesced.snapshot.child_thread.id,
            first.snapshot.child_thread.id
        );
        assert_eq!(list_calls.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(stream_calls.load(AtomicOrdering::SeqCst), 0);

        service
            .acknowledge_fork_delivery(
                AcknowledgeConversationForkDelivery {
                    child_thread_id: first.snapshot.child_thread.id.to_string(),
                    expected_revision: first.snapshot.delivery.revision,
                },
                "fork-delivery-model-ack",
            )
            .await
            .expect("acknowledge regenerate result");
        let next = service
            .regenerate(
                input,
                "fork-delivery-model-third",
                Box::pin(std::future::pending()),
            )
            .await
            .expect("new regenerate after acknowledgement");
        assert!(!next.reconciled_pending_delivery);
        assert!(next.dispatch.is_some());
        assert_ne!(
            next.snapshot.child_thread.id,
            first.snapshot.child_thread.id
        );
        assert_eq!(list_calls.load(AtomicOrdering::SeqCst), 2);
        assert_eq!(stream_calls.load(AtomicOrdering::SeqCst), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_fork_preflight_reserves_one_child_turn_and_provider_generation() {
        let (store, source) = completed_conversation_store_fixture("fork-delivery-race").await;
        let vault = Arc::new(InMemorySecretVault::new());
        seed_test_xai_credential_with_binding(&vault, TEST_CREDENTIAL_BINDING);
        let credentials = Arc::new(CredentialService::new(
            vault,
            store.clone(),
            Arc::new(AcceptXaiKey),
        ));
        let clock = Arc::new(FixedClock::new(20));
        let workspace = Arc::new(WorkspaceService::new(
            store.clone(),
            clock.clone(),
            Arc::new(UuidGenerator),
        ));
        let list_calls = Arc::new(AtomicUsize::new(0));
        let stream_calls = Arc::new(AtomicUsize::new(0));
        let service = Arc::new(ConversationService::new(
            store.clone(),
            workspace,
            credentials,
            Arc::new(ForkReservationRaceFactory(Arc::new(
                ForkReservationRaceModel {
                    list_barrier: Arc::new(Barrier::new(2)),
                    list_calls: list_calls.clone(),
                    stream_calls: stream_calls.clone(),
                },
            ))),
            clock,
            Arc::new(UuidGenerator),
            store.clone(),
        ));
        let input = RegenerateConversationTurn {
            source_turn_id: source.turn.id.to_string(),
            expected_revision: source.turn.revision,
        };
        let left_service = service.clone();
        let left_input = input.clone();
        let left = tokio::spawn(async move {
            left_service
                .regenerate(
                    left_input,
                    "fork-delivery-race-left",
                    Box::pin(std::future::pending()),
                )
                .await
        });
        let right_service = service.clone();
        let right = tokio::spawn(async move {
            right_service
                .regenerate(
                    input,
                    "fork-delivery-race-right",
                    Box::pin(std::future::pending()),
                )
                .await
        });
        let mut outcomes = [
            left.await.expect("left task").expect("left fork"),
            right.await.expect("right task").expect("right fork"),
        ];
        assert_eq!(list_calls.load(AtomicOrdering::SeqCst), 2);
        assert_eq!(
            outcomes[0].snapshot.child_thread.id,
            outcomes[1].snapshot.child_thread.id
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| outcome.reconciled_pending_delivery)
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| outcome.dispatch.is_some())
                .count(),
            1
        );
        let dispatch = outcomes
            .iter_mut()
            .find_map(|outcome| outcome.dispatch.take())
            .expect("one canonical provider dispatch");
        service
            .dispatch(dispatch, Box::pin(std::future::pending()))
            .await
            .expect("known provider failure is durable");
        assert_eq!(stream_calls.load(AtomicOrdering::SeqCst), 1);
        let state = store.state.lock().await;
        assert_eq!(state.conversation_fork_deliveries.len(), 1);
        assert_eq!(state.conversation_fork_commands.len(), 2);
        assert_eq!(state.conversation_turns.len(), 2);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn conversation_edit_and_regenerate_use_exact_recorded_model_and_context() {
        let (store, source) = completed_conversation_store_fixture("fork-dispatch").await;
        let list_calls = Arc::new(AtomicUsize::new(0));
        let stream_calls = Arc::new(AtomicUsize::new(0));
        let model = Arc::new(RetryableFailureModel {
            list_calls: list_calls.clone(),
            stream_calls: stream_calls.clone(),
            requests: StdMutex::new(Vec::new()),
        });
        let service = conversation_fork_service(
            store.clone(),
            model.clone(),
            Arc::new(FixedClock::new(20)),
            Some(TEST_CREDENTIAL_BINDING),
        );
        let parent_before = store
            .list_messages(&source.turn.thread_id, None, 100)
            .await
            .expect("parent history");

        let edited = service
            .edit_and_branch(
                EditAndBranchConversationTurn {
                    source_turn_id: source.turn.id.to_string(),
                    expected_revision: source.turn.revision,
                    content: "Edited request".into(),
                },
                "edit-command",
                Box::pin(std::future::pending()),
            )
            .await
            .expect("edit and branch");
        assert_eq!(
            edited
                .snapshot
                .started_turn
                .as_ref()
                .expect("edited turn")
                .turn
                .model_id,
            source.turn.model_id
        );
        service
            .dispatch(
                edited.dispatch.expect("edited dispatch"),
                Box::pin(std::future::pending()),
            )
            .await
            .expect("known edited failure is durable");

        let regenerated = service
            .regenerate(
                RegenerateConversationTurn {
                    source_turn_id: source.turn.id.to_string(),
                    expected_revision: source.turn.revision,
                },
                "regenerate-command",
                Box::pin(std::future::pending()),
            )
            .await
            .expect("regenerate");
        assert_eq!(
            regenerated
                .snapshot
                .started_turn
                .as_ref()
                .expect("regenerated turn")
                .turn
                .model_id,
            source.turn.model_id
        );
        service
            .dispatch(
                regenerated.dispatch.expect("regenerate dispatch"),
                Box::pin(std::future::pending()),
            )
            .await
            .expect("known regenerate failure is durable");

        assert_eq!(list_calls.load(AtomicOrdering::SeqCst), 2);
        assert_eq!(stream_calls.load(AtomicOrdering::SeqCst), 2);
        {
            let requests = model.requests.lock().expect("request lock");
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[0].model, source.turn.model_id);
            assert_eq!(requests[1].model, source.turn.model_id);
            assert_eq!(requests[0].continuation, None);
            assert_eq!(requests[1].continuation, None);
            assert!(!requests[0].store && !requests[1].store);
            assert_eq!(requests[0].messages.len(), 1);
            assert_eq!(requests[1].messages.len(), 1);
            assert_eq!(requests[0].messages[0].role, ConversationRole::User);
            assert_eq!(requests[1].messages[0].role, ConversationRole::User);
            assert_eq!(
                requests[0].messages[0].content,
                vec![ContentPart::Text("Edited request".into())]
            );
            assert_eq!(
                requests[1].messages[0].content,
                vec![ContentPart::Text(source.user_message.content.clone())]
            );
        }
        assert_eq!(
            store
                .list_messages(&source.turn.thread_id, None, 100)
                .await
                .expect("unchanged parent"),
            parent_before
        );
        let replay = service
            .replay_regenerate(
                &RegenerateConversationTurn {
                    source_turn_id: source.turn.id.to_string(),
                    expected_revision: source.turn.revision,
                },
                "regenerate-command",
            )
            .await
            .expect("replay query")
            .expect("replayed fork");
        assert_eq!(
            replay.snapshot.child_thread.id,
            regenerated.snapshot.child_thread.id
        );
        assert!(!replay.reconciled_pending_delivery);
        assert_eq!(list_calls.load(AtomicOrdering::SeqCst), 2);
    }

    #[tokio::test]
    async fn conversation_fork_invalid_state_or_binding_never_calls_provider() {
        let (reserved_store, reserved) = conversation_store_fixture("fork-ineligible").await;
        let list_calls = Arc::new(AtomicUsize::new(0));
        let model = Arc::new(RetryableFailureModel {
            list_calls: list_calls.clone(),
            stream_calls: Arc::new(AtomicUsize::new(0)),
            requests: StdMutex::new(Vec::new()),
        });
        let service = conversation_fork_service(
            reserved_store,
            model.clone(),
            Arc::new(FixedClock::new(20)),
            Some(TEST_CREDENTIAL_BINDING),
        );
        assert!(matches!(
            service
                .edit_and_branch(
                    EditAndBranchConversationTurn {
                        source_turn_id: reserved.turn.id.to_string(),
                        expected_revision: reserved.turn.revision,
                        content: "Edited request".into(),
                    },
                    "ineligible-edit",
                    Box::pin(std::future::pending()),
                )
                .await,
            Err(ApplicationError::InvalidState(_))
        ));
        assert_eq!(list_calls.load(AtomicOrdering::SeqCst), 0);

        let (invalid_store, completed_for_invalid_edit) =
            completed_conversation_store_fixture("fork-invalid-edit").await;
        let invalid_edit_service = conversation_fork_service(
            invalid_store,
            model.clone(),
            Arc::new(FixedClock::new(20)),
            Some(TEST_CREDENTIAL_BINDING),
        );
        for (index, content) in [
            completed_for_invalid_edit.user_message.content.clone(),
            " \t ".into(),
            "unsupported\u{7f}control".into(),
        ]
        .into_iter()
        .enumerate()
        {
            assert!(matches!(
                invalid_edit_service
                    .edit_and_branch(
                        EditAndBranchConversationTurn {
                            source_turn_id: completed_for_invalid_edit.turn.id.to_string(),
                            expected_revision: completed_for_invalid_edit.turn.revision,
                            content,
                        },
                        &format!("invalid-edit-{index}"),
                        Box::pin(std::future::pending()),
                    )
                    .await,
                Err(ApplicationError::InvalidInput(_))
            ));
        }
        assert_eq!(list_calls.load(AtomicOrdering::SeqCst), 0);

        let (completed_store, completed) =
            completed_conversation_store_fixture("fork-wrong-binding").await;
        let wrong_binding_service = conversation_fork_service(
            completed_store,
            model,
            Arc::new(FixedClock::new(20)),
            Some(OTHER_TEST_CREDENTIAL_BINDING),
        );
        assert!(matches!(
            wrong_binding_service
                .regenerate(
                    RegenerateConversationTurn {
                        source_turn_id: completed.turn.id.to_string(),
                        expected_revision: completed.turn.revision,
                    },
                    "wrong-binding-regenerate",
                    Box::pin(std::future::pending()),
                )
                .await,
            Err(ApplicationError::InvalidState(_))
        ));
        assert_eq!(list_calls.load(AtomicOrdering::SeqCst), 0);
    }

    #[tokio::test]
    async fn conversation_fork_store_rejects_forgery_and_key_conflict_without_partial_state() {
        let (store, source) = completed_conversation_store_fixture("fork-atomic").await;
        let valid = {
            let state = store.state.lock().await;
            canonical_fork_plan(&state, &source, ConversationForkKind::Branch, "atomic", 20)
        };
        let mut forged = valid.clone();
        forged.messages[0].content = "forged source context".into();
        let before = {
            let state = store.state.lock().await;
            (
                state.threads.len(),
                state.messages.len(),
                state.runs.len(),
                state.conversation_turns.len(),
                state.conversation_contexts.len(),
                state.conversation_events.len(),
                state.conversation_fork_commands.len(),
                state.conversation_inherited_outcomes.len(),
            )
        };
        assert!(matches!(
            store.reserve_conversation_fork(forged).await,
            Err(StoreError::Conflict)
        ));
        let after = {
            let state = store.state.lock().await;
            (
                state.threads.len(),
                state.messages.len(),
                state.runs.len(),
                state.conversation_turns.len(),
                state.conversation_contexts.len(),
                state.conversation_events.len(),
                state.conversation_fork_commands.len(),
                state.conversation_inherited_outcomes.len(),
            )
        };
        assert_eq!(after, before);

        let first = store
            .reserve_conversation_fork(valid.clone())
            .await
            .expect("valid branch");
        assert!(first.created);
        let replay = store
            .reserve_conversation_fork(valid.clone())
            .await
            .expect("exact replay");
        assert!(!replay.created);
        assert_eq!(replay.snapshot, first.snapshot);
        let mut conflicting = valid.command;
        conflicting.fingerprint[0] ^= 0xff;
        assert!(matches!(
            store.load_conversation_fork_by_command(&conflicting).await,
            Err(StoreError::Conflict)
        ));
    }

    #[tokio::test]
    async fn conversation_fork_pending_alias_bound_fails_closed_without_poisoning_old_keys() {
        let (store, source) = completed_conversation_store_fixture("fork-alias-bound").await;
        let plan = {
            let state = store.state.lock().await;
            canonical_fork_plan(
                &state,
                &source,
                ConversationForkKind::Branch,
                "alias-bound",
                20,
            )
        };
        let created = store
            .reserve_conversation_fork(plan.clone())
            .await
            .expect("canonical pending fork");
        let mut aliases = Vec::new();
        for index in 0..MAX_CONVERSATION_FORK_DELIVERY_ALIASES {
            let mut alias = plan.command.clone();
            alias.key = format!("alias-bound-{index}");
            let resolution = store
                .resolve_conversation_fork_command(&alias)
                .await
                .expect("bounded alias")
                .expect("pending child");
            assert!(resolution.reconciled_pending_delivery);
            assert_eq!(
                resolution.snapshot.child_thread.id,
                created.snapshot.child_thread.id
            );
            aliases.push(alias);
        }
        let before = store.state.lock().await.conversation_fork_commands.len();
        let mut overflow = plan.command.clone();
        overflow.key = "alias-bound-overflow".into();
        assert!(matches!(
            store.resolve_conversation_fork_command(&overflow).await,
            Err(StoreError::Conflict)
        ));
        assert_eq!(
            store.state.lock().await.conversation_fork_commands.len(),
            before
        );

        let exact_alias = store
            .resolve_conversation_fork_command(&aliases[0])
            .await
            .expect("old alias remains valid")
            .expect("old alias replay");
        assert!(!exact_alias.reconciled_pending_delivery);
        let acknowledged = store
            .acknowledge_conversation_fork_delivery(
                MutationCommand {
                    scope: "acknowledge_conversation_fork_delivery".into(),
                    key: "alias-bound-ack".into(),
                    fingerprint: [211; 32],
                },
                created.snapshot.child_thread.id.clone(),
                0,
            )
            .await
            .expect("ack after alias bound rejection");
        assert_eq!(
            acknowledged.state,
            ConversationForkDeliveryState::Acknowledged
        );
        assert_eq!(acknowledged.revision, 1);
        assert_eq!(
            store
                .load_conversation_fork_by_command(&aliases[0])
                .await
                .expect("exact alias after ack")
                .expect("old child")
                .child_thread
                .id,
            created.snapshot.child_thread.id
        );
    }

    #[tokio::test]
    async fn conversation_fork_delivery_corruption_fails_before_ack_mutation() {
        let (store, source) = completed_conversation_store_fixture("fork-delivery-corrupt").await;
        let plan = {
            let state = store.state.lock().await;
            canonical_fork_plan(
                &state,
                &source,
                ConversationForkKind::Branch,
                "delivery-corrupt",
                20,
            )
        };
        let canonical_command = plan.command.clone();
        let created = store
            .reserve_conversation_fork(plan)
            .await
            .expect("canonical pending fork");
        let child_id = created.snapshot.child_thread.id;
        {
            let mut state = store.state.lock().await;
            let delivery = state
                .conversation_fork_deliveries
                .get_mut(&child_id)
                .expect("delivery");
            delivery.revision = 1;
            assert_eq!(delivery.state, ConversationForkDeliveryState::Pending);
        }
        let ack = MutationCommand {
            scope: "acknowledge_conversation_fork_delivery".into(),
            key: "delivery-corrupt-ack".into(),
            fingerprint: [212; 32],
        };
        assert!(matches!(
            store
                .acknowledge_conversation_fork_delivery(ack, child_id.clone(), 1)
                .await,
            Err(StoreError::Internal(_))
        ));
        let state = store.state.lock().await;
        let delivery = state
            .conversation_fork_deliveries
            .get(&child_id)
            .expect("unchanged corrupt delivery");
        assert_eq!(delivery.state, ConversationForkDeliveryState::Pending);
        assert_eq!(delivery.revision, 1);
        assert!(state.conversation_fork_delivery_ack_commands.is_empty());
        drop(state);

        let corrupted_fingerprint = {
            let mut state = store.state.lock().await;
            let delivery = state
                .conversation_fork_deliveries
                .get_mut(&child_id)
                .expect("delivery");
            delivery.revision = 0;
            delivery.request_fingerprint[0] ^= 0xff;
            delivery.request_fingerprint
        };
        assert!(matches!(
            store
                .load_conversation_fork_by_command(&canonical_command)
                .await,
            Err(StoreError::Internal(_))
        ));
        assert!(matches!(
            store
                .acknowledge_conversation_fork_delivery(
                    MutationCommand {
                        scope: "acknowledge_conversation_fork_delivery".into(),
                        key: "delivery-corrupt-correlation-ack".into(),
                        fingerprint: [213; 32],
                    },
                    child_id.clone(),
                    0,
                )
                .await,
            Err(StoreError::Internal(_))
        ));
        let state = store.state.lock().await;
        let delivery = state
            .conversation_fork_deliveries
            .get(&child_id)
            .expect("unchanged corrupt delivery correlation");
        assert_eq!(delivery.state, ConversationForkDeliveryState::Pending);
        assert_eq!(delivery.revision, 0);
        assert_eq!(delivery.request_fingerprint, corrupted_fingerprint);
        assert!(state.conversation_fork_delivery_ack_commands.is_empty());
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn conversation_fork_store_resolves_nested_inherited_assistant_outcomes() {
        let (store, root_source) = completed_conversation_store_fixture("fork-nested").await;
        let first_plan = {
            let state = store.state.lock().await;
            canonical_fork_plan(
                &state,
                &root_source,
                ConversationForkKind::Branch,
                "nested-first",
                20,
            )
        };
        let first = store
            .reserve_conversation_fork(first_plan)
            .await
            .expect("first branch");
        let child = first.snapshot.child_thread;
        let (turn, user, run, event) = canonical_reservation_inputs(
            root_source.turn.project_id.clone(),
            child.id.clone(),
            "nested-follow-up",
            30,
            "Follow-up request",
        );
        let reserved = store
            .reserve_turn(
                turn,
                original_test_lineage(),
                ConversationTurnReservationSource::CurrentThread,
                user,
                run,
                event,
                ConversationTurnEventKind::Created,
            )
            .await
            .expect("child follow-up reservation")
            .snapshot;
        let started = store
            .commit_provider_start(canonical_provider_start(&reserved, "nested-follow-up"))
            .await
            .expect("follow-up provider start");
        store
            .append_turn_text(
                &started.turn.id,
                started.turn.revision,
                0,
                "Canonical answer".into(),
            )
            .await
            .expect("follow-up text");
        let completed = store
            .commit_terminal(canonical_completed_terminal(&started, "nested-follow-up"))
            .await
            .expect("completed follow-up");
        let second_plan = {
            let state = store.state.lock().await;
            canonical_fork_plan(
                &state,
                &completed,
                ConversationForkKind::Branch,
                "nested-second",
                40,
            )
        };
        let second = store
            .reserve_conversation_fork(second_plan)
            .await
            .expect("nested branch");
        assert_eq!(second.snapshot.child_thread.lineage.fork_depth, 2);
        let metadata = store
            .load_conversation_fork_metadata(&second.snapshot.child_thread.id)
            .await
            .expect("nested metadata");
        assert_eq!(metadata.family_threads.len(), 3);
        assert_eq!(metadata.inherited_assistant_outcomes.len(), 2);
        assert_eq!(
            metadata
                .inherited_assistant_outcomes
                .iter()
                .map(|outcome| outcome.source_turn_id.clone())
                .collect::<HashSet<_>>(),
            HashSet::from([root_source.turn.id.clone(), completed.turn.id.clone()])
        );
        let root_copy = metadata
            .inherited_assistant_outcomes
            .iter()
            .find(|outcome| outcome.source_turn_id == root_source.turn.id)
            .expect("root assistant copy")
            .child_assistant_message_id
            .clone();
        store
            .state
            .lock()
            .await
            .conversation_inherited_outcomes
            .insert(root_copy, completed.turn.id.clone());
        assert!(matches!(
            store
                .load_conversation_fork_metadata(&second.snapshot.child_thread.id)
                .await,
            Err(StoreError::Internal(_))
        ));
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn conversation_fork_store_enforces_direct_and_family_bounds() {
        let (direct_store, direct_source) =
            completed_conversation_store_fixture("fork-direct-bound").await;
        let direct_plan = {
            let mut state = direct_store.state.lock().await;
            let parent = state
                .threads
                .get(&direct_source.turn.thread_id)
                .cloned()
                .expect("parent");
            let source_message = direct_source.assistant_message.as_ref().expect("assistant");
            for index in 0..MAX_CONVERSATION_FORK_DIRECT_CHILDREN {
                let child = Thread::new_fork(
                    ThreadId::new(format!("direct-bound-child-{index}")).expect("child ID"),
                    parent.project_id.clone(),
                    parent.title.clone(),
                    parent.id.clone(),
                    &parent.lineage,
                    direct_source.turn.id.clone(),
                    source_message.id.clone(),
                    MessageRole::Assistant,
                    ConversationForkKind::Branch,
                    20,
                )
                .expect("bounded child");
                state.threads.insert(child.id.clone(), child);
            }
            canonical_fork_plan(
                &state,
                &direct_source,
                ConversationForkKind::Branch,
                "direct-overflow",
                30,
            )
        };
        assert!(matches!(
            direct_store
                .reserve_conversation_fork(direct_plan.clone())
                .await,
            Err(StoreError::Conflict)
        ));
        assert!(
            direct_store
                .get_thread(&direct_plan.child_thread.id)
                .await
                .is_err()
        );

        let (family_store, family_source) =
            completed_conversation_store_fixture("fork-family-bound").await;
        let family_plan = {
            let mut state = family_store.state.lock().await;
            let root = state
                .threads
                .get(&family_source.turn.thread_id)
                .cloned()
                .expect("root");
            let source_message = family_source.assistant_message.as_ref().expect("assistant");
            for index in 0..(MAX_CONVERSATION_FORK_FAMILY_THREADS - 1) {
                let id =
                    ThreadId::new(format!("family-bound-child-{index}")).expect("family child ID");
                let thread = Thread {
                    id: id.clone(),
                    project_id: root.project_id.clone(),
                    title: root.title.clone(),
                    state: ThreadState::Open,
                    lineage: grok_domain::ConversationThreadLineage {
                        root_thread_id: root.id.clone(),
                        origin: ConversationThreadOrigin::Fork {
                            parent_thread_id: ThreadId::new(format!("family-parent-{index}"))
                                .expect("synthetic parent ID"),
                            source_turn_id: family_source.turn.id.clone(),
                            source_message_id: source_message.id.clone(),
                            kind: ConversationForkKind::Branch,
                        },
                        fork_depth: 2,
                    },
                    revision: 0,
                    created_at: 20,
                    updated_at: 20,
                };
                Thread::restore(thread.clone()).expect("self-contained family member");
                state.threads.insert(id, thread);
            }
            canonical_fork_plan(
                &state,
                &family_source,
                ConversationForkKind::Branch,
                "family-overflow",
                30,
            )
        };
        assert!(matches!(
            family_store
                .reserve_conversation_fork(family_plan.clone())
                .await,
            Err(StoreError::Conflict)
        ));
        assert!(
            family_store
                .get_thread(&family_plan.child_thread.id)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn conversation_fork_metadata_outcome_bound_rejects_before_child_commit() {
        let (store, mut source) = completed_conversation_store_fixture("fork-metadata-bound").await;
        for index in 0..MAX_CONVERSATION_FORK_INHERITED_OUTCOMES {
            let prefix = format!("fork-metadata-history-{index}");
            let (turn, user, run, event) = canonical_reservation_inputs(
                source.turn.project_id.clone(),
                source.turn.thread_id.clone(),
                &prefix,
                20 + u64::try_from(index).expect("test index") * 3,
                "Bounded request",
            );
            let reserved = store
                .reserve_turn(
                    turn,
                    original_test_lineage(),
                    ConversationTurnReservationSource::CurrentThread,
                    user,
                    run,
                    event,
                    ConversationTurnEventKind::Created,
                )
                .await
                .expect("history reservation")
                .snapshot;
            let context = store
                .load_turn_context(&reserved.turn.id)
                .await
                .expect("history context");
            let mut provider_start = canonical_provider_start(&reserved, &prefix);
            provider_start.turn.provider_request_fingerprint = Some(
                test_provider_request_fingerprint(&reserved.turn.model_id, &context),
            );
            let started = store
                .commit_provider_start(provider_start)
                .await
                .expect("history provider start");
            store
                .append_turn_text(
                    &started.turn.id,
                    started.turn.revision,
                    0,
                    "Canonical answer".into(),
                )
                .await
                .expect("history assistant text");
            source = store
                .commit_terminal(canonical_completed_terminal(&started, &prefix))
                .await
                .expect("history completion");
        }
        let plan = {
            let state = store.state.lock().await;
            canonical_fork_plan(
                &state,
                &source,
                ConversationForkKind::Branch,
                "metadata-overflow",
                source.turn.updated_at + 1,
            )
        };
        let before = {
            let state = store.state.lock().await;
            (
                state.threads.len(),
                state.messages.len(),
                state.conversation_fork_commands.len(),
                state.conversation_inherited_outcomes.len(),
            )
        };
        assert!(matches!(
            store.reserve_conversation_fork(plan.clone()).await,
            Err(StoreError::Conflict)
        ));
        let state = store.state.lock().await;
        assert_eq!(
            (
                state.threads.len(),
                state.messages.len(),
                state.conversation_fork_commands.len(),
                state.conversation_inherited_outcomes.len(),
            ),
            before
        );
        assert!(!state.threads.contains_key(&plan.child_thread.id));
    }

    fn canonical_reservation_inputs(
        project_id: ProjectId,
        thread_id: ThreadId,
        prefix: &str,
        now: UnixMillis,
        content: &str,
    ) -> (ConversationTurn, Message, Run, NewRunEvent) {
        let user = Message::new(
            MessageId::new(format!("{prefix}-user")).expect("message ID"),
            thread_id.clone(),
            MessageRole::User,
            content.into(),
            now,
        )
        .expect("user message");
        let run = Run::queued(
            RunId::new(format!("{prefix}-run")).expect("run ID"),
            project_id.clone(),
            thread_id.clone(),
            now,
        );
        let turn = ConversationTurn::reserve(
            ConversationTurnId::new(format!("{prefix}-turn")).expect("turn ID"),
            format!("{prefix}-command"),
            [7; 32],
            project_id,
            thread_id,
            user.id.clone(),
            run.id.clone(),
            "grok-4.3".into(),
            now,
        )
        .expect("turn");
        (
            turn,
            user,
            run,
            NewRunEvent {
                occurred_at: now,
                kind: RunEventKind::Created,
            },
        )
    }

    fn canonical_provider_start(
        snapshot: &ConversationTurnSnapshot,
        prefix: &str,
    ) -> ProviderStartCommit {
        let transition_at = snapshot.turn.updated_at + 1;
        let mut turn = snapshot.turn.clone();
        let mut run = snapshot.run.clone();
        run.transition(RunState::Planning, transition_at)
            .expect("planning");
        run.transition(RunState::Running, transition_at)
            .expect("running");
        let mut effect = SideEffect::prepare(
            EffectId::new(format!("{prefix}-effect")).expect("effect ID"),
            run.id.clone(),
            EffectKind::ExternalMutation,
            format!("official xAI Responses API model {}", turn.model_id),
            Idempotency::NonIdempotent,
            transition_at,
        );
        effect.start(transition_at).expect("effect start");
        turn.start_provider(effect.id.clone(), [8; 32], transition_at)
            .expect("provider start");
        ProviderStartCommit {
            turn,
            expected_turn_revision: snapshot.turn.revision,
            run,
            expected_run_revision: snapshot.run.revision,
            events: vec![
                NewRunEvent {
                    occurred_at: transition_at,
                    kind: RunEventKind::StateChanged {
                        from: RunState::Queued,
                        to: RunState::Planning,
                    },
                },
                NewRunEvent {
                    occurred_at: transition_at,
                    kind: RunEventKind::StateChanged {
                        from: RunState::Planning,
                        to: RunState::Running,
                    },
                },
                NewRunEvent {
                    occurred_at: transition_at,
                    kind: RunEventKind::EffectPrepared {
                        effect_id: effect.id.clone(),
                    },
                },
            ],
            effect,
            turn_event: ConversationTurnEventKind::StateChanged {
                from: ConversationTurnState::Reserved,
                to: ConversationTurnState::ProviderStarted,
            },
        }
    }

    fn canonical_completed_terminal(
        snapshot: &ConversationTurnSnapshot,
        prefix: &str,
    ) -> TerminalTurnCommit {
        let transition_at = snapshot.turn.updated_at + 1;
        let mut turn = snapshot.turn.clone();
        let mut run = snapshot.run.clone();
        let mut effect = snapshot.effect.clone().expect("provider effect");
        let assistant = Message::new(
            MessageId::new(format!("{prefix}-assistant")).expect("assistant ID"),
            turn.thread_id.clone(),
            MessageRole::Assistant,
            "Canonical answer".into(),
            transition_at,
        )
        .expect("assistant");
        turn.complete(
            assistant.id.clone(),
            Some(format!("{prefix}-response")),
            Vec::new(),
            grok_domain::ConversationUsage::default(),
            Some(true),
            transition_at,
        )
        .expect("complete turn");
        run.transition(RunState::Completed, transition_at)
            .expect("complete run");
        effect.finish(true, transition_at).expect("finish effect");
        TerminalTurnCommit {
            turn,
            expected_turn_revision: snapshot.turn.revision,
            run,
            expected_run_revision: snapshot.run.revision,
            effect: Some(effect),
            expected_effect_revision: snapshot.effect.as_ref().map(|value| value.revision),
            assistant_message: Some(assistant),
            events: vec![NewRunEvent {
                occurred_at: transition_at,
                kind: RunEventKind::StateChanged {
                    from: RunState::Running,
                    to: RunState::Completed,
                },
            }],
            turn_event: ConversationTurnEventKind::StateChanged {
                from: ConversationTurnState::ProviderStarted,
                to: ConversationTurnState::Completed,
            },
        }
    }

    async fn load_conversation_snapshot(
        store: &InMemoryExecutionStore,
        snapshot: &ConversationTurnSnapshot,
    ) -> ConversationTurnSnapshot {
        store
            .load_turn_by_command(&MutationCommand {
                scope: "execute_conversation_turn".into(),
                key: snapshot.turn.idempotency_key.clone(),
                fingerprint: snapshot.turn.request_fingerprint,
            })
            .await
            .expect("load command")
            .expect("stored conversation")
    }

    #[tokio::test]
    async fn conversation_store_rejects_noncanonical_reservation_aggregates() {
        let store = Arc::new(InMemoryExecutionStore::new());
        let workspace = WorkspaceService::new(
            store.clone(),
            Arc::new(FixedClock::new(10)),
            Arc::new(SequentialIdGenerator::new()),
        );
        let project = workspace
            .create_project(
                CreateProject {
                    name: "Reservation validation".into(),
                    description: String::new(),
                },
                "reservation-validation-project",
            )
            .await
            .expect("project");
        let thread = workspace
            .create_thread(
                CreateThread {
                    project_id: project.id.to_string(),
                    title: "Reservation validation".into(),
                },
                "reservation-validation-thread",
            )
            .await
            .expect("thread");
        let canonical = canonical_reservation_inputs(
            project.id,
            thread.id,
            "reservation-validation",
            10,
            "User request",
        );

        for forgery in [
            "turn_state",
            "turn_metadata",
            "user_sequence",
            "user_revision",
            "run_state",
            "run_project",
            "event",
            "turn_event",
        ] {
            let (mut turn, mut user, mut run, mut event) = canonical.clone();
            match forgery {
                "turn_state" => turn.state = ConversationTurnState::Cancelled,
                "turn_metadata" => turn.model_id = "grok\0forged".into(),
                "user_sequence" => user.sequence = 99,
                "user_revision" => user.revision = 1,
                "run_state" => run.state = RunState::Planning,
                "run_project" => {
                    run.project_id = ProjectId::new("forged-project").expect("project ID");
                }
                "event" => event.occurred_at += 1,
                "turn_event" => {}
                _ => unreachable!(),
            }
            let turn_event = if forgery == "turn_event" {
                ConversationTurnEventKind::StateChanged {
                    from: ConversationTurnState::Reserved,
                    to: ConversationTurnState::Cancelled,
                }
            } else {
                ConversationTurnEventKind::Created
            };
            assert!(matches!(
                store
                    .reserve_turn(
                        turn,
                        original_test_lineage(),
                        ConversationTurnReservationSource::CurrentThread,
                        user,
                        run,
                        event,
                        turn_event,
                    )
                    .await,
                Err(StoreError::Conflict)
            ));
        }

        let reservation = store
            .reserve_turn(
                canonical.0,
                original_test_lineage(),
                ConversationTurnReservationSource::CurrentThread,
                canonical.1,
                canonical.2,
                canonical.3,
                ConversationTurnEventKind::Created,
            )
            .await
            .expect("canonical reservation");
        assert_eq!(reservation.snapshot.user_message.sequence, 1);
        assert_eq!(
            load_conversation_snapshot(&store, &reservation.snapshot).await,
            reservation.snapshot
        );
    }

    #[tokio::test]
    async fn conversation_store_rejects_forged_provider_start_aggregates_atomically() {
        for forgery in [
            "turn_identity",
            "turn_state",
            "run_link",
            "run_state",
            "effect_state",
            "effect_target",
            "effect_policy",
            "events",
            "turn_event",
        ] {
            let prefix = format!("provider-forgery-{forgery}");
            let (store, reserved) = conversation_store_fixture(&prefix).await;
            let canonical = canonical_provider_start(&reserved, &prefix);
            let mut forged = canonical.clone();
            match forgery {
                "turn_identity" => forged.turn.request_fingerprint = [99; 32],
                "turn_state" => forged.turn.state = ConversationTurnState::Completed,
                "run_link" => {
                    forged.run.thread_id = ThreadId::new("forged-thread").expect("thread ID");
                }
                "run_state" => forged.run.state = RunState::Planning,
                "effect_state" => forged.effect.state = EffectState::Prepared,
                "effect_target" => forged.effect.target = "forged provider target".into(),
                "effect_policy" => forged.effect.idempotency = Idempotency::Idempotent,
                "events" => forged.events[0].occurred_at += 1,
                "turn_event" => {
                    forged.turn_event = ConversationTurnEventKind::StateChanged {
                        from: ConversationTurnState::Reserved,
                        to: ConversationTurnState::Cancelled,
                    };
                }
                _ => unreachable!(),
            }
            assert!(matches!(
                store.commit_provider_start(forged).await,
                Err(StoreError::Conflict)
            ));
            assert_eq!(
                load_conversation_snapshot(&store, &reserved).await,
                reserved
            );

            let committed = store
                .commit_provider_start(canonical)
                .await
                .expect("canonical provider start");
            assert_eq!(
                load_conversation_snapshot(&store, &committed).await,
                committed
            );
        }
    }

    #[tokio::test]
    async fn conversation_store_rejects_forged_terminal_aggregates_atomically() {
        for forgery in [
            "turn_identity",
            "turn_user_link",
            "run_link",
            "run_state",
            "effect_kind",
            "effect_state",
            "assistant_content",
            "assistant_revision",
            "assistant_sequence",
            "assistant_timestamp",
            "events",
            "turn_event",
        ] {
            let prefix = format!("terminal-forgery-{forgery}");
            let (store, reserved) = conversation_store_fixture(&prefix).await;
            let started = store
                .commit_provider_start(canonical_provider_start(&reserved, &prefix))
                .await
                .expect("provider start");
            let canonical = canonical_completed_terminal(&started, &prefix);
            store
                .append_turn_text(
                    &started.turn.id,
                    started.turn.revision,
                    0,
                    "Canonical answer".into(),
                )
                .await
                .expect("durable assistant text");
            let mut forged = canonical.clone();
            match forgery {
                "turn_identity" => forged.turn.model_id = "grok-forged".into(),
                "turn_user_link" => {
                    forged.turn.user_message_id =
                        MessageId::new("forged-user").expect("message ID");
                }
                "run_link" => {
                    forged.run.project_id = ProjectId::new("forged-project").expect("project ID");
                }
                "run_state" => forged.run.state = RunState::Failed,
                "effect_kind" => {
                    forged.effect.as_mut().expect("effect").kind = EffectKind::FileWrite;
                }
                "effect_state" => {
                    forged.effect.as_mut().expect("effect").state = EffectState::Executing;
                }
                "assistant_content" => {
                    forged
                        .assistant_message
                        .as_mut()
                        .expect("assistant")
                        .content = "forged\0content".into();
                }
                "assistant_revision" => {
                    forged
                        .assistant_message
                        .as_mut()
                        .expect("assistant")
                        .revision = 1;
                }
                "assistant_sequence" => {
                    forged
                        .assistant_message
                        .as_mut()
                        .expect("assistant")
                        .sequence = 99;
                }
                "assistant_timestamp" => {
                    forged
                        .assistant_message
                        .as_mut()
                        .expect("assistant")
                        .updated_at += 1;
                }
                "events" => forged.events.clear(),
                "turn_event" => {
                    forged.turn_event = ConversationTurnEventKind::StateChanged {
                        from: ConversationTurnState::ProviderStarted,
                        to: ConversationTurnState::Failed,
                    };
                }
                _ => unreachable!(),
            }
            assert!(matches!(
                store.commit_terminal(forged).await,
                Err(StoreError::Conflict)
            ));
            assert_eq!(load_conversation_snapshot(&store, &started).await, started);

            let committed = store
                .commit_terminal(canonical)
                .await
                .expect("canonical terminal commit");
            assert_eq!(
                load_conversation_snapshot(&store, &committed).await,
                committed
            );
            assert_eq!(committed.assistant_message.expect("assistant").sequence, 2);
        }
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn conversation_cancellation_accepts_only_exact_edges_and_binds_its_outcome() {
        let (started_store, reserved) = conversation_store_fixture("cancel-forged-edge").await;
        let started = started_store
            .commit_provider_start(canonical_provider_start(&reserved, "cancel-forged-edge"))
            .await
            .expect("provider start");
        started_store
            .append_turn_text(
                &started.turn.id,
                started.turn.revision,
                0,
                "Canonical answer".into(),
            )
            .await
            .expect("durable assistant text");
        let forged_completion = canonical_completed_terminal(&started, "cancel-forged-edge");
        assert!(matches!(
            started_store
                .commit_cancellation(CancelConversationTurnCommit {
                    command: MutationCommand {
                        scope: "cancel_conversation_turn".into(),
                        key: "cancel-forged-edge-command".into(),
                        fingerprint: [71; 32],
                    },
                    turn_id: started.turn.id.clone(),
                    expected_turn_revision: started.turn.revision,
                    terminal: Some(forged_completion),
                })
                .await,
            Err(StoreError::Conflict)
        ));
        assert_eq!(
            load_conversation_snapshot(&started_store, &started).await,
            started
        );

        let (reserved_store, reserved) = conversation_store_fixture("cancel-bound-outcome").await;
        let transition_at = reserved.turn.updated_at + 1;
        let mut cancelled_turn = reserved.turn.clone();
        let mut cancelled_run = reserved.run.clone();
        cancelled_turn.cancel(transition_at).expect("cancel turn");
        cancelled_run
            .transition(RunState::Cancelled, transition_at)
            .expect("cancel run");
        let cancellation = CancelConversationTurnCommit {
            command: MutationCommand {
                scope: "cancel_conversation_turn".into(),
                key: "cancel-bound-outcome-command".into(),
                fingerprint: [72; 32],
            },
            turn_id: reserved.turn.id.clone(),
            expected_turn_revision: reserved.turn.revision,
            terminal: Some(TerminalTurnCommit {
                turn: cancelled_turn,
                expected_turn_revision: reserved.turn.revision,
                run: cancelled_run,
                expected_run_revision: reserved.run.revision,
                effect: None,
                expected_effect_revision: None,
                assistant_message: None,
                events: vec![NewRunEvent {
                    occurred_at: transition_at,
                    kind: RunEventKind::StateChanged {
                        from: RunState::Queued,
                        to: RunState::Cancelled,
                    },
                }],
                turn_event: ConversationTurnEventKind::StateChanged {
                    from: ConversationTurnState::Reserved,
                    to: ConversationTurnState::Cancelled,
                },
            }),
        };
        let cancelled = reserved_store
            .commit_cancellation(cancellation.clone())
            .await
            .expect("exact cancellation");
        assert_eq!(cancelled.turn.state, ConversationTurnState::Cancelled);
        assert_eq!(
            reserved_store
                .commit_cancellation(cancellation.clone())
                .await
                .expect("exact cancellation replay"),
            cancelled
        );
        let mut internal = cancellation.clone();
        internal.command.scope = "reconcile_conversation_dispatch_exit".into();
        internal.command.fingerprint = [73; 32];
        assert_eq!(
            reserved_store
                .commit_dispatch_exit_reconciliation(internal.clone())
                .await
                .expect("same key in internal namespace"),
            cancelled
        );
        assert!(matches!(
            reserved_store.commit_cancellation(internal).await,
            Err(StoreError::Conflict)
        ));

        reserved_store
            .state
            .lock()
            .await
            .conversation_cancel_commands
            .get_mut(&(
                cancellation.command.scope.clone(),
                cancellation.command.key.clone(),
            ))
            .expect("cancel command record")
            .outcome_revision += 1;
        assert!(matches!(
            reserved_store.commit_cancellation(cancellation).await,
            Err(StoreError::Internal(_))
        ));
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn captured_context_excludes_noncompleted_prompts_and_retains_completed_turns() {
        let store = Arc::new(InMemoryExecutionStore::new());
        let workspace = WorkspaceService::new(
            store.clone(),
            Arc::new(FixedClock::new(1)),
            Arc::new(SequentialIdGenerator::new()),
        );
        let project = workspace
            .create_project(
                CreateProject {
                    name: "Context ownership".into(),
                    description: String::new(),
                },
                "context-ownership-project",
            )
            .await
            .expect("project");
        let thread = workspace
            .create_thread(
                CreateThread {
                    project_id: project.id.to_string(),
                    title: "Context ownership".into(),
                },
                "context-ownership-thread",
            )
            .await
            .expect("thread");

        let cancelled_id =
            reserve_cancelled_turn(&store, project.id.clone(), thread.id.clone(), 1).await;
        let cancelled = store
            .list_thread_turns(&thread.id, None, 1)
            .await
            .expect("cancelled turn")
            .remove(0);
        assert_eq!(cancelled.turn.id, cancelled_id);

        let completed_prefix = "context-completed";
        let completed_inputs = canonical_reservation_inputs(
            project.id.clone(),
            thread.id.clone(),
            completed_prefix,
            20,
            "Completed prompt",
        );
        let completed_reserved = store
            .reserve_turn(
                completed_inputs.0,
                original_test_lineage(),
                ConversationTurnReservationSource::CurrentThread,
                completed_inputs.1,
                completed_inputs.2,
                completed_inputs.3,
                ConversationTurnEventKind::Created,
            )
            .await
            .expect("completed reservation")
            .snapshot;
        assert_eq!(
            completed_reserved.clone().turn.idempotency_key,
            format!("{completed_prefix}-command")
        );
        assert_eq!(completed_reserved.user_message.sequence, 2);
        let completed_started = store
            .commit_provider_start(canonical_provider_start(
                &completed_reserved,
                completed_prefix,
            ))
            .await
            .expect("completed provider start");
        store
            .append_turn_text(
                &completed_started.turn.id,
                completed_started.turn.revision,
                0,
                "Canonical answer".into(),
            )
            .await
            .expect("durable assistant text");
        let completed = store
            .commit_terminal(canonical_completed_terminal(
                &completed_started,
                completed_prefix,
            ))
            .await
            .expect("completed terminal");

        let context_workspace = WorkspaceService::new(
            store.clone(),
            Arc::new(FixedClock::new(25)),
            Arc::new(SequentialIdGenerator::new()),
        );
        let standalone_context = context_workspace
            .create_message(
                CreateMessage {
                    thread_id: thread.id.to_string(),
                    role: MessageRole::User,
                    content: "Standalone billed context".into(),
                },
                "standalone-context-message",
            )
            .await
            .expect("standalone context message");

        let next_inputs =
            canonical_reservation_inputs(project.id, thread.id, "context-next", 30, "Next prompt");
        let next = store
            .reserve_turn(
                next_inputs.0,
                original_test_lineage(),
                ConversationTurnReservationSource::CurrentThread,
                next_inputs.1,
                next_inputs.2,
                next_inputs.3,
                ConversationTurnEventKind::Created,
            )
            .await
            .expect("next reservation");
        let captured_ids = next
            .context
            .iter()
            .map(|message| message.id.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            next.context
                .iter()
                .map(|message| message.sequence)
                .collect::<Vec<_>>(),
            vec![2, 3, 4, 5]
        );
        assert_eq!(
            captured_ids,
            vec![
                completed.user_message.id,
                completed.assistant_message.expect("assistant").id,
                standalone_context.id.clone(),
                next.snapshot.user_message.id,
            ]
        );
        assert!(!captured_ids.contains(&cancelled.user_message.id));
        assert_eq!(
            store
                .load_turn_context(&next.snapshot.turn.id)
                .await
                .expect("stored immutable context"),
            next.context
        );
        let editing = WorkspaceService::new(
            store.clone(),
            Arc::new(FixedClock::new(40)),
            Arc::new(SequentialIdGenerator::new()),
        );
        assert!(matches!(
            editing
                .update_message(
                    UpdateMessage {
                        id: standalone_context.id.to_string(),
                        expected_revision: 0,
                        content: "Rewritten billed context".into(),
                    },
                    "rewrite-standalone-context",
                )
                .await,
            Err(ApplicationError::Conflict)
        ));
        assert!(matches!(
            editing
                .delete_message(&standalone_context.id, 0, "delete-standalone-context",)
                .await,
            Err(ApplicationError::Conflict)
        ));
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn conversation_turn_history_is_chronological_paged_and_scope_isolated() {
        let store = Arc::new(InMemoryExecutionStore::new());
        let clock = Arc::new(FixedClock::new(1));
        let ids_source = Arc::new(SequentialIdGenerator::new());
        let workspace = Arc::new(WorkspaceService::new(
            store.clone(),
            clock.clone(),
            ids_source.clone(),
        ));
        let project = workspace
            .create_project(
                CreateProject {
                    name: "Conversation history".into(),
                    description: String::new(),
                },
                "history-project",
            )
            .await
            .expect("project");
        let thread = workspace
            .create_thread(
                CreateThread {
                    project_id: project.id.to_string(),
                    title: "History".into(),
                },
                "history-thread",
            )
            .await
            .expect("thread");

        let mut ids = Vec::new();
        for index in 1..=3_u8 {
            ids.push(
                reserve_cancelled_turn(&store, project.id.clone(), thread.id.clone(), index).await,
            );
        }

        let conversation = ConversationService::new(
            store.clone(),
            workspace.clone(),
            Arc::new(CredentialService::new(
                Arc::new(InMemorySecretVault::new()),
                store.clone(),
                Arc::new(AcceptXaiKey),
            )),
            Arc::new(CatalogFactory(Arc::new(CatalogModel {
                models: Vec::new(),
                calls: Arc::new(AtomicUsize::new(0)),
                stream_calls: Arc::new(AtomicUsize::new(0)),
            }))),
            clock,
            ids_source,
            store.clone(),
        );
        let first_materialized = conversation
            .list_for_thread(&thread.id, None, 200)
            .await
            .expect("bounded application page");
        assert_eq!(first_materialized.items.len(), 1);
        assert_eq!(
            first_materialized.next_cursor.as_deref(),
            Some(ids[0].as_str())
        );

        let editing = WorkspaceService::new(
            store.clone(),
            Arc::new(FixedClock::new(100)),
            Arc::new(SequentialIdGenerator::new()),
        );
        let linked_message_id = MessageId::new("history-message-1").expect("message id");
        assert!(matches!(
            editing
                .update_message(
                    UpdateMessage {
                        id: linked_message_id.to_string(),
                        expected_revision: 0,
                        content: "Rewritten history".into(),
                    },
                    "rewrite-linked-message",
                )
                .await,
            Err(ApplicationError::Conflict)
        ));
        assert!(matches!(
            editing
                .delete_message(&linked_message_id, 0, "delete-linked-message")
                .await,
            Err(ApplicationError::Conflict)
        ));

        let first = store
            .list_thread_turns(&thread.id, None, 2)
            .await
            .expect("first page");
        let second = store
            .list_thread_turns(&thread.id, Some(&first[1].turn.id), 2)
            .await
            .expect("second page");
        assert_eq!(
            first
                .iter()
                .chain(&second)
                .map(|snapshot| snapshot.turn.id.clone())
                .collect::<Vec<_>>(),
            ids
        );
        assert_eq!(first[0].user_message.sequence, 1);
        assert_eq!(first[1].user_message.sequence, 2);
        assert_eq!(second[0].user_message.sequence, 3);

        let wrong_scope = MutationCommand {
            scope: "delete_xai_api_key".into(),
            key: "history-command-1".into(),
            fingerprint: [1; 32],
        };
        assert!(
            store
                .load_turn_by_command(&wrong_scope)
                .await
                .expect("scope isolation")
                .is_none()
        );
    }

    async fn reserve_cancelled_turn(
        store: &Arc<InMemoryExecutionStore>,
        project_id: ProjectId,
        thread_id: ThreadId,
        index: u8,
    ) -> ConversationTurnId {
        let created_at = u64::from(index) * 10;
        let user = Message::new(
            MessageId::new(format!("history-message-{index}")).expect("message id"),
            thread_id.clone(),
            MessageRole::User,
            format!("Message {index}"),
            created_at,
        )
        .expect("message");
        let run = Run::queued(
            RunId::new(format!("history-run-{index}")).expect("run id"),
            project_id.clone(),
            thread_id.clone(),
            created_at,
        );
        let turn = ConversationTurn::reserve(
            ConversationTurnId::new(format!("history-turn-{index}")).expect("turn id"),
            format!("history-command-{index}"),
            [index; 32],
            project_id,
            thread_id,
            user.id.clone(),
            run.id.clone(),
            "grok-4.3".into(),
            created_at,
        )
        .expect("turn");
        let reservation = store
            .reserve_turn(
                turn,
                original_test_lineage(),
                ConversationTurnReservationSource::CurrentThread,
                user,
                run,
                NewRunEvent {
                    occurred_at: created_at,
                    kind: grok_domain::RunEventKind::Created,
                },
                ConversationTurnEventKind::Created,
            )
            .await
            .expect("reserve turn");
        let mut terminal_turn = reservation.snapshot.turn;
        let mut terminal_run = reservation.snapshot.run;
        terminal_turn.cancel(created_at + 1).expect("cancel turn");
        terminal_run
            .transition(RunState::Cancelled, created_at + 1)
            .expect("cancel run");
        let turn_id = terminal_turn.id.clone();
        store
            .commit_terminal(TerminalTurnCommit {
                turn: terminal_turn,
                expected_turn_revision: 0,
                run: terminal_run,
                expected_run_revision: 0,
                effect: None,
                expected_effect_revision: None,
                assistant_message: None,
                events: vec![NewRunEvent {
                    occurred_at: created_at + 1,
                    kind: grok_domain::RunEventKind::StateChanged {
                        from: RunState::Queued,
                        to: RunState::Cancelled,
                    },
                }],
                turn_event: ConversationTurnEventKind::StateChanged {
                    from: ConversationTurnState::Reserved,
                    to: ConversationTurnState::Cancelled,
                },
            })
            .await
            .expect("commit cancelled turn");
        turn_id
    }

    #[tokio::test]
    async fn conversation_events_are_contiguous_split_on_utf8_and_exactly_replayable() {
        let (store, reserved) = conversation_store_fixture("event-stream").await;
        let created = store
            .list_turn_events_since(&reserved.turn.id, 0, 100)
            .await
            .expect("created event");
        assert!(!created.has_more);
        assert_eq!(created.events.len(), 1);
        assert_eq!(created.events[0].sequence, 1);
        assert_eq!(created.events[0].kind, ConversationTurnEventKind::Created);

        let started = store
            .commit_provider_start(canonical_provider_start(&reserved, "event-stream"))
            .await
            .expect("provider start");
        let text = format!(
            "{}🙂tail",
            "a".repeat(MAX_CONVERSATION_TEXT_CHUNK_BYTES - 1)
        );
        let appended = store
            .append_turn_text(&started.turn.id, started.turn.revision, 0, text.clone())
            .await
            .expect("text append");
        assert_eq!(appended.len(), 2);
        assert_eq!(appended[0].sequence, 3);
        assert_eq!(appended[1].sequence, 4);
        assert!(matches!(
            &appended[0].kind,
            ConversationTurnEventKind::TextAppended {
                start_utf8_offset: 0,
                text,
            } if text.len() == MAX_CONVERSATION_TEXT_CHUNK_BYTES - 1
        ));
        assert!(matches!(
            &appended[1].kind,
            ConversationTurnEventKind::TextAppended {
                start_utf8_offset,
                text: tail,
            } if *start_utf8_offset == u64::try_from(MAX_CONVERSATION_TEXT_CHUNK_BYTES - 1).expect("offset")
                && tail == "🙂tail"
        ));

        let replay = store
            .append_turn_text(&started.turn.id, started.turn.revision, 0, text)
            .await
            .expect("exact replay");
        assert_eq!(replay, appended);
        assert!(matches!(
            store
                .append_turn_text(&started.turn.id, started.turn.revision, 1, "forged".into())
                .await,
            Err(StoreError::Conflict)
        ));
        assert!(matches!(
            store
                .append_turn_text(
                    &started.turn.id,
                    0,
                    u64::try_from(appended_text(&appended).len()).expect("offset"),
                    "x".into()
                )
                .await,
            Err(StoreError::Conflict)
        ));

        let first_page = store
            .list_turn_events_since(&started.turn.id, 0, 2)
            .await
            .expect("first event page");
        assert_eq!(
            first_page
                .events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(first_page.has_more);
        let second_page = store
            .list_turn_events_since(&started.turn.id, 2, 2)
            .await
            .expect("second event page");
        assert_eq!(second_page.events, appended);
        assert!(!second_page.has_more);
        assert!(matches!(
            store
                .list_turn_events_since(&started.turn.id, 0, MAX_CONVERSATION_EVENT_BATCH + 1)
                .await,
            Err(StoreError::Conflict)
        ));
    }

    fn appended_text(events: &[ConversationTurnEvent]) -> String {
        events
            .iter()
            .filter_map(|event| match &event.kind {
                ConversationTurnEventKind::TextAppended { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn conversation_completion_requires_the_exact_durable_event_text() {
        let (store, reserved) = conversation_store_fixture("event-mismatch").await;
        let started = store
            .commit_provider_start(canonical_provider_start(&reserved, "event-mismatch"))
            .await
            .expect("provider start");
        store
            .append_turn_text(
                &started.turn.id,
                started.turn.revision,
                0,
                "Different durable answer".into(),
            )
            .await
            .expect("text append");
        assert!(matches!(
            store
                .commit_terminal(canonical_completed_terminal(&started, "event-mismatch"))
                .await,
            Err(StoreError::Conflict)
        ));
        assert_eq!(load_conversation_snapshot(&store, &started).await, started);

        let (exact_store, exact_reserved) = conversation_store_fixture("event-exact").await;
        let exact_started = exact_store
            .commit_provider_start(canonical_provider_start(&exact_reserved, "event-exact"))
            .await
            .expect("provider start");
        exact_store
            .append_turn_text(
                &exact_started.turn.id,
                exact_started.turn.revision,
                0,
                "Canonical answer".into(),
            )
            .await
            .expect("text append");
        let completed = exact_store
            .commit_terminal(canonical_completed_terminal(&exact_started, "event-exact"))
            .await
            .expect("exact completion");
        let events = exact_store
            .list_turn_events_since(&completed.turn.id, 0, 100)
            .await
            .expect("completed event log");
        assert_eq!(events.events.len(), 4);
        assert!(matches!(
            events.events.last().map(|event| &event.kind),
            Some(ConversationTurnEventKind::StateChanged {
                from: ConversationTurnState::ProviderStarted,
                to: ConversationTurnState::Completed,
            })
        ));
    }

    #[tokio::test]
    async fn conversation_event_count_and_corrupt_logs_fail_closed() {
        let (store, reserved) = conversation_store_fixture("event-count").await;
        let started = store
            .commit_provider_start(canonical_provider_start(&reserved, "event-count"))
            .await
            .expect("provider start");
        {
            let mut state = store.state.lock().await;
            let mut events = state
                .conversation_events
                .get(&started.turn.id)
                .cloned()
                .expect("event log");
            let mut log = ConversationTurnEventLog::restore(started.turn.id.clone(), &events)
                .expect("valid started log");
            for _ in 0..grok_domain::MAX_CONVERSATION_TEXT_EVENTS {
                events.push(
                    log.append_kind(ConversationTurnEventKind::TextAppended {
                        start_utf8_offset: log.next_utf8_offset(),
                        text: "x".into(),
                    })
                    .expect("event within limit"),
                );
            }
            state
                .conversation_events
                .insert(started.turn.id.clone(), events);
        }
        assert!(matches!(
            store
                .append_turn_text(
                    &started.turn.id,
                    started.turn.revision,
                    u64::try_from(grok_domain::MAX_CONVERSATION_TEXT_EVENTS).expect("offset"),
                    "x".into(),
                )
                .await,
            Err(StoreError::Conflict)
        ));

        {
            let mut state = store.state.lock().await;
            state
                .conversation_events
                .get_mut(&started.turn.id)
                .expect("event log")[1]
                .sequence = 99;
        }
        assert!(matches!(
            store.list_turn_events_since(&started.turn.id, 0, 100).await,
            Err(StoreError::Internal(_))
        ));
        assert!(matches!(
            load_conversation_snapshot_result(&store, &started).await,
            Err(StoreError::Internal(_))
        ));
    }

    fn artifact_command(scope: &str, key: &str, fingerprint: u8) -> MutationCommand {
        MutationCommand {
            scope: scope.into(),
            key: key.into(),
            fingerprint: [fingerprint; 32],
        }
    }

    async fn seed_artifact_project(
        store: &InMemoryExecutionStore,
        id: &str,
        now: UnixMillis,
    ) -> Project {
        let project = Project::new(
            ProjectId::new(id).expect("project ID"),
            "Artifact container".into(),
            String::new(),
            now,
        )
        .expect("project");
        store
            .state
            .lock()
            .await
            .projects
            .insert(project.id.clone(), project.clone());
        project
    }

    fn unavailable_artifact(
        project_id: ProjectId,
        id: &str,
        name: &str,
        now: UnixMillis,
    ) -> Artifact {
        Artifact::new_unavailable(
            ArtifactId::new(id).expect("artifact ID"),
            project_id,
            None,
            name.into(),
            now,
        )
        .expect("artifact")
    }

    fn artifact_version(
        artifact_id: ArtifactId,
        digest: u8,
        byte_size: u64,
        now: UnixMillis,
    ) -> ArtifactVersion {
        ArtifactVersion::new(
            artifact_id,
            1,
            [digest; 32],
            "text/plain".into(),
            byte_size,
            now,
        )
        .expect("artifact version")
    }

    fn content_ready(result: ArtifactContentReadyResult) -> ArtifactImportPlan {
        match result {
            ArtifactContentReadyResult::ContentReady(plan) => plan,
            ArtifactContentReadyResult::QuotaExceeded { .. } => {
                panic!("unexpected artifact quota failure")
            }
        }
    }

    fn quota_failure(
        result: ArtifactContentReadyResult,
        expected: ArtifactImportFailureCode,
    ) -> ArtifactImportPlan {
        match result {
            ArtifactContentReadyResult::QuotaExceeded { plan, failure } => {
                assert_eq!(failure, expected);
                plan
            }
            ArtifactContentReadyResult::ContentReady(_) => {
                panic!("expected artifact quota failure")
            }
        }
    }

    async fn commit_memory_artifact(
        store: &InMemoryExecutionStore,
        project_id: ProjectId,
        id: &str,
        key: &str,
        digest: u8,
        byte_size: u64,
        now: UnixMillis,
    ) -> (Artifact, ArtifactVersion) {
        let artifact = unavailable_artifact(project_id, id, &format!("{id}.txt"), now);
        let command = artifact_command("import_artifact", key, digest);
        let prepared = match store
            .reserve_import(artifact, &command)
            .await
            .expect("reserve import")
        {
            ArtifactImportReservation::NewlyPrepared(plan) => plan,
            ArtifactImportReservation::ExactReplay(_) => panic!("unexpected import replay"),
        };
        let content = artifact_version(
            prepared.artifact.id.clone(),
            digest,
            byte_size,
            now.saturating_add(1),
        );
        let ready = content_ready(
            store
                .mark_content_ready(
                    &prepared.artifact.id,
                    0,
                    content.clone(),
                    now.saturating_add(1),
                )
                .await
                .expect("content ready"),
        );
        assert_eq!(ready.state, ArtifactImportState::ContentReady);
        let mut available = ready.artifact.clone();
        available
            .record_content(content.summary(), now.saturating_add(2))
            .expect("available artifact");
        let committed = store
            .commit_import(
                available,
                0,
                ready.revision,
                content.clone(),
                now.saturating_add(2),
            )
            .await
            .expect("commit import");
        (committed.artifact, content)
    }

    fn insert_accounted_artifact(
        state: &mut State,
        project_id: ProjectId,
        id: &str,
        digest: u8,
        byte_size: u64,
        now: UnixMillis,
    ) {
        let mut artifact = unavailable_artifact(project_id, id, &format!("{id}.bin"), now);
        let version = ArtifactVersion::new(
            artifact.id.clone(),
            1,
            [digest; 32],
            "application/octet-stream".into(),
            byte_size,
            now,
        )
        .expect("accounted version");
        artifact
            .record_content(version.summary(), now)
            .expect("accounted artifact");
        let retention = ArtifactRetentionRecord::retained(version.clone()).expect("retention");
        state
            .artifact_versions
            .insert((artifact.id.clone(), 1), version);
        state
            .artifact_retention
            .insert((artifact.id.clone(), 1), retention);
        state.artifacts.insert(artifact.id.clone(), artifact);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn artifact_import_store_is_exact_atomic_and_searches_only_available_content() {
        let store = InMemoryExecutionStore::new();
        let project = seed_artifact_project(&store, "artifact-project", 10).await;
        let artifact = unavailable_artifact(
            project.id.clone(),
            "artifact-report",
            "Memory report.txt",
            10,
        );
        let command = artifact_command("import_artifact", "report-import", 1);
        let prepared = match store
            .reserve_import(artifact.clone(), &command)
            .await
            .expect("reserve")
        {
            ArtifactImportReservation::NewlyPrepared(plan) => plan,
            ArtifactImportReservation::ExactReplay(_) => panic!("unexpected replay"),
        };
        assert_eq!(prepared.state, ArtifactImportState::Prepared);
        assert_eq!(
            store.resolve_import(&command).await.expect("resolve"),
            Some(prepared.clone())
        );
        assert!(matches!(
            store
                .reserve_import(artifact.clone(), &command)
                .await
                .expect("exact replay"),
            ArtifactImportReservation::ExactReplay(plan) if plan == prepared
        ));
        let changed = artifact_command("import_artifact", "report-import", 2);
        assert!(matches!(
            store.resolve_import(&changed).await,
            Err(StoreError::Conflict)
        ));

        let blocked =
            unavailable_artifact(project.id.clone(), "artifact-blocked", "Blocked.txt", 11);
        assert!(matches!(
            store
                .reserve_import(
                    blocked.clone(),
                    &artifact_command("import_artifact", "blocked-import", 3)
                )
                .await,
            Err(StoreError::Conflict)
        ));
        assert!(matches!(
            store.get_artifact(&blocked.id).await,
            Err(StoreError::NotFound)
        ));
        assert!(
            store
                .list_incomplete_imports(0)
                .await
                .expect("zero recovery page")
                .is_empty()
        );
        assert_eq!(
            store
                .list_incomplete_imports(1)
                .await
                .expect("prepared recovery"),
            vec![prepared.clone()]
        );
        assert!(
            WorkspaceStore::search(&store, Some(&project.id), "memory report", 0, 10)
                .await
                .expect("unavailable search")
                .is_empty()
        );

        let content = artifact_version(artifact.id.clone(), 4, 5, 11);
        let ready = content_ready(
            store
                .mark_content_ready(&artifact.id, 0, content.clone(), 11)
                .await
                .expect("content ready"),
        );
        assert_eq!(ready.state, ArtifactImportState::ContentReady);
        assert!(matches!(
            store
                .mark_content_ready(&artifact.id, 0, content.clone(), 11)
                .await,
            Err(StoreError::Conflict)
        ));
        let mut available = ready.artifact.clone();
        available
            .record_content(content.summary(), 12)
            .expect("available");
        let committed = store
            .commit_import(available.clone(), 0, 1, content.clone(), 12)
            .await
            .expect("commit");
        assert_eq!(committed.state, ArtifactImportState::Committed);
        assert_eq!(committed.artifact, available);
        assert_eq!(
            store
                .get_artifact_version(&artifact.id, 1)
                .await
                .expect("version"),
            content
        );
        assert_eq!(
            store
                .resolve_import(&command)
                .await
                .expect("terminal replay"),
            Some(committed.clone())
        );
        assert!(matches!(
            store
                .commit_import(available, 0, 1, content.clone(), 12)
                .await,
            Err(StoreError::Conflict)
        ));
        assert_eq!(
            store.quota_usage(&project.id).await.expect("usage"),
            ArtifactQuotaUsage {
                project_artifact_count: 1,
                project_bytes: 5,
                global_bytes: 5,
            }
        );
        let hits = WorkspaceStore::search(&store, Some(&project.id), "memory report", 0, 10)
            .await
            .expect("available search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, WorkspaceSearchKind::Artifact);
        assert!(hits[0].snippet.is_empty());

        let failed_artifact = unavailable_artifact(
            project.id.clone(),
            "artifact-failed",
            "Failed import.txt",
            13,
        );
        let failed_command = artifact_command("import_artifact", "failed-import", 5);
        let failed_prepared = match store
            .reserve_import(failed_artifact, &failed_command)
            .await
            .expect("reserve failure")
        {
            ArtifactImportReservation::NewlyPrepared(plan) => plan,
            ArtifactImportReservation::ExactReplay(_) => panic!("unexpected replay"),
        };
        let failed = store
            .fail_import(
                &failed_prepared.artifact.id,
                0,
                ArtifactImportFailureCode::SourceUnavailable,
                14,
            )
            .await
            .expect("durable failure");
        assert_eq!(failed.state, ArtifactImportState::Failed);
        assert_eq!(
            store
                .resolve_import(&failed_command)
                .await
                .expect("failure replay"),
            Some(failed)
        );
        assert!(
            store
                .list_incomplete_imports(10)
                .await
                .expect("no incomplete imports")
                .is_empty()
        );

        let staged_failure = unavailable_artifact(
            project.id.clone(),
            "artifact-staged-failure",
            "Staged failure.txt",
            15,
        );
        let staged_failure = match store
            .reserve_import(
                staged_failure,
                &artifact_command("import_artifact", "staged-failure", 6),
            )
            .await
            .expect("reserve staged failure")
        {
            ArtifactImportReservation::NewlyPrepared(plan) => plan,
            ArtifactImportReservation::ExactReplay(_) => panic!("unexpected replay"),
        };
        let staged_content = artifact_version(staged_failure.artifact.id.clone(), 6, 7, 16);
        let staged_failure = content_ready(
            store
                .mark_content_ready(
                    &staged_failure.artifact.id,
                    staged_failure.revision,
                    staged_content,
                    16,
                )
                .await
                .expect("staged content"),
        );
        let staged_failure = store
            .fail_import(
                &staged_failure.artifact.id,
                staged_failure.revision,
                ArtifactImportFailureCode::IntegrityFailure,
                17,
            )
            .await
            .expect("staged terminal failure");
        assert_eq!(staged_failure.state, ArtifactImportState::Failed);
        assert_eq!(staged_failure.revision, 2);
        assert!(staged_failure.content.is_some());

        {
            let mut state = store.state.lock().await;
            state
                .projects
                .get_mut(&project.id)
                .expect("project")
                .archive(18)
                .expect("archive");
        }
        assert_eq!(
            store
                .resolve_import(&command)
                .await
                .expect("archived replay"),
            Some(committed)
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn artifact_import_store_enforces_count_byte_and_version_bounds_atomically() {
        let count_store = InMemoryExecutionStore::new();
        let count_project = seed_artifact_project(&count_store, "count-project", 10).await;
        {
            let mut state = count_store.state.lock().await;
            for index in 0..MAX_PROJECT_ARTIFACT_COUNT {
                let artifact = unavailable_artifact(
                    count_project.id.clone(),
                    &format!("count-{index}"),
                    &format!("count-{index}.txt"),
                    10,
                );
                state.artifacts.insert(artifact.id.clone(), artifact);
            }
        }
        let before = count_store.state.lock().await.artifacts.len();
        let overflow = unavailable_artifact(
            count_project.id.clone(),
            "count-overflow",
            "overflow.txt",
            11,
        );
        let overflow_command = artifact_command("import_artifact", "count-overflow", 1);
        assert!(matches!(
            count_store
                .reserve_import(overflow, &overflow_command)
                .await,
            Err(StoreError::Conflict)
        ));
        assert_eq!(count_store.state.lock().await.artifacts.len(), before);
        assert_eq!(
            count_store
                .resolve_import(&overflow_command)
                .await
                .expect("no rejection row"),
            None
        );

        let file_store = InMemoryExecutionStore::new();
        let file_project = seed_artifact_project(&file_store, "file-project", 20).await;
        let file = unavailable_artifact(file_project.id, "file-too-large", "large.bin", 20);
        let file_command = artifact_command("import_artifact", "file-too-large", 2);
        let file_plan = match file_store
            .reserve_import(file, &file_command)
            .await
            .expect("file reservation")
        {
            ArtifactImportReservation::NewlyPrepared(plan) => plan,
            ArtifactImportReservation::ExactReplay(_) => panic!("unexpected replay"),
        };
        let failed = quota_failure(
            file_store
                .mark_content_ready(
                    &file_plan.artifact.id,
                    0,
                    artifact_version(
                        file_plan.artifact.id.clone(),
                        2,
                        MAX_ARTIFACT_FILE_BYTES + 1,
                        21,
                    ),
                    21,
                )
                .await
                .expect("durable file failure"),
            ArtifactImportFailureCode::FileTooLarge,
        );
        assert_eq!(failed.state, ArtifactImportState::Prepared);
        assert_eq!(failed.revision, 0);
        assert!(failed.content.is_none());

        let quota_store = InMemoryExecutionStore::new();
        let project = seed_artifact_project(&quota_store, "quota-project", 30).await;
        let global_target = seed_artifact_project(&quota_store, "global-target", 30).await;
        {
            let mut state = quota_store.state.lock().await;
            insert_accounted_artifact(
                &mut state,
                project.id.clone(),
                "project-full",
                3,
                MAX_PROJECT_ARTIFACT_BYTES,
                30,
            );
        }
        let project_candidate = unavailable_artifact(
            project.id.clone(),
            "project-overflow",
            "project-overflow.txt",
            31,
        );
        let project_plan = match quota_store
            .reserve_import(
                project_candidate,
                &artifact_command("import_artifact", "project-overflow", 4),
            )
            .await
            .expect("project reservation")
        {
            ArtifactImportReservation::NewlyPrepared(plan) => plan,
            ArtifactImportReservation::ExactReplay(_) => panic!("unexpected replay"),
        };
        let project_failed = quota_failure(
            quota_store
                .mark_content_ready(
                    &project_plan.artifact.id,
                    0,
                    artifact_version(project_plan.artifact.id.clone(), 4, 1, 32),
                    32,
                )
                .await
                .expect("project quota failure"),
            ArtifactImportFailureCode::ProjectByteQuotaExceeded,
        );
        assert_eq!(project_failed.state, ArtifactImportState::Prepared);
        quota_store
            .fail_import(
                &project_failed.artifact.id,
                project_failed.revision,
                ArtifactImportFailureCode::ProjectByteQuotaExceeded,
                32,
            )
            .await
            .expect("terminalize project quota after cleanup");

        {
            let mut state = quota_store.state.lock().await;
            for index in 0..3_u8 {
                let extra = Project::new(
                    ProjectId::new(format!("global-source-{index}")).expect("project ID"),
                    format!("Global {index}"),
                    String::new(),
                    33,
                )
                .expect("project");
                state.projects.insert(extra.id.clone(), extra.clone());
                insert_accounted_artifact(
                    &mut state,
                    extra.id,
                    &format!("global-full-{index}"),
                    10 + index,
                    MAX_PROJECT_ARTIFACT_BYTES,
                    33,
                );
            }
        }
        let global_candidate = unavailable_artifact(
            global_target.id,
            "global-overflow",
            "global-overflow.txt",
            34,
        );
        let global_plan = match quota_store
            .reserve_import(
                global_candidate,
                &artifact_command("import_artifact", "global-overflow", 9),
            )
            .await
            .expect("global reservation")
        {
            ArtifactImportReservation::NewlyPrepared(plan) => plan,
            ArtifactImportReservation::ExactReplay(_) => panic!("unexpected replay"),
        };
        let global_failed = quota_failure(
            quota_store
                .mark_content_ready(
                    &global_plan.artifact.id,
                    0,
                    artifact_version(global_plan.artifact.id.clone(), 9, 1, 35),
                    35,
                )
                .await
                .expect("global quota failure"),
            ArtifactImportFailureCode::GlobalByteQuotaExceeded,
        );
        assert_eq!(global_failed.state, ArtifactImportState::Prepared);
        quota_store
            .fail_import(
                &global_failed.artifact.id,
                global_failed.revision,
                ArtifactImportFailureCode::GlobalByteQuotaExceeded,
                35,
            )
            .await
            .expect("terminalize global quota after cleanup");

        let atomic_store = InMemoryExecutionStore::new();
        let atomic_project = seed_artifact_project(&atomic_store, "atomic-project", 40).await;
        let atomic_artifact =
            unavailable_artifact(atomic_project.id, "atomic-artifact", "atomic.txt", 40);
        let atomic_plan = match atomic_store
            .reserve_import(
                atomic_artifact,
                &artifact_command("import_artifact", "atomic-import", 7),
            )
            .await
            .expect("atomic reservation")
        {
            ArtifactImportReservation::NewlyPrepared(plan) => plan,
            ArtifactImportReservation::ExactReplay(_) => panic!("unexpected replay"),
        };
        let atomic_content = artifact_version(atomic_plan.artifact.id.clone(), 7, 8, 41);
        let atomic_ready = content_ready(
            atomic_store
                .mark_content_ready(&atomic_plan.artifact.id, 0, atomic_content.clone(), 41)
                .await
                .expect("atomic ready"),
        );
        {
            atomic_store
                .state
                .lock()
                .await
                .artifact_versions
                .insert((atomic_plan.artifact.id.clone(), 1), atomic_content.clone());
        }
        let mut atomic_available = atomic_ready.artifact.clone();
        atomic_available
            .record_content(atomic_content.summary(), 42)
            .expect("atomic available");
        assert!(matches!(
            atomic_store
                .commit_import(atomic_available.clone(), 0, 1, atomic_content.clone(), 42,)
                .await,
            Err(StoreError::Conflict)
        ));
        assert_eq!(
            atomic_store
                .get_artifact(&atomic_plan.artifact.id)
                .await
                .expect("unchanged artifact")
                .state,
            ArtifactState::Unavailable
        );
        assert_eq!(
            atomic_store
                .list_incomplete_imports(1)
                .await
                .expect("still recoverable")[0]
                .state,
            ArtifactImportState::ContentReady
        );
        atomic_store
            .state
            .lock()
            .await
            .artifact_versions
            .remove(&(atomic_plan.artifact.id.clone(), 1));
        assert_eq!(
            atomic_store
                .commit_import(atomic_available, 0, 1, atomic_content, 42)
                .await
                .expect("retry commit")
                .state,
            ArtifactImportState::Committed
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn artifact_open_store_replays_exactly_and_never_overlaps_or_replays_dispatch() {
        let store = InMemoryExecutionStore::new();
        let project = seed_artifact_project(&store, "open-project", 10).await;
        let (first, first_content) = commit_memory_artifact(
            &store,
            project.id.clone(),
            "open-first",
            "open-first-import",
            1,
            10,
            10,
        )
        .await;
        let (second, second_content) = commit_memory_artifact(
            &store,
            project.id,
            "open-second",
            "open-second-import",
            2,
            10,
            20,
        )
        .await;
        let command = artifact_command("open_artifact", "open-command", 20);
        let prepared = match store
            .prepare_open(first_content.clone(), &command, 30)
            .await
            .expect("prepare open")
        {
            ArtifactOpenReservation::NewlyPrepared(plan) => plan,
            ArtifactOpenReservation::ExactReplay(_) => panic!("unexpected replay"),
        };
        assert_eq!(prepared.state, ArtifactOpenState::Prepared);
        assert_eq!(
            store.resolve_open(&command).await.expect("resolve open"),
            Some(prepared.clone())
        );
        assert!(matches!(
            store
                .prepare_open(first_content.clone(), &command, 31)
                .await
                .expect("exact open replay"),
            ArtifactOpenReservation::ExactReplay(plan) if plan == prepared
        ));
        assert!(matches!(
            store
                .resolve_open(&artifact_command("open_artifact", "open-command", 21))
                .await,
            Err(StoreError::Conflict)
        ));
        assert!(matches!(
            store
                .prepare_open(
                    second_content.clone(),
                    &artifact_command("open_artifact", "overlapping-open", 22),
                    31,
                )
                .await,
            Err(StoreError::Conflict)
        ));
        assert!(
            store
                .list_incomplete_opens(0)
                .await
                .expect("zero open page")
                .is_empty()
        );
        assert_eq!(
            store.list_incomplete_opens(1).await.expect("prepared open"),
            vec![prepared.clone()]
        );
        let dispatching = store
            .mark_open_dispatching(&first.id, 1, 0, 31)
            .await
            .expect("dispatching");
        assert_eq!(dispatching.state, ArtifactOpenState::Dispatching);
        assert!(matches!(
            store.mark_open_dispatching(&first.id, 1, 0, 31).await,
            Err(StoreError::Conflict)
        ));
        let opened = store
            .complete_open(&first.id, 1, 1, 32)
            .await
            .expect("opened");
        assert_eq!(opened.state, ArtifactOpenState::Opened);
        assert!(
            store
                .list_incomplete_opens(10)
                .await
                .expect("no incomplete open")
                .is_empty()
        );
        assert_eq!(
            store.resolve_open(&command).await.expect("opened replay"),
            Some(opened.clone())
        );

        let prepared_failure = match store
            .prepare_open(
                first_content.clone(),
                &artifact_command("open_artifact", "prepared-failure", 23),
                33,
            )
            .await
            .expect("prepare failure")
        {
            ArtifactOpenReservation::NewlyPrepared(plan) => plan,
            ArtifactOpenReservation::ExactReplay(_) => panic!("unexpected replay"),
        };
        let failed = store
            .fail_open(
                &first.id,
                1,
                prepared_failure.revision,
                ArtifactOpenFailureCode::PlatformUnavailable,
                34,
            )
            .await
            .expect("known pre-dispatch failure");
        assert_eq!(failed.state, ArtifactOpenState::Failed);
        assert_eq!(failed.revision, 1);

        let dispatch_failure = match store
            .prepare_open(
                first_content.clone(),
                &artifact_command("open_artifact", "dispatch-failure", 24),
                35,
            )
            .await
            .expect("prepare dispatch failure")
        {
            ArtifactOpenReservation::NewlyPrepared(plan) => plan,
            ArtifactOpenReservation::ExactReplay(_) => panic!("unexpected replay"),
        };
        let dispatch_failure = store
            .mark_open_dispatching(&first.id, 1, dispatch_failure.revision, 36)
            .await
            .expect("dispatch failure start");
        let failed = store
            .fail_open(
                &first.id,
                1,
                dispatch_failure.revision,
                ArtifactOpenFailureCode::IntegrityFailure,
                37,
            )
            .await
            .expect("known dispatch failure");
        assert_eq!(failed.state, ArtifactOpenState::Failed);
        assert_eq!(failed.revision, 2);

        let review_command = artifact_command("open_artifact", "review-open", 25);
        let review = match store
            .prepare_open(first_content.clone(), &review_command, 38)
            .await
            .expect("prepare review")
        {
            ArtifactOpenReservation::NewlyPrepared(plan) => plan,
            ArtifactOpenReservation::ExactReplay(_) => panic!("unexpected replay"),
        };
        let review = store
            .mark_open_dispatching(&first.id, 1, review.revision, 39)
            .await
            .expect("review dispatch");
        let review = store
            .interrupt_open(&first.id, 1, review.revision, 40)
            .await
            .expect("interrupt");
        assert_eq!(review.state, ArtifactOpenState::InterruptedNeedsReview);

        {
            let mut state = store.state.lock().await;
            let artifact = state.artifacts.get_mut(&first.id).expect("artifact");
            artifact.state = ArtifactState::Deleted;
            artifact.content = None;
            artifact.revision = artifact.revision.saturating_add(1);
            artifact.updated_at = 41;
            Artifact::restore(artifact.clone()).expect("valid tombstone");
        }
        assert_eq!(
            store
                .resolve_open(&review_command)
                .await
                .expect("review survives tombstone"),
            Some(review.clone())
        );
        assert!(matches!(
            store
                .prepare_open(first_content, &review_command, 42)
                .await
                .expect("exact tombstoned replay"),
            ArtifactOpenReservation::ExactReplay(plan) if plan == review
        ));
        assert!(matches!(
            store
                .prepare_open(
                    second_content,
                    &artifact_command("open_artifact", "new-after-tombstone", 26),
                    42,
                )
                .await,
            Ok(ArtifactOpenReservation::NewlyPrepared(_))
        ));
        assert_eq!(second.state, ArtifactState::Available);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn artifact_removal_tombstones_before_purge_and_releases_only_confirmed_bytes() {
        let store = InMemoryExecutionStore::new();
        let project = seed_artifact_project(&store, "removal-project", 10).await;
        let (first, first_content) = commit_memory_artifact(
            &store,
            project.id.clone(),
            "remove-first",
            "remove-first-import",
            1,
            11,
            10,
        )
        .await;
        let (second, second_content) = commit_memory_artifact(
            &store,
            project.id.clone(),
            "remove-second",
            "remove-second-import",
            2,
            13,
            20,
        )
        .await;
        assert_eq!(
            store.quota_usage(&project.id).await.expect("initial quota"),
            ArtifactQuotaUsage {
                project_artifact_count: 2,
                project_bytes: 24,
                global_bytes: 24,
            }
        );

        let command = artifact_command("remove_artifact", "remove-first", 31);
        let pending = match store
            .reserve_removal(&first.id, first.revision, 1, &command, 30)
            .await
            .expect("reserve removal")
        {
            ArtifactRemovalReservation::NewlyPending(plan) => plan,
            ArtifactRemovalReservation::ExactReplay(_) => panic!("unexpected removal replay"),
        };
        assert_eq!(pending.state, ArtifactRemovalState::Pending);
        assert_eq!(pending.artifact.state, ArtifactState::Deleted);
        assert!(pending.artifact.content.is_none());
        assert_eq!(pending.artifact.revision, first.revision + 1);
        assert_eq!(
            store.quota_usage(&project.id).await.expect("pending quota"),
            ArtifactQuotaUsage {
                project_artifact_count: 1,
                project_bytes: 24,
                global_bytes: 24,
            },
            "logical tombstone releases count but not retained bytes"
        );
        assert!(matches!(
            store
                .reserve_removal(&first.id, first.revision, 1, &command, 31)
                .await
                .expect("exact pending replay"),
            ArtifactRemovalReservation::ExactReplay(plan) if plan == pending
        ));
        assert!(matches!(
            store
                .resolve_removal(&artifact_command("remove_artifact", "remove-first", 32))
                .await,
            Err(StoreError::Conflict)
        ));
        assert!(matches!(
            store
                .reserve_removal(
                    &second.id,
                    second.revision,
                    1,
                    &artifact_command("remove_artifact", "remove-overlap", 33),
                    31,
                )
                .await,
            Err(StoreError::Conflict)
        ));
        assert!(matches!(
            store
                .prepare_open(
                    first_content.clone(),
                    &artifact_command("open_artifact", "open-tombstone", 34),
                    31,
                )
                .await,
            Err(StoreError::Conflict)
        ));

        let pending_versions = store
            .list_pending_removal_versions(&first.id, 10)
            .await
            .expect("pending version");
        assert_eq!(pending_versions.len(), 1);
        let retention = &pending_versions[0];
        assert_eq!(retention.content, first_content);
        assert_eq!(retention.state, ArtifactRetentionState::PurgePending);
        assert!(matches!(
            store
                .mark_content_purged(&first.id, 1, retention.revision + 1, 32)
                .await,
            Err(StoreError::Conflict)
        ));
        let purged = store
            .mark_content_purged(&first.id, 1, retention.revision, 32)
            .await
            .expect("mark purged");
        assert_eq!(purged.state, ArtifactRetentionState::Purged);
        assert_eq!(purged.purged_at, Some(32));
        assert_eq!(
            store.quota_usage(&project.id).await.expect("purged quota"),
            ArtifactQuotaUsage {
                project_artifact_count: 1,
                project_bytes: second_content.byte_size,
                global_bytes: second_content.byte_size,
            }
        );
        let committed = store
            .commit_removal(&first.id, pending.revision, 33)
            .await
            .expect("commit removal");
        assert_eq!(committed.state, ArtifactRemovalState::Committed);
        assert_eq!(committed.artifact, pending.artifact);
        assert_eq!(
            store
                .resolve_removal(&command)
                .await
                .expect("terminal replay"),
            Some(committed)
        );
        assert!(
            store
                .list_incomplete_removals(10)
                .await
                .expect("no pending removals")
                .is_empty()
        );
        assert_eq!(
            store
                .get_artifact_version(&first.id, 1)
                .await
                .expect("immutable metadata retained"),
            first_content
        );
        store
            .state
            .lock()
            .await
            .artifact_retention
            .remove(&(first.id.clone(), 1));
        assert!(matches!(
            store.resolve_removal(&command).await,
            Err(StoreError::Internal(_))
        ));
    }

    async fn load_conversation_snapshot_result(
        store: &InMemoryExecutionStore,
        snapshot: &ConversationTurnSnapshot,
    ) -> Result<ConversationTurnSnapshot, StoreError> {
        store
            .load_turn_by_command(&MutationCommand {
                scope: "execute_conversation_turn".into(),
                key: snapshot.turn.idempotency_key.clone(),
                fingerprint: snapshot.turn.request_fingerprint,
            })
            .await?
            .ok_or(StoreError::NotFound)
    }
}
