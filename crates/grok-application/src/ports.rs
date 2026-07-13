use async_trait::async_trait;
use grok_domain::{
    Approval, ApprovalId, Automation, AutomationHistoryEntry, AutomationId, ChatModelPreference,
    DesktopPreferences, HostExecutionPolicy, Message, MessageId, Project, ProjectId, Run, RunEvent,
    RunEventKind, RunId, SideEffect, Thread, ThreadId, UnixMillis,
};
use thiserror::Error;
use zeroize::Zeroize;

/// A 256-bit `SQLCipher` key that clears its allocation on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct DatabaseKey([u8; 32]);

impl DatabaseKey {
    /// Copies a key from a secure provider into a short-lived guarded value.
    ///
    /// # Errors
    ///
    /// Returns [`KeyProviderError`] unless `value` is exactly 32 bytes.
    pub fn from_slice(value: &[u8]) -> Result<Self, KeyProviderError> {
        let key: [u8; 32] = value
            .try_into()
            .map_err(|_| KeyProviderError::InvalidKeyLength)?;
        Ok(Self(key))
    }

    /// Borrows the key only for immediate cryptographic configuration.
    #[must_use]
    pub const fn expose_secret(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for DatabaseKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DatabaseKey([REDACTED])")
    }
}

impl Drop for DatabaseKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Failure to retrieve key material from the operating-system vault boundary.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum KeyProviderError {
    /// `SQLCipher` requires an exact 256-bit raw key.
    #[error("database key must be exactly 32 bytes")]
    InvalidKeyLength,
    /// Vault is locked or temporarily unreachable.
    #[error("secure key provider unavailable: {0}")]
    Unavailable(String),
    /// Provider returned an unexpected failure without exposing secrets.
    #[error("secure key provider failure: {0}")]
    Internal(String),
}

/// Supplies encryption keys without coupling use cases to an OS vault API.
pub trait SecureKeyProvider: Send + Sync {
    /// Returns a short-lived copy of the database key.
    ///
    /// # Errors
    ///
    /// Returns [`KeyProviderError`] when the vault cannot provide a valid key.
    fn database_key(&self) -> Result<DatabaseKey, KeyProviderError>;
}

/// Persistence failure independent of a database implementation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StoreError {
    /// Entity does not exist.
    #[error("not found")]
    NotFound,
    /// Optimistic revision or uniqueness check failed.
    #[error("conflict")]
    Conflict,
    /// Storage is temporarily unreachable.
    #[error("unavailable: {0}")]
    Unavailable(String),
    /// Storage reported a non-recoverable error.
    #[error("internal: {0}")]
    Internal(String),
}

/// Durable singleton store for the versioned Host Tools enrollment.
#[async_trait]
pub trait HostExecutionPolicyStore: Send + Sync {
    /// Loads the current inactive, active, or revoked policy snapshot.
    async fn get_host_execution_policy(&self) -> Result<HostExecutionPolicy, StoreError>;

    /// Resolves an exact prior enrollment mutation or conflicts on key reuse.
    async fn resolve_host_execution_policy_mutation(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<HostExecutionPolicy>, StoreError>;

    /// Atomically replaces the policy and complete root set.
    async fn replace_host_execution_policy(
        &self,
        policy: HostExecutionPolicy,
        expected_revision: u64,
        command: &MutationCommand,
    ) -> Result<HostExecutionPolicy, StoreError>;
}

/// Event payload before the store assigns a run-local sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRunEvent {
    /// Event timestamp.
    pub occurred_at: UnixMillis,
    /// Structured event body.
    pub kind: RunEventKind,
}

/// Canonical result retained by the durable execution command journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionMutationOutcome {
    /// Exact run snapshot returned by the first committed command.
    Run(Run),
    /// Exact approval snapshot returned by the first committed command.
    Approval(Approval),
}

/// Atomic persistence boundary for a run aggregate and its approvals/effects.
///
/// Compound methods are deliberate: implementations must commit entity changes
/// and audit events in one transaction.
#[async_trait]
pub trait ExecutionStore: Send + Sync {
    /// Resolves a prior command, conflicting when a key is reused for new input.
    async fn resolve_execution_mutation(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<ExecutionMutationOutcome>, StoreError>;

    /// Inserts a new run and its creation event atomically.
    async fn create_run(
        &self,
        run: Run,
        event: NewRunEvent,
        command: &MutationCommand,
    ) -> Result<Run, StoreError>;

    /// Loads one run.
    async fn get_run(&self, id: &RunId) -> Result<Run, StoreError>;

    /// Saves a revisioned run and appends an event atomically.
    async fn save_run(
        &self,
        run: Run,
        expected_revision: u64,
        event: NewRunEvent,
        command: &MutationCommand,
    ) -> Result<Run, StoreError>;

    /// Inserts an approval, updates its run, and appends events atomically.
    async fn create_approval(
        &self,
        approval: Approval,
        run: Run,
        expected_run_revision: u64,
        events: Vec<NewRunEvent>,
        command: &MutationCommand,
    ) -> Result<Approval, StoreError>;

    /// Loads one approval.
    async fn get_approval(&self, id: &ApprovalId) -> Result<Approval, StoreError>;

    /// Saves an approval and optional run update in one transaction.
    async fn decide_approval(
        &self,
        approval: Approval,
        expected_approval_revision: u64,
        run_update: Option<(Run, u64, NewRunEvent)>,
        command: &MutationCommand,
    ) -> Result<Approval, StoreError>;

    /// Inserts a prepared side-effect intent and appends its event atomically.
    async fn create_effect(&self, effect: SideEffect, event: NewRunEvent)
    -> Result<(), StoreError>;

    /// Loads one side effect.
    async fn get_effect(&self, id: &grok_domain::EffectId) -> Result<SideEffect, StoreError>;

    /// Lists bounded unfinished file/process effects owned by `HostDirect` runs.
    async fn list_recoverable_host_effects(
        &self,
        limit: usize,
    ) -> Result<Vec<SideEffect>, StoreError>;

    /// Lists bounded non-terminal `HostDirect` Work runs for restart recovery.
    async fn list_recoverable_host_runs(&self, limit: usize) -> Result<Vec<Run>, StoreError>;

    /// Saves a revisioned side effect.
    async fn save_effect(
        &self,
        effect: SideEffect,
        expected_revision: u64,
    ) -> Result<(), StoreError>;

    /// Marks an uncertain effect and its run for explicit review atomically.
    async fn interrupt_effect(
        &self,
        effect: SideEffect,
        expected_effect_revision: u64,
        run: Run,
        expected_run_revision: u64,
        events: Vec<NewRunEvent>,
    ) -> Result<(), StoreError>;

    /// Replays events after a client cursor, ordered by sequence.
    async fn events_since(
        &self,
        run_id: &RunId,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<RunEvent>, StoreError>;
}

/// Entity class returned by canonical workspace search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceSearchKind {
    /// Project metadata.
    Project,
    /// Conversation thread metadata.
    Thread,
    /// Canonical message content.
    Message,
    /// Artifact metadata.
    Artifact,
    /// Automation definition metadata.
    Automation,
}

/// Bounded full-text search result without provider or renderer DTOs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSearchHit {
    /// Stable entity identifier.
    pub id: String,
    /// Owning project.
    pub project_id: ProjectId,
    /// Conversation target for thread, message, and thread-owned artifact hits.
    pub thread_id: Option<ThreadId>,
    /// Canonical entity class.
    pub kind: WorkspaceSearchKind,
    /// User-visible title.
    pub title: String,
    /// Bounded plain-text snippet.
    pub snippet: String,
    /// Last indexed update timestamp.
    pub updated_at: UnixMillis,
}

/// Validated mutation key and canonical request fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationCommand {
    /// Stable application operation scope.
    pub scope: String,
    /// Caller-supplied key scoped to one operation.
    pub key: String,
    /// SHA-256 of the operation and normalized application input.
    pub fingerprint: [u8; 32],
}

/// Durable canonical repository for projects and their child entities.
#[async_trait]
pub trait WorkspaceStore: Send + Sync {
    /// Resolves a prior mutation, conflicting when a key is reused for new input.
    async fn resolve_mutation(
        &self,
        scope: &str,
        command: &MutationCommand,
    ) -> Result<Option<String>, StoreError>;

    /// Inserts a project or returns the prior result for the same command key.
    async fn create_project(
        &self,
        project: Project,
        command: &MutationCommand,
    ) -> Result<Project, StoreError>;
    /// Loads a project.
    async fn get_project(&self, id: &ProjectId) -> Result<Project, StoreError>;
    /// Saves a project with optimistic concurrency.
    async fn save_project(
        &self,
        project: Project,
        expected_revision: u64,
        command: &MutationCommand,
    ) -> Result<(), StoreError>;
    /// Lists projects in stable recent-update order after an optional entity cursor.
    async fn list_projects(
        &self,
        after: Option<&ProjectId>,
        limit: usize,
    ) -> Result<Vec<Project>, StoreError>;

    /// Inserts a thread or returns the prior result for the same command key.
    async fn create_thread(
        &self,
        thread: Thread,
        command: &MutationCommand,
    ) -> Result<Thread, StoreError>;
    /// Loads a thread.
    async fn get_thread(&self, id: &ThreadId) -> Result<Thread, StoreError>;
    /// Saves a thread with optimistic concurrency.
    async fn save_thread(
        &self,
        thread: Thread,
        expected_revision: u64,
        command: &MutationCommand,
    ) -> Result<(), StoreError>;
    /// Lists project threads in stable recent-update order.
    async fn list_threads(
        &self,
        project_id: &ProjectId,
        after: Option<&ThreadId>,
        limit: usize,
    ) -> Result<Vec<Thread>, StoreError>;

    /// Atomically assigns a thread-local sequence and inserts a message.
    async fn create_message(
        &self,
        message: Message,
        command: &MutationCommand,
    ) -> Result<Message, StoreError>;
    /// Loads a message.
    async fn get_message(&self, id: &MessageId) -> Result<Message, StoreError>;
    /// Saves an edited or deleted message with optimistic concurrency.
    async fn save_message(
        &self,
        message: Message,
        expected_revision: u64,
        command: &MutationCommand,
    ) -> Result<(), StoreError>;
    /// Lists canonical messages in ascending thread-local sequence order.
    async fn list_messages(
        &self,
        thread_id: &ThreadId,
        after: Option<&MessageId>,
        limit: usize,
    ) -> Result<Vec<Message>, StoreError>;

    /// Inserts an automation or returns the prior command result.
    async fn create_automation(
        &self,
        automation: Automation,
        command: &MutationCommand,
    ) -> Result<Automation, StoreError>;
    /// Loads an automation.
    async fn get_automation(&self, id: &AutomationId) -> Result<Automation, StoreError>;
    /// Saves an automation with optimistic concurrency.
    async fn save_automation(
        &self,
        automation: Automation,
        expected_revision: u64,
        command: &MutationCommand,
    ) -> Result<(), StoreError>;
    /// Lists project automations in stable recent-update order.
    async fn list_automations(
        &self,
        project_id: &ProjectId,
        after: Option<&AutomationId>,
        limit: usize,
    ) -> Result<Vec<Automation>, StoreError>;
    /// Appends one occurrence result, idempotent by scheduled timestamp.
    async fn record_automation_history(
        &self,
        entry: AutomationHistoryEntry,
    ) -> Result<AutomationHistoryEntry, StoreError>;
    /// Lists occurrence history after an automation-local sequence.
    async fn automation_history(
        &self,
        automation_id: &AutomationId,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<AutomationHistoryEntry>, StoreError>;

    /// Searches canonical non-deleted workspace content with a bounded offset.
    async fn search(
        &self,
        project_id: Option<&ProjectId>,
        query: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<WorkspaceSearchHit>, StoreError>;
}

/// Durable repository for process-wide desktop behavior preferences.
#[async_trait]
pub trait DesktopPreferencesStore: Send + Sync {
    /// Resolves an exact prior mutation, conflicting when a key is reused for new input.
    async fn resolve_desktop_preferences_mutation(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<DesktopPreferences>, StoreError>;

    /// Loads the current singleton preference snapshot.
    async fn get_desktop_preferences(&self) -> Result<DesktopPreferences, StoreError>;

    /// Saves a revisioned snapshot and exact idempotent result atomically.
    async fn save_desktop_preferences(
        &self,
        preferences: DesktopPreferences,
        expected_revision: u64,
        command: &MutationCommand,
    ) -> Result<DesktopPreferences, StoreError>;
}

/// Durable repository for the model policy applied to newly reserved Chat turns.
#[async_trait]
pub trait ChatModelPreferenceStore: Send + Sync {
    /// Resolves an exact prior mutation, conflicting when a key is reused for new input.
    async fn resolve_chat_model_preference_mutation(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<ChatModelPreference>, StoreError>;

    /// Loads the current canonical model selection.
    async fn get_chat_model_preference(&self) -> Result<ChatModelPreference, StoreError>;

    /// Saves a revisioned selection and exact idempotent result atomically.
    async fn save_chat_model_preference(
        &self,
        preference: ChatModelPreference,
        expected_revision: u64,
        command: &MutationCommand,
    ) -> Result<ChatModelPreference, StoreError>;
}

/// Wall clock supplied by infrastructure and controlled by tests.
pub trait Clock: Send + Sync {
    /// Current Unix timestamp in milliseconds.
    fn now(&self) -> UnixMillis;
}

/// Collision-resistant identifier source supplied by infrastructure.
pub trait IdGenerator: Send + Sync {
    /// Generates a printable identifier with the requested prefix.
    fn generate(&self, prefix: &str) -> String;
}
