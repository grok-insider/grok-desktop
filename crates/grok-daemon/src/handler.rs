use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use grok_application::{
    ARTIFACT_IMPORT_IO_TIMEOUT_MS, ARTIFACT_OPEN_TIMEOUT_MS, ARTIFACT_REMOVAL_IO_TIMEOUT_MS,
    AcknowledgeConversationForkDelivery, AgentRuntime, AgentRuntimeErrorKind, AgentRuntimeProbe,
    ApplicationError, ApprovalService, ArtifactRemovalResolution, ArtifactService,
    AutomationSchedulerService, BranchConversationThread, CapabilityFacts, CapabilityResolver,
    ChatModelService, ConversationForkCommandResolution, ConversationForkSnapshot,
    ConversationService, ConversationTurnSnapshot, CreateAutomation, CreateProject, CreateThread,
    CredentialEnrollmentRequest, CredentialEnrollmentService, CredentialService,
    DesktopPreferencesService, EditAndBranchConversationTurn, GetUsageSummary,
    GrokBuildAuthService, GrokBuildAuthStatus, HostExecutionPolicyStore, IsolationRuntime,
    MAX_ARTIFACT_RECOVERY_BATCH, MutationCommand, OAuthCancellation, RegenerateConversationTurn,
    RetryConversationTurn, RunService, SelectChatModel, StartConversationTurn,
    StartedConversationFork, StartedConversationTurn, SuperGrokEnrollmentService,
    SuperGrokEnrollmentStatus, UpdateAutomation, UpdateDesktopPreferences, UpdateProject,
    UpdateThread, UsageScope, UsageWindow, WorkspaceService,
};
use grok_domain::{
    ApprovalId, ArtifactId, AutomationId, ConversationTurnId, ConversationTurnState,
    DesktopUpdateChannel, HOST_ACKNOWLEDGMENT_VERSION, HostExecutionPolicy, HostToolClasses,
    MAX_HOST_EXECUTION_ROOTS, MessageId, MissedRunPolicy, OverlapPolicy, ProjectId, RunId,
    ThreadId,
};
use grok_protocol::{
    ConversationRetryEligibility, EnvelopeError, PROTOCOL_VERSION, account_state_to_wire,
    approval_decision_from_wire, approval_to_wire, artifact_open_receipt_to_wire,
    artifact_removal_pending_to_wire, artifact_to_wire, automation_history_to_wire,
    automation_to_wire, capability_to_wire, chat_model_catalog_to_wire,
    chat_model_preference_to_wire, conversation_fork_delivery_to_wire,
    conversation_fork_metadata_to_wire, conversation_fork_to_wire,
    conversation_turn_event_page_to_wire, conversation_turn_to_wire_with_retry_eligibility,
    desktop_preferences_to_wire, event_to_wire, import_artifact_from_wire,
    imported_artifact_to_wire, message_to_wire, missed_run_policy_from_wire,
    open_artifact_from_wire, overlap_policy_from_wire, project_to_wire, remove_artifact_from_wire,
    removed_artifact_to_wire, run_to_wire, thread_to_wire, usage_summary_to_wire, v1,
    validate_envelope, workspace_search_hit_to_wire,
};
use prost::Message as _;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::{Mutex as AsyncMutex, Notify, OwnedSemaphorePermit, Semaphore, oneshot};

use crate::transport::MAX_FRAME_BYTES;
use crate::{HostWorkRuntime, HostWorkService};

const MAX_DISPATCH_DURATION: Duration = Duration::from_mins(1);
const MAX_CREDENTIAL_ENROLLMENT_DURATION: Duration = Duration::from_mins(2);
const MAX_CONVERSATION_DISPATCH_DURATION: Duration = Duration::from_secs(50);
const MAX_HOST_WORK_DISPATCH_DURATION: Duration = Duration::from_mins(5);
const CONVERSATION_CLEANUP_GRACE: Duration = Duration::from_secs(2);
const CONVERSATION_RECONCILIATION_RETRY: Duration = Duration::from_millis(100);
const MAX_CONVERSATION_RECONCILIATION_ATTEMPTS: usize = 5;
const MAX_CONVERSATION_RECONCILIATION_DURATION: Duration = Duration::from_secs(2);
const MAX_CONVERSATION_TASKS: usize = 8;
const MAX_RUN_EVENT_POLL_WAIT: Duration = Duration::from_secs(20);
const RUN_EVENT_POLL_RESPONSE_RESERVE: Duration = Duration::from_secs(1);
const MAX_RUN_EVENT_POLL_DISPATCH_DURATION: Duration = Duration::from_secs(21);
const RUN_EVENT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const MAX_RUN_EVENT_BATCH_SIZE: usize = 100;
const MAX_RUN_EVENT_SEQUENCE: u64 = i64::MAX as u64;
const MAX_CHAT_MODEL_DISPATCH_DURATION: Duration = Duration::from_secs(15);
// Leaves a bounded window for the application to persist a stable failure or
// uncertainty transition after its inner artifact I/O timeout. Electron's
// 35s import/removal and 15s open budgets retain additional transport margin.
const ARTIFACT_TERMINALIZATION_RESERVE: Duration = Duration::from_secs(2);
const ARTIFACT_REMOVAL_RECOVERY_INITIAL_RETRY: Duration = Duration::from_millis(100);
const ARTIFACT_REMOVAL_RECOVERY_MAX_RETRY: Duration = Duration::from_secs(30);

#[derive(Default)]
struct ArtifactRemovalRecoveryState {
    running: AtomicBool,
    wake: Notify,
}

struct ArtifactRemovalLifetime {
    recovery: Weak<ArtifactRemovalRecoveryState>,
}

impl Drop for ArtifactRemovalLifetime {
    fn drop(&mut self) {
        if let Some(recovery) = self.recovery.upgrade() {
            recovery.wake.notify_one();
        }
    }
}

struct ArtifactRemovalRecoveryGuard(Arc<ArtifactRemovalRecoveryState>);

impl Drop for ArtifactRemovalRecoveryGuard {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::Release);
    }
}

struct ArtifactRemovalDirectTaskGuard {
    recovery: Arc<ArtifactRemovalRecoveryState>,
    artifacts: Weak<ArtifactService>,
    lifetime: Weak<ArtifactRemovalLifetime>,
    _permit: OwnedSemaphorePermit,
}

impl Drop for ArtifactRemovalDirectTaskGuard {
    fn drop(&mut self) {
        trigger_artifact_removal_recovery(
            Arc::clone(&self.recovery),
            self.artifacts.clone(),
            self.lifetime.clone(),
        );
    }
}

fn trigger_artifact_removal_recovery(
    recovery: Arc<ArtifactRemovalRecoveryState>,
    artifacts: Weak<ArtifactService>,
    lifetime: Weak<ArtifactRemovalLifetime>,
) {
    if lifetime.upgrade().is_none() {
        return;
    }
    recovery.wake.notify_one();
    if recovery
        .running
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    tokio::spawn(run_artifact_removal_recovery(recovery, artifacts, lifetime));
}

async fn run_artifact_removal_recovery(
    recovery: Arc<ArtifactRemovalRecoveryState>,
    artifacts: Weak<ArtifactService>,
    lifetime: Weak<ArtifactRemovalLifetime>,
) {
    let _guard = ArtifactRemovalRecoveryGuard(Arc::clone(&recovery));
    let mut retry_delay = ARTIFACT_REMOVAL_RECOVERY_INITIAL_RETRY;
    loop {
        if lifetime.upgrade().is_none() {
            return;
        }
        let Some(artifacts) = artifacts.upgrade() else {
            return;
        };
        let result = artifacts
            .recover_incomplete_removals(MAX_ARTIFACT_RECOVERY_BATCH)
            .await;
        drop(artifacts);
        if lifetime.upgrade().is_none() {
            return;
        }
        match result {
            Ok(summary) if summary.committed > 0 || summary.truncated => {
                retry_delay = ARTIFACT_REMOVAL_RECOVERY_INITIAL_RETRY;
            }
            Ok(_) => {
                retry_delay = ARTIFACT_REMOVAL_RECOVERY_INITIAL_RETRY;
                tokio::select! {
                    () = recovery.wake.notified() => {}
                    () = tokio::time::sleep(ARTIFACT_REMOVAL_RECOVERY_MAX_RETRY) => {}
                }
            }
            Err(_) => {
                tokio::select! {
                    () = recovery.wake.notified() => {}
                    () = tokio::time::sleep(retry_delay) => {}
                }
                retry_delay = retry_delay
                    .saturating_mul(2)
                    .min(ARTIFACT_REMOVAL_RECOVERY_MAX_RETRY);
            }
        }
    }
}

struct ConversationTaskRegistry {
    slots: Arc<Semaphore>,
    state: AsyncMutex<ConversationTaskRegistryState>,
}

#[derive(Clone)]
enum SuperGrokEnrollmentProjection {
    Idle,
    Starting {
        generation: u64,
    },
    Awaiting {
        generation: u64,
        verification_uri: String,
        user_code: String,
        expires_at_ms: i64,
        cancellation: OAuthCancellation,
    },
    Failed {
        reason_code: &'static str,
    },
}

struct SuperGrokEnrollmentRuntime {
    next_generation: u64,
    projection: SuperGrokEnrollmentProjection,
    changed: Arc<tokio::sync::Notify>,
}

struct ConversationTaskRegistryState {
    next_generation: u64,
    tasks: HashMap<ConversationTurnId, ConversationTaskEntry>,
}

enum ConversationTaskEntry {
    Active {
        generation: u64,
        cancel: oneshot::Sender<()>,
    },
    Quarantined {
        _permit: OwnedSemaphorePermit,
    },
}

struct ConversationTaskRegistration {
    generation: u64,
    cancel: oneshot::Receiver<()>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConversationTaskOwnership {
    Active,
    Quarantined,
}

impl ConversationTaskRegistry {
    fn new(capacity: usize) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(capacity)),
            state: AsyncMutex::new(ConversationTaskRegistryState {
                next_generation: 0,
                tasks: HashMap::with_capacity(capacity),
            }),
        }
    }

    fn try_acquire(&self) -> Result<OwnedSemaphorePermit, ApplicationError> {
        self.slots.clone().try_acquire_owned().map_err(|_| {
            ApplicationError::Unavailable("conversation task capacity is exhausted".into())
        })
    }

    async fn register(&self, turn_id: ConversationTurnId) -> Option<ConversationTaskRegistration> {
        let mut state = self.state.lock().await;
        if state.tasks.contains_key(&turn_id) {
            return None;
        }
        state.next_generation = state.next_generation.wrapping_add(1).max(1);
        let generation = state.next_generation;
        let (sender, receiver) = oneshot::channel();
        state.tasks.insert(
            turn_id,
            ConversationTaskEntry::Active {
                generation,
                cancel: sender,
            },
        );
        Some(ConversationTaskRegistration {
            generation,
            cancel: receiver,
        })
    }

    async fn ownership(&self, turn_id: &ConversationTurnId) -> Option<ConversationTaskOwnership> {
        self.state
            .lock()
            .await
            .tasks
            .get(turn_id)
            .map(|entry| match entry {
                ConversationTaskEntry::Active { .. } => ConversationTaskOwnership::Active,
                ConversationTaskEntry::Quarantined { .. } => ConversationTaskOwnership::Quarantined,
            })
    }

    async fn signal(&self, turn_id: &ConversationTurnId) -> bool {
        let entry = self.state.lock().await.tasks.remove(turn_id);
        match entry {
            Some(ConversationTaskEntry::Active { cancel, .. }) => cancel.send(()).is_ok(),
            Some(ConversationTaskEntry::Quarantined { .. }) | None => false,
        }
    }

    async fn finish(&self, turn_id: &ConversationTurnId, generation: u64) {
        let mut state = self.state.lock().await;
        if matches!(
            state.tasks.get(turn_id),
            Some(ConversationTaskEntry::Active {
                generation: current,
                ..
            }) if *current == generation
        ) {
            state.tasks.remove(turn_id);
        }
    }

    async fn quarantine(
        &self,
        turn_id: &ConversationTurnId,
        generation: u64,
        permit: OwnedSemaphorePermit,
    ) {
        let mut state = self.state.lock().await;
        if matches!(
            state.tasks.get(turn_id),
            Some(ConversationTaskEntry::Active {
                generation: current,
                ..
            }) if *current == generation
        ) {
            state.tasks.insert(
                turn_id.clone(),
                ConversationTaskEntry::Quarantined { _permit: permit },
            );
        }
    }

    /// Releases only fail-closed ownership for a durably terminal turn.
    ///
    /// A stale terminal observation must never evict a newer active generation,
    /// so this deliberately removes only the quarantined variant.
    async fn release_quarantined(&self, turn_id: &ConversationTurnId) {
        let mut state = self.state.lock().await;
        if matches!(
            state.tasks.get(turn_id),
            Some(ConversationTaskEntry::Quarantined { .. })
        ) {
            state.tasks.remove(turn_id);
        }
    }

    #[cfg(test)]
    async fn active_count(&self) -> usize {
        self.state
            .lock()
            .await
            .tasks
            .values()
            .filter(|entry| matches!(entry, ConversationTaskEntry::Active { .. }))
            .count()
    }

    #[cfg(test)]
    async fn is_quarantined(&self, turn_id: &ConversationTurnId) -> bool {
        matches!(
            self.ownership(turn_id).await,
            Some(ConversationTaskOwnership::Quarantined)
        )
    }

    #[cfg(test)]
    fn available_slots(&self) -> usize {
        self.slots.available_permits()
    }
}

/// Request failure that must terminate the untrusted local connection.
#[derive(Debug, Error)]
pub enum HandlerError {
    /// Envelope pairing, freshness, or shape validation failed.
    #[error(transparent)]
    Envelope(#[from] EnvelopeError),
}

fn mutation_key(value: Option<&str>) -> Result<&str, ApplicationError> {
    value.ok_or_else(|| ApplicationError::InvalidInput("idempotency key is required".into()))
}

fn host_policy_command(
    scope: &str,
    key: &str,
    parts: &[String],
) -> Result<MutationCommand, ApplicationError> {
    if scope.is_empty()
        || scope.len() > 64
        || key.is_empty()
        || key.len() > 128
        || key.chars().any(char::is_control)
    {
        return Err(ApplicationError::InvalidInput(
            "Host Tools mutation identity is invalid".into(),
        ));
    }
    let mut hasher = Sha256::new();
    hasher.update(b"grok-host-policy-command-v1\0");
    hasher.update(scope.as_bytes());
    for part in parts {
        hasher.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    Ok(MutationCommand {
        scope: scope.into(),
        key: key.into(),
        fingerprint: hasher.finalize().into(),
    })
}

fn canonical_host_roots(values: &[String]) -> Result<Vec<PathBuf>, ApplicationError> {
    if values.is_empty() || values.len() > MAX_HOST_EXECUTION_ROOTS {
        return Err(ApplicationError::InvalidInput(
            "Host Tools requires between one and eight roots".into(),
        ));
    }
    let mut unique = HashSet::with_capacity(values.len());
    let mut roots = Vec::with_capacity(values.len());
    for value in values {
        let path = PathBuf::from(value);
        if !path.is_absolute() || value.len() > 4096 {
            return Err(ApplicationError::InvalidInput(
                "Host Tools roots must be bounded absolute directories".into(),
            ));
        }
        let canonical = path
            .canonicalize()
            .map_err(|_| ApplicationError::InvalidInput("Host Tools root is unavailable".into()))?;
        if !canonical.is_dir() || !unique.insert(canonical.clone()) {
            return Err(ApplicationError::InvalidInput(
                "Host Tools roots must be unique directories".into(),
            ));
        }
        if host_root_is_daemon_private(&canonical) {
            return Err(ApplicationError::InvalidInput(
                "Grok Desktop private data cannot be a Host Tools root".into(),
            ));
        }
        roots.push(canonical);
    }
    Ok(roots)
}

fn host_root_is_daemon_private(root: &Path) -> bool {
    directories::ProjectDirs::from("net", "Grok Insider", "Grok Desktop")
        .and_then(|directories| directories.data_local_dir().canonicalize().ok())
        .is_some_and(|private| root.starts_with(private))
}

fn host_root_is_broad(root: &Path) -> bool {
    if root.parent().is_none() {
        return true;
    }
    [std::env::var_os("HOME"), std::env::var_os("USERPROFILE")]
        .into_iter()
        .flatten()
        .filter_map(|home| PathBuf::from(home).canonicalize().ok())
        .any(|home| home == root)
}

fn host_execution_policy_to_wire(
    policy: HostExecutionPolicy,
    runtime_prepared: bool,
) -> v1::HostExecutionPolicy {
    let unavailable_reason_code = if runtime_prepared {
        ""
    } else if policy.is_effectively_active() {
        "host_tools_runtime_not_prepared"
    } else if policy.active {
        "host_tools_acknowledgment_outdated"
    } else {
        "host_tools_not_enrolled"
    };
    v1::HostExecutionPolicy {
        revision: policy.revision,
        active: policy.active,
        acknowledgment_version: policy.acknowledgment_version,
        required_acknowledgment_version: HOST_ACKNOWLEDGMENT_VERSION,
        acknowledged_at_unix_ms: policy.acknowledged_at,
        filesystem_read: policy.tool_classes.filesystem_read,
        filesystem_write: policy.tool_classes.filesystem_write,
        process_execute: policy.tool_classes.process_execute,
        path_roots: policy.canonical_roots,
        broad_scope_acknowledged: policy.broad_scope_acknowledged,
        updated_at_unix_ms: policy.updated_at,
        runtime_prepared,
        unavailable_reason_code: unavailable_reason_code.into(),
    }
}

fn agent_runtime_application_error(error: grok_application::AgentRuntimeError) -> ApplicationError {
    match error.kind {
        AgentRuntimeErrorKind::InvalidRequest => ApplicationError::InvalidInput(error.message),
        AgentRuntimeErrorKind::Authentication => ApplicationError::Unauthorized(error.message),
        AgentRuntimeErrorKind::ComponentVerification => ApplicationError::Integrity(error.message),
        AgentRuntimeErrorKind::Cancelled => ApplicationError::Cancelled,
        AgentRuntimeErrorKind::ConfigurationIsolation
        | AgentRuntimeErrorKind::Process
        | AgentRuntimeErrorKind::Protocol
        | AgentRuntimeErrorKind::Permission
        | AgentRuntimeErrorKind::Unavailable => ApplicationError::Unavailable(error.message),
    }
}

fn artifact_task_join_failure() -> ApplicationError {
    ApplicationError::Unavailable("artifact operation task failed".into())
}

fn optional(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
}

fn supergrok_status_wire(
    state: &str,
    verification_uri: &str,
    user_code: &str,
    expires_at_ms: i64,
    credential_generation: u64,
    reason_code: &str,
) -> v1::SuperGrokEnrollmentStatus {
    v1::SuperGrokEnrollmentStatus {
        state: state.into(),
        verification_uri: verification_uri.into(),
        user_code: user_code.into(),
        expires_at_unix_ms: u64::try_from(expires_at_ms).unwrap_or(0),
        credential_generation,
        reason_code: reason_code.into(),
    }
}

const fn supergrok_failure_reason(error: &ApplicationError) -> &'static str {
    match error {
        ApplicationError::Unauthorized(_) => "authorization_rejected",
        ApplicationError::DeadlineExceeded => "authorization_expired",
        ApplicationError::Cancelled => "cancelled",
        ApplicationError::Unavailable(_) => "provider_unavailable",
        ApplicationError::Storage(_) => "vault_write_failed",
        ApplicationError::Integrity(_) => "provider_response_invalid",
        ApplicationError::InvalidInput(_)
        | ApplicationError::NotFound
        | ApplicationError::Conflict
        | ApplicationError::InvalidState(_) => "internal_failure",
    }
}

const fn operation_dispatch_limit(operation: &v1::request::Operation) -> Duration {
    match operation {
        v1::request::Operation::EnrollXaiApiKey(_) => MAX_CREDENTIAL_ENROLLMENT_DURATION,
        v1::request::Operation::StartConversationTurn(_)
        | v1::request::Operation::RetryConversationTurn(_)
        | v1::request::Operation::EditAndBranchConversationTurn(_)
        | v1::request::Operation::RegenerateConversationTurn(_) => {
            MAX_CONVERSATION_DISPATCH_DURATION
        }
        v1::request::Operation::StartHostWork(_) => MAX_HOST_WORK_DISPATCH_DURATION,
        v1::request::Operation::PollRunEvents(_)
        | v1::request::Operation::PollConversationTurnEvents(_) => {
            MAX_RUN_EVENT_POLL_DISPATCH_DURATION
        }
        v1::request::Operation::GetChatModelCatalog(_)
        | v1::request::Operation::SelectChatModel(_) => MAX_CHAT_MODEL_DISPATCH_DURATION,
        _ => MAX_DISPATCH_DURATION,
    }
}

fn validate_event_poll_budget(
    operation: Option<&v1::request::Operation>,
    remaining: Duration,
) -> Result<(), ApplicationError> {
    let (wait_timeout_ms, label) = match operation {
        Some(v1::request::Operation::PollRunEvents(request)) => {
            (request.wait_timeout_ms, "run event")
        }
        Some(v1::request::Operation::PollConversationTurnEvents(request)) => {
            (request.wait_timeout_ms, "conversation event")
        }
        _ => return Ok(()),
    };
    let wait = Duration::from_millis(u64::from(wait_timeout_ms));
    if wait > MAX_RUN_EVENT_POLL_WAIT {
        return Err(ApplicationError::InvalidInput(format!(
            "{label} poll wait must not exceed 20000 milliseconds"
        )));
    }
    if wait.saturating_add(RUN_EVENT_POLL_RESPONSE_RESERVE) >= remaining {
        return Err(ApplicationError::InvalidInput(format!(
            "{label} poll wait must remain below the request deadline"
        )));
    }
    Ok(())
}

fn artifact_operation_minimum_budget(
    operation: Option<&v1::request::Operation>,
) -> Option<Duration> {
    let inner_timeout_ms = match operation {
        Some(v1::request::Operation::ImportArtifact(_)) => ARTIFACT_IMPORT_IO_TIMEOUT_MS,
        Some(v1::request::Operation::OpenArtifact(_)) => ARTIFACT_OPEN_TIMEOUT_MS,
        Some(v1::request::Operation::RemoveArtifact(_)) => ARTIFACT_REMOVAL_IO_TIMEOUT_MS,
        _ => return None,
    };
    Some(Duration::from_millis(inner_timeout_ms).saturating_add(ARTIFACT_TERMINALIZATION_RESERVE))
}

fn validate_artifact_operation_budget(
    operation: Option<&v1::request::Operation>,
    remaining: Duration,
) -> Result<(), ApplicationError> {
    if artifact_operation_minimum_budget(operation).is_some_and(|minimum| remaining < minimum) {
        return Err(ApplicationError::DeadlineExceeded);
    }
    Ok(())
}

fn automation_policies(
    missed: i32,
    overlap: i32,
) -> Result<(MissedRunPolicy, OverlapPolicy), ApplicationError> {
    Ok((
        missed_run_policy_from_wire(missed)
            .map_err(|error| ApplicationError::InvalidInput(error.to_string()))?,
        overlap_policy_from_wire(overlap)
            .map_err(|error| ApplicationError::InvalidInput(error.to_string()))?,
    ))
}

/// Stable reason why a configured official Grok runtime could not be retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRuntimeUnavailableReason {
    /// Trusted component-manager configuration was absent or incomplete.
    InvalidConfiguration,
    /// Runtime construction failed after configuration validation.
    Runtime(AgentRuntimeErrorKind),
}

/// Daemon-owned automation scheduler kernel lifecycle.
///
/// Every state explicitly keeps occurrence execution disabled. The lifecycle is
/// health-only and grants no renderer operation or scheduling authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationSchedulerLifecycle {
    /// The durable journal service exists and no startup recovery is outstanding.
    KernelInitializedExecutionDisabled,
    /// Journal is live and occurrence dispatch / enabled definitions are armed.
    KernelInitializedExecutionEnabled,
    /// The durable journal exists but bounded recovery must finish before kernel use.
    RecoveryPendingExecutionDisabled,
    /// The journal service is absent or failed closed.
    DegradedExecutionDisabled,
}

impl AgentRuntimeUnavailableReason {
    /// Returns the stable IPC and diagnostic reason code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "configuration_invalid",
            Self::Runtime(kind) => runtime_error_code(kind),
        }
    }
}

/// Trusted daemon application composition exposed through local IPC.
pub struct Daemon {
    runs: Arc<RunService>,
    approvals: Arc<ApprovalService>,
    credentials: Arc<CredentialService>,
    clock: Arc<dyn grok_application::Clock>,
    startup_nonce: [u8; 32],
    instance_id: String,
    agent_runtime: Option<Arc<dyn AgentRuntime>>,
    agent_runtime_failure: Option<AgentRuntimeUnavailableReason>,
    host_work_runtime: Option<Arc<HostWorkRuntime>>,
    host_work_service: Option<Arc<HostWorkService>>,
    workspace: Option<Arc<WorkspaceService>>,
    automation_scheduler: Option<Arc<AutomationSchedulerService>>,
    automation_scheduler_lifecycle: AutomationSchedulerLifecycle,
    artifacts: Option<Arc<ArtifactService>>,
    artifact_content_available: bool,
    artifact_open_available: bool,
    artifact_removal_direct_slots: Arc<Semaphore>,
    artifact_removal_recovery: Arc<ArtifactRemovalRecoveryState>,
    artifact_removal_lifetime: Arc<ArtifactRemovalLifetime>,
    conversation: Option<Arc<ConversationService>>,
    conversation_tasks: Arc<ConversationTaskRegistry>,
    credential_enrollment: Option<Arc<CredentialEnrollmentService>>,
    desktop_preferences: Option<Arc<DesktopPreferencesService>>,
    host_execution_policy: Option<Arc<dyn HostExecutionPolicyStore>>,
    chat_models: Option<Arc<ChatModelService>>,
    runtime_capability_facts: CapabilityFacts,
    grok_build_auth: Option<Arc<GrokBuildAuthService>>,
    isolation_runtime: Option<Arc<IsolationRuntime>>,
    managed_integrations: Option<Arc<crate::ManagedIntegrationService>>,
    supergrok_enrollment: Option<Arc<SuperGrokEnrollmentService>>,
    chat_rail: Option<Arc<grok_application::ChatRailSelection>>,
    supergrok_enrollment_runtime: Arc<AsyncMutex<SuperGrokEnrollmentRuntime>>,
    supergrok_lifetime_cancellation: OAuthCancellation,
}

impl Daemon {
    /// Creates a daemon handler paired to one Electron main-process nonce.
    #[must_use]
    pub fn new(
        runs: Arc<RunService>,
        approvals: Arc<ApprovalService>,
        credentials: Arc<CredentialService>,
        clock: Arc<dyn grok_application::Clock>,
        startup_nonce: [u8; 32],
        instance_id: String,
    ) -> Self {
        let artifact_removal_recovery = Arc::new(ArtifactRemovalRecoveryState::default());
        let artifact_removal_lifetime = Arc::new(ArtifactRemovalLifetime {
            recovery: Arc::downgrade(&artifact_removal_recovery),
        });
        Self {
            runs,
            approvals,
            credentials,
            clock,
            startup_nonce,
            instance_id,
            agent_runtime: None,
            agent_runtime_failure: None,
            host_work_runtime: None,
            host_work_service: None,
            workspace: None,
            automation_scheduler: None,
            automation_scheduler_lifecycle: AutomationSchedulerLifecycle::DegradedExecutionDisabled,
            artifacts: None,
            artifact_content_available: false,
            artifact_open_available: false,
            artifact_removal_direct_slots: Arc::new(Semaphore::new(1)),
            artifact_removal_recovery,
            artifact_removal_lifetime,
            conversation: None,
            conversation_tasks: Arc::new(ConversationTaskRegistry::new(MAX_CONVERSATION_TASKS)),
            credential_enrollment: None,
            desktop_preferences: None,
            host_execution_policy: None,
            chat_models: None,
            runtime_capability_facts: CapabilityFacts::default(),
            grok_build_auth: None,
            isolation_runtime: None,
            managed_integrations: None,
            supergrok_enrollment: None,
            chat_rail: None,
            supergrok_enrollment_runtime: Arc::new(AsyncMutex::new(SuperGrokEnrollmentRuntime {
                next_generation: 0,
                projection: SuperGrokEnrollmentProjection::Idle,
                changed: Arc::new(tokio::sync::Notify::new()),
            })),
            supergrok_lifetime_cancellation: OAuthCancellation::default(),
        }
    }

    /// Attaches daemon-owned `SuperGrok` OAuth enrollment and vault persistence.
    #[must_use]
    pub fn with_supergrok_enrollment(
        mut self,
        service: Arc<SuperGrokEnrollmentService>,
        chat_rail: Arc<grok_application::ChatRailSelection>,
    ) -> Self {
        self.supergrok_enrollment = Some(service);
        self.chat_rail = Some(chat_rail);
        self
    }

    /// Attaches a live isolation probe + privileged guest-health gateway.
    #[must_use]
    pub fn with_isolation_runtime(mut self, runtime: Arc<IsolationRuntime>) -> Self {
        self.isolation_runtime = Some(runtime);
        self
    }

    /// Attaches the signed managed-integration lifecycle service (Wisp).
    #[must_use]
    pub fn with_managed_integrations(
        mut self,
        service: Arc<crate::ManagedIntegrationService>,
    ) -> Self {
        self.managed_integrations = Some(service);
        self
    }

    /// Adds canonical durable workspace use cases to the daemon composition.
    #[must_use]
    pub fn with_workspace(mut self, workspace: Arc<WorkspaceService>) -> Self {
        self.workspace = Some(workspace);
        self
    }

    /// Adds the journal-only automation scheduler kernel and its disabled lifecycle state.
    ///
    /// This does not start a timer, recover a claim, create a Run, or enable
    /// renderer automation execution.
    #[must_use]
    pub fn with_automation_scheduler(
        mut self,
        scheduler: Arc<AutomationSchedulerService>,
        lifecycle: AutomationSchedulerLifecycle,
    ) -> Self {
        self.automation_scheduler = Some(scheduler);
        self.automation_scheduler_lifecycle = lifecycle;
        self
    }

    /// Adds canonical artifact reads plus the qualified platform import/open boundary.
    #[must_use]
    pub fn with_artifacts(
        mut self,
        artifacts: Arc<ArtifactService>,
        content_available: bool,
        open_available: bool,
    ) -> Self {
        self.artifacts = Some(artifacts);
        self.artifact_content_available = content_available;
        self.artifact_open_available = open_available;
        self
    }

    /// Adds official xAI BYOK conversation execution to the daemon composition.
    #[must_use]
    pub fn with_conversation(mut self, conversation: Arc<ConversationService>) -> Self {
        self.conversation = Some(conversation);
        self
    }

    /// Adds the native, renderer-free xAI credential-entry boundary.
    #[must_use]
    pub fn with_credential_enrollment(
        mut self,
        enrollment: Arc<CredentialEnrollmentService>,
    ) -> Self {
        self.credential_enrollment = Some(enrollment);
        self
    }

    /// Adds daemon-owned process-wide desktop behavior preferences.
    #[must_use]
    pub fn with_desktop_preferences(
        mut self,
        desktop_preferences: Arc<DesktopPreferencesService>,
    ) -> Self {
        self.desktop_preferences = Some(desktop_preferences);
        self
    }

    /// Adds the durable daemon-owned Host Tools enrollment store.
    #[must_use]
    pub fn with_host_execution_policy(mut self, store: Arc<dyn HostExecutionPolicyStore>) -> Self {
        self.host_execution_policy = Some(store);
        self
    }

    /// Adds live official xAI discovery and the durable default Chat model policy.
    #[must_use]
    pub fn with_chat_models(mut self, chat_models: Arc<ChatModelService>) -> Self {
        self.chat_models = Some(chat_models);
        self
    }

    /// Adds daemon-observed runtime facts; credential presence is always derived from the vault.
    #[must_use]
    pub const fn with_runtime_capability_facts(mut self, facts: CapabilityFacts) -> Self {
        self.runtime_capability_facts = facts;
        self
    }

    /// Retains an initialized official Grok runtime for live health probing.
    #[must_use]
    pub fn with_agent_runtime(mut self, runtime: Arc<dyn AgentRuntime>) -> Self {
        self.grok_build_auth = Some(Arc::new(GrokBuildAuthService::new(
            Arc::clone(&runtime),
            Arc::clone(&self.clock),
        )));
        self.agent_runtime = Some(runtime);
        self.agent_runtime_failure = None;
        self
    }

    /// Retains the role-switching official runtime used by Host Work.
    #[must_use]
    pub fn with_host_work_runtime(mut self, runtime: Arc<HostWorkRuntime>) -> Self {
        self.host_work_runtime = Some(runtime.clone());
        self.with_agent_runtime(runtime)
    }

    /// Adds the productive Host Work orchestration service.
    #[must_use]
    pub fn with_host_work_service(mut self, service: Arc<HostWorkService>) -> Self {
        self.host_work_service = Some(service);
        self
    }

    /// Records a configured runtime that failed before it could be retained.
    #[must_use]
    pub fn with_unavailable_agent_runtime(mut self, reason: AgentRuntimeUnavailableReason) -> Self {
        self.agent_runtime = None;
        self.agent_runtime_failure = Some(reason);
        self.grok_build_auth = None;
        self
    }

    /// Returns the nonce that the spawning process must attach to every frame.
    #[must_use]
    pub const fn startup_nonce(&self) -> &[u8; 32] {
        &self.startup_nonce
    }

    /// Validates and dispatches one request envelope.
    ///
    /// # Errors
    ///
    /// Returns [`HandlerError`] when envelope pairing, freshness, or shape
    /// validation fails. Application failures are encoded as protocol responses.
    pub async fn handle(&self, envelope: v1::Envelope) -> Result<v1::Envelope, HandlerError> {
        self.handle_with_dispatch_limit(envelope, None).await
    }

    async fn handle_with_dispatch_limit(
        &self,
        envelope: v1::Envelope,
        dispatch_limit_override: Option<Duration>,
    ) -> Result<v1::Envelope, HandlerError> {
        let metadata = validate_envelope(&envelope, &self.startup_nonce, self.clock.now())?;
        let Some(v1::envelope::Payload::Request(request)) = envelope.payload else {
            unreachable!("validated request payload")
        };
        let dispatch_limit = dispatch_limit_override.unwrap_or_else(|| {
            request
                .operation
                .as_ref()
                .map_or(MAX_DISPATCH_DURATION, operation_dispatch_limit)
        });
        let remaining =
            Duration::from_millis(metadata.deadline_unix_ms.saturating_sub(self.clock.now()));
        let outer_budget = remaining.min(dispatch_limit);
        let conversation_budget = outer_budget.saturating_sub(CONVERSATION_CLEANUP_GRACE);
        let validation = validate_event_poll_budget(request.operation.as_ref(), remaining)
            .and_then(|()| {
                validate_artifact_operation_budget(request.operation.as_ref(), remaining)
            });
        let result = match validation {
            Ok(()) => match tokio::time::timeout(
                outer_budget,
                Box::pin(self.dispatch(
                    request.operation,
                    metadata.idempotency_key.as_deref(),
                    conversation_budget,
                )),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => deadline_exceeded_result(),
            },
            Err(error) => error_result(&error),
        };
        let mut response = v1::Envelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: metadata.request_id,
            startup_nonce: self.startup_nonce.to_vec(),
            deadline_unix_ms: 0,
            idempotency_key: metadata.idempotency_key.unwrap_or_default(),
            payload: Some(v1::envelope::Payload::Response(v1::Response {
                result: Some(result),
            })),
        };
        bound_response_to_frame(&mut response);
        Ok(response)
    }

    #[allow(clippy::too_many_lines)]
    async fn dispatch(
        &self,
        operation: Option<v1::request::Operation>,
        idempotency_key: Option<&str>,
        conversation_budget: Duration,
    ) -> v1::response::Result {
        let result = match operation {
            Some(v1::request::Operation::Health(_)) => {
                Ok(v1::response::Result::Health(v1::HealthResponse {
                    service_version: env!("CARGO_PKG_VERSION").into(),
                    protocol_version: PROTOCOL_VERSION,
                    instance_id: self.instance_id.clone(),
                    agent_runtime: Some(self.agent_runtime_health().await),
                    automation_scheduler: self.automation_scheduler_health() as i32,
                }))
            }
            Some(v1::request::Operation::ResolveCapabilities(_)) => {
                self.resolve_capabilities().await
            }
            Some(v1::request::Operation::EventsSince(request)) => self.events_since(request).await,
            Some(v1::request::Operation::PollRunEvents(request)) => {
                self.poll_run_events(request).await
            }
            Some(v1::request::Operation::DecideApproval(request)) => {
                self.decide_approval(request, idempotency_key).await
            }
            Some(v1::request::Operation::CreateProject(request)) => {
                self.create_project(request, idempotency_key).await
            }
            Some(v1::request::Operation::UpdateProject(request)) => {
                self.update_project(request, idempotency_key).await
            }
            Some(v1::request::Operation::ArchiveProject(request)) => {
                self.archive_project(request, idempotency_key).await
            }
            Some(v1::request::Operation::GetProject(request)) => self.get_project(request).await,
            Some(v1::request::Operation::ListProjects(request)) => {
                self.list_projects(request).await
            }
            Some(v1::request::Operation::CreateThread(request)) => {
                self.create_thread(request, idempotency_key).await
            }
            Some(v1::request::Operation::UpdateThread(request)) => {
                self.update_thread(request, idempotency_key).await
            }
            Some(v1::request::Operation::ArchiveThread(request)) => {
                self.archive_thread(request, idempotency_key).await
            }
            Some(v1::request::Operation::GetThread(request)) => self.get_thread(request).await,
            Some(v1::request::Operation::ListThreads(request)) => self.list_threads(request).await,
            Some(v1::request::Operation::GetMessage(request)) => self.get_message(request).await,
            Some(v1::request::Operation::ListMessages(request)) => {
                self.list_messages(request).await
            }
            Some(v1::request::Operation::GetArtifact(request)) => self.get_artifact(request).await,
            Some(v1::request::Operation::ListArtifacts(request)) => {
                self.list_artifacts(request).await
            }
            Some(v1::request::Operation::ImportArtifact(request)) => {
                self.import_artifact(request, idempotency_key).await
            }
            Some(v1::request::Operation::OpenArtifact(request)) => {
                self.open_artifact(request, idempotency_key).await
            }
            Some(v1::request::Operation::RemoveArtifact(request)) => {
                self.remove_artifact(request, idempotency_key).await
            }
            Some(v1::request::Operation::CreateAutomation(request)) => {
                self.create_automation(request, idempotency_key).await
            }
            Some(v1::request::Operation::UpdateAutomation(request)) => {
                self.update_automation(request, idempotency_key).await
            }
            Some(v1::request::Operation::ArchiveAutomation(request)) => {
                self.archive_automation(request, idempotency_key).await
            }
            Some(v1::request::Operation::GetAutomation(request)) => {
                self.get_automation(request).await
            }
            Some(v1::request::Operation::ListAutomations(request)) => {
                self.list_automations(request).await
            }
            Some(v1::request::Operation::ListAutomationHistory(request)) => {
                self.list_automation_history(request).await
            }
            Some(v1::request::Operation::SearchWorkspace(request)) => {
                self.search_workspace(request).await
            }
            Some(v1::request::Operation::GetAccountState(_)) => self.account_state().await,
            Some(v1::request::Operation::EnrollXaiApiKey(request)) => {
                self.enroll_xai_api_key(request, idempotency_key).await
            }
            Some(v1::request::Operation::DeleteXaiApiKey(_)) => {
                self.delete_xai_api_key(idempotency_key).await
            }
            Some(v1::request::Operation::StartConversationTurn(request)) => {
                self.start_conversation_turn(request, idempotency_key, conversation_budget)
                    .await
            }
            Some(v1::request::Operation::RetryConversationTurn(request)) => {
                self.retry_conversation_turn(request, idempotency_key, conversation_budget)
                    .await
            }
            Some(v1::request::Operation::BranchConversationThread(request)) => {
                self.branch_conversation_thread(request, idempotency_key)
                    .await
            }
            Some(v1::request::Operation::EditAndBranchConversationTurn(request)) => {
                self.edit_and_branch_conversation_turn(
                    request,
                    idempotency_key,
                    conversation_budget,
                )
                .await
            }
            Some(v1::request::Operation::RegenerateConversationTurn(request)) => {
                self.regenerate_conversation_turn(request, idempotency_key, conversation_budget)
                    .await
            }
            Some(v1::request::Operation::GetConversationForkMetadata(request)) => {
                self.get_conversation_fork_metadata(request).await
            }
            Some(v1::request::Operation::AcknowledgeConversationForkDelivery(request)) => {
                self.acknowledge_conversation_fork_delivery(request, idempotency_key)
                    .await
            }
            Some(v1::request::Operation::CancelConversationTurn(request)) => {
                self.cancel_conversation_turn(request, idempotency_key)
                    .await
            }
            Some(v1::request::Operation::PollConversationTurnEvents(request)) => {
                self.poll_conversation_turn_events(request).await
            }
            Some(v1::request::Operation::ListConversationTurns(request)) => {
                self.list_conversation_turns(request).await
            }
            Some(v1::request::Operation::GetDesktopPreferences(_)) => {
                self.get_desktop_preferences().await
            }
            Some(v1::request::Operation::UpdateDesktopPreferences(request)) => {
                self.update_desktop_preferences(request, idempotency_key)
                    .await
            }
            Some(v1::request::Operation::GetChatModelCatalog(_)) => {
                self.get_chat_model_catalog().await
            }
            Some(v1::request::Operation::GetUsageSummary(request)) => {
                self.get_usage_summary(request).await
            }
            Some(v1::request::Operation::GetHostExecutionPolicy(_)) => {
                self.get_host_execution_policy().await
            }
            Some(v1::request::Operation::EnrollHostExecution(request)) => {
                self.enroll_host_execution(request, idempotency_key).await
            }
            Some(v1::request::Operation::RevokeHostExecution(request)) => {
                self.revoke_host_execution(request, idempotency_key).await
            }
            Some(v1::request::Operation::PrepareHostWorkRuntime(_)) => {
                self.prepare_host_work_runtime().await
            }
            Some(v1::request::Operation::DeactivateHostWorkRuntime(_)) => {
                self.deactivate_host_work_runtime().await
            }
            Some(v1::request::Operation::StartHostWork(request)) => {
                self.start_host_work(request, idempotency_key).await
            }
            Some(v1::request::Operation::CancelHostWork(request)) => {
                self.cancel_host_work(request, idempotency_key).await
            }
            Some(v1::request::Operation::ListHostWorkRuns(request)) => {
                self.list_host_work_runs(request).await
            }
            Some(v1::request::Operation::SelectChatModel(request)) => {
                self.select_chat_model(request, idempotency_key).await
            }
            Some(v1::request::Operation::StartGrokBuildAuth(_)) => {
                self.start_grok_build_auth().await
            }
            Some(v1::request::Operation::GetGrokBuildAuthStatus(_)) => {
                self.get_grok_build_auth_status().await
            }
            Some(v1::request::Operation::GetManagedIntegration(request)) => {
                self.get_managed_integration(request).await
            }
            Some(v1::request::Operation::ChangeManagedIntegration(request)) => {
                self.change_managed_integration(request).await
            }
            Some(v1::request::Operation::BeginSupergrokDeviceEnrollment(_)) => {
                self.begin_supergrok_device_enrollment().await
            }
            Some(v1::request::Operation::GetSupergrokEnrollmentStatus(_)) => {
                self.get_supergrok_enrollment_status().await
            }
            Some(v1::request::Operation::CancelSupergrokEnrollment(_)) => {
                self.cancel_supergrok_enrollment().await
            }
            Some(v1::request::Operation::DisconnectSupergrok(_)) => {
                self.disconnect_supergrok().await
            }
            None => Err(ApplicationError::InvalidInput(
                "request operation is required".into(),
            )),
        };
        result.unwrap_or_else(|error| error_result(&error))
    }

    async fn agent_runtime_health(&self) -> v1::AgentRuntimeHealth {
        let Some(runtime) = &self.agent_runtime else {
            return v1::AgentRuntimeHealth {
                configured: self.agent_runtime_failure.is_some(),
                healthy: false,
                protocol_version: 0,
                agent_name: String::new(),
                agent_version: String::new(),
                auth_methods: Vec::new(),
                capabilities: None,
                reason_code: self
                    .agent_runtime_failure
                    .map_or("not_configured", |reason| reason.code())
                    .into(),
            };
        };
        match runtime.probe().await {
            Ok(probe) => runtime_probe_to_wire(probe),
            Err(error) => v1::AgentRuntimeHealth {
                configured: true,
                healthy: false,
                protocol_version: 0,
                agent_name: String::new(),
                agent_version: String::new(),
                auth_methods: Vec::new(),
                capabilities: None,
                reason_code: runtime_error_code(error.kind).into(),
            },
        }
    }

    const fn automation_scheduler_health(&self) -> v1::AutomationSchedulerHealth {
        if self.automation_scheduler.is_none() {
            return v1::AutomationSchedulerHealth::DegradedExecutionDisabled;
        }
        match self.automation_scheduler_lifecycle {
            // Epoch 20 preserves the historical lifecycle variant for internal
            // compatibility, but never advertises execution readiness.
            AutomationSchedulerLifecycle::KernelInitializedExecutionDisabled
            | AutomationSchedulerLifecycle::KernelInitializedExecutionEnabled => {
                v1::AutomationSchedulerHealth::KernelInitializedExecutionDisabled
            }
            AutomationSchedulerLifecycle::RecoveryPendingExecutionDisabled => {
                v1::AutomationSchedulerHealth::RecoveryPendingExecutionDisabled
            }
            AutomationSchedulerLifecycle::DegradedExecutionDisabled => {
                v1::AutomationSchedulerHealth::DegradedExecutionDisabled
            }
        }
    }

    #[allow(clippy::unused_self)]
    const fn automation_execution_armed(&self) -> bool {
        false
    }

    async fn capability_facts(&self) -> Result<CapabilityFacts, ApplicationError> {
        let mut facts = self.runtime_capability_facts;
        let account = self.credentials.account_state()?;
        facts.xai_api_key_configured = account.xai_api_key_configured;
        facts.supergrok_api_connected = self.supergrok_enrollment.as_ref().is_some_and(|service| {
            service
                .connection_status()
                .is_ok_and(|status| status.is_some())
        });
        facts.xai_capabilities_resolved =
            if account.xai_api_key_configured || facts.supergrok_api_connected {
                match &self.chat_models {
                    Some(chat_models) => chat_models
                        .catalog()
                        .await
                        .is_ok_and(|catalog| catalog.selected_model_ready),
                    None => account.xai_capabilities_resolved,
                }
            } else {
                false
            };
        if let Some(auth) = &self.grok_build_auth {
            facts.subscription_authenticated = auth.is_authenticated().await;
        }
        if let Some(store) = &self.host_execution_policy {
            let policy = store.get_host_execution_policy().await?;
            facts.host_policy_effective = policy.is_effectively_active();
            facts.host_process_execute_enabled =
                facts.host_policy_effective && policy.tool_classes.process_execute;
            facts.host_work_runtime_ready = if facts.host_policy_effective {
                match &self.host_work_runtime {
                    Some(runtime) => runtime.is_ready().await,
                    None => false,
                }
            } else {
                false
            };
        }
        if let Some(isolation) = &self.isolation_runtime {
            // Refresh is best-effort; probe unavailability clears readiness.
            let key = format!(
                "isolation-refresh-{:016x}",
                self.clock.now().wrapping_mul(0x9E37_79B9_7F4A_7C15)
            );
            if let Ok(live) = isolation.refresh(&key).await {
                facts.isolation_broker_qualified = live.broker_qualified;
                facts.strong_isolation_ready = live.strong_isolation_ready;
            } else {
                facts.isolation_broker_qualified = false;
                facts.strong_isolation_ready = false;
            }
        }
        facts.automation_scheduler_ready = self.automation_execution_armed();
        Ok(facts)
    }

    async fn resolve_capabilities(&self) -> Result<v1::response::Result, ApplicationError> {
        let facts = self.capability_facts().await?;
        let work_execution_backend = match CapabilityResolver::resolve_work_backend(facts) {
            None => v1::WorkExecutionBackend::Unspecified,
            Some(grok_domain::WorkExecutionBackend::HostDirect) => {
                v1::WorkExecutionBackend::HostDirect
            }
            Some(grok_domain::WorkExecutionBackend::IsolatedGuest) => {
                v1::WorkExecutionBackend::IsolatedGuest
            }
        };
        let statuses = CapabilityResolver::resolve(facts)
            .into_iter()
            .map(capability_to_wire)
            .collect();
        let host_bound_run_active = self.runs.has_active_host_work().await?;
        Ok(v1::response::Result::Capabilities(
            v1::ResolveCapabilitiesResponse {
                statuses,
                work_execution_backend: work_execution_backend as i32,
                host_work_runtime_ready: facts.host_work_runtime_ready,
                host_bound_run_active,
            },
        ))
    }

    async fn account_state(&self) -> Result<v1::response::Result, ApplicationError> {
        let mut state = self.credentials.account_state()?;
        if let Some(auth) = &self.grok_build_auth {
            state.grok_build_authenticated = auth.is_authenticated().await;
        }
        Ok(v1::response::Result::AccountState(account_state_to_wire(
            state,
        )))
    }

    async fn start_grok_build_auth(&self) -> Result<v1::response::Result, ApplicationError> {
        let Some(auth) = &self.grok_build_auth else {
            return Err(ApplicationError::Unavailable(
                "Grok Build runtime is not configured".into(),
            ));
        };
        let status = auth.authenticate().await?;
        Ok(v1::response::Result::GrokBuildAuthStatus(
            grok_build_auth_status_to_wire(status, auth.is_authenticated().await),
        ))
    }

    async fn get_grok_build_auth_status(&self) -> Result<v1::response::Result, ApplicationError> {
        let Some(auth) = &self.grok_build_auth else {
            return Ok(v1::response::Result::GrokBuildAuthStatus(
                v1::GrokBuildAuthStatus {
                    state: "not_authenticated".into(),
                    authenticated: false,
                },
            ));
        };
        let status = auth.status().await;
        Ok(v1::response::Result::GrokBuildAuthStatus(
            grok_build_auth_status_to_wire(status, auth.is_authenticated().await),
        ))
    }

    async fn get_managed_integration(
        &self,
        request: v1::GetManagedIntegrationRequest,
    ) -> Result<v1::response::Result, ApplicationError> {
        let Some(service) = &self.managed_integrations else {
            return Err(ApplicationError::Unavailable(
                "managed integration lifecycle is not configured".into(),
            ));
        };
        if request.integration_id != "desktop.grok.wisp" && request.integration_id != "wisp" {
            return Err(ApplicationError::NotFound);
        }
        let id = "desktop.grok.wisp";
        let record = service
            .get_durable(id)
            .await
            .map_err(ApplicationError::from)?;
        Ok(v1::response::Result::ManagedIntegration(
            // Epoch 20 preserves the record projection but does not attest the
            // legacy bundle verifier's result as trusted lifecycle readiness.
            managed_integration_to_wire(&record, false),
        ))
    }

    async fn begin_supergrok_device_enrollment(
        &self,
    ) -> Result<v1::response::Result, ApplicationError> {
        let service = self.supergrok_enrollment.as_ref().ok_or_else(|| {
            ApplicationError::Unavailable("SuperGrok enrollment is not configured".into())
        })?;
        let generation = {
            let mut runtime = self.supergrok_enrollment_runtime.lock().await;
            if matches!(
                runtime.projection,
                SuperGrokEnrollmentProjection::Starting { .. }
                    | SuperGrokEnrollmentProjection::Awaiting { .. }
            ) {
                return Err(ApplicationError::Conflict);
            }
            runtime.next_generation = runtime.next_generation.checked_add(1).ok_or_else(|| {
                ApplicationError::InvalidState("OAuth enrollment generation exhausted".into())
            })?;
            let generation = runtime.next_generation;
            runtime.projection = SuperGrokEnrollmentProjection::Starting { generation };
            generation
        };
        let observed_at = i64::try_from(self.clock.now()).map_err(|_| {
            ApplicationError::InvalidState("OAuth enrollment clock is out of range".into())
        })?;
        let authorization = match service.begin_device(observed_at).await {
            Ok(authorization) => authorization,
            Err(error) => {
                let mut runtime = self.supergrok_enrollment_runtime.lock().await;
                if matches!(runtime.projection, SuperGrokEnrollmentProjection::Starting { generation: current } if current == generation)
                {
                    runtime.projection = SuperGrokEnrollmentProjection::Failed {
                        reason_code: supergrok_failure_reason(&error),
                    };
                }
                return Err(error);
            }
        };
        let verification_uri = authorization.verification_uri.clone();
        let user_code = authorization.user_code.clone();
        let expires_at_ms = authorization.expires_at_ms;
        let cancellation = OAuthCancellation::default();
        {
            let mut runtime = self.supergrok_enrollment_runtime.lock().await;
            if !matches!(runtime.projection, SuperGrokEnrollmentProjection::Starting { generation: current } if current == generation)
            {
                return Err(ApplicationError::Cancelled);
            }
            runtime.projection = SuperGrokEnrollmentProjection::Awaiting {
                generation,
                verification_uri: verification_uri.clone(),
                user_code: user_code.clone(),
                expires_at_ms,
                cancellation: cancellation.clone(),
            };
        }
        let service = Arc::clone(service);
        let runtime = Arc::clone(&self.supergrok_enrollment_runtime);
        let chat_rail = self.chat_rail.clone();
        let lifetime_cancellation = self.supergrok_lifetime_cancellation.clone();
        tokio::spawn(async move {
            let result = tokio::select! {
                result = service.complete_device(&authorization, &cancellation) => result,
                () = lifetime_cancellation.cancelled() => Err(ApplicationError::Cancelled),
            };
            let mut runtime = runtime.lock().await;
            let current = matches!(
                runtime.projection,
                SuperGrokEnrollmentProjection::Awaiting { generation: current, .. }
                    if current == generation
            );
            if !current {
                return;
            }
            runtime.projection = match result {
                Ok(_) => {
                    if let Some(chat_rail) = chat_rail {
                        chat_rail.set(grok_domain::ChatRail::SuperGrokApi);
                    }
                    SuperGrokEnrollmentProjection::Idle
                }
                Err(ApplicationError::Cancelled) => SuperGrokEnrollmentProjection::Idle,
                Err(error) => SuperGrokEnrollmentProjection::Failed {
                    reason_code: supergrok_failure_reason(&error),
                },
            };
            runtime.changed.notify_waiters();
        });
        Ok(v1::response::Result::SupergrokEnrollmentStatus(
            supergrok_status_wire(
                "awaiting_user",
                &verification_uri,
                &user_code,
                expires_at_ms,
                0,
                "",
            ),
        ))
    }

    async fn get_supergrok_enrollment_status(
        &self,
    ) -> Result<v1::response::Result, ApplicationError> {
        let service = self.supergrok_enrollment.as_ref().ok_or_else(|| {
            ApplicationError::Unavailable("SuperGrok enrollment is not configured".into())
        })?;
        let projection = self
            .supergrok_enrollment_runtime
            .lock()
            .await
            .projection
            .clone();
        let wire = match projection {
            SuperGrokEnrollmentProjection::Awaiting {
                verification_uri,
                user_code,
                expires_at_ms,
                ..
            } => supergrok_status_wire(
                "awaiting_user",
                &verification_uri,
                &user_code,
                expires_at_ms,
                0,
                "",
            ),
            SuperGrokEnrollmentProjection::Starting { .. } => {
                supergrok_status_wire("starting", "", "", 0, 0, "")
            }
            SuperGrokEnrollmentProjection::Failed { reason_code, .. } => {
                supergrok_status_wire("failed", "", "", 0, 0, reason_code)
            }
            SuperGrokEnrollmentProjection::Idle => match service.connection_status()? {
                Some(SuperGrokEnrollmentStatus::Connected {
                    expires_at_ms,
                    generation,
                }) => supergrok_status_wire("connected", "", "", expires_at_ms, generation, ""),
                Some(SuperGrokEnrollmentStatus::AwaitingUser { .. }) => {
                    return Err(ApplicationError::Integrity(
                        "persisted OAuth state has an invalid projection".into(),
                    ));
                }
                None => supergrok_status_wire("disconnected", "", "", 0, 0, ""),
            },
        };
        Ok(v1::response::Result::SupergrokEnrollmentStatus(wire))
    }

    async fn cancel_supergrok_enrollment(&self) -> Result<v1::response::Result, ApplicationError> {
        let wait = {
            let runtime = self.supergrok_enrollment_runtime.lock().await;
            if let SuperGrokEnrollmentProjection::Awaiting {
                generation,
                cancellation,
                ..
            } = &runtime.projection
            {
                cancellation.cancel();
                Some((*generation, Arc::clone(&runtime.changed)))
            } else {
                None
            }
        };
        if let Some((generation, changed)) = wait {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    let notified = changed.notified();
                    let still_active = matches!(
                        self.supergrok_enrollment_runtime.lock().await.projection,
                        SuperGrokEnrollmentProjection::Awaiting { generation: current, .. }
                            if current == generation
                    );
                    if !still_active {
                        break;
                    }
                    notified.await;
                }
            })
            .await
            .map_err(|_| ApplicationError::DeadlineExceeded)?;
        }
        self.get_supergrok_enrollment_status().await
    }

    async fn disconnect_supergrok(&self) -> Result<v1::response::Result, ApplicationError> {
        let service = self.supergrok_enrollment.as_ref().ok_or_else(|| {
            ApplicationError::Unavailable("SuperGrok enrollment is not configured".into())
        })?;
        let mut runtime = self.supergrok_enrollment_runtime.lock().await;
        if let SuperGrokEnrollmentProjection::Awaiting { cancellation, .. } = &runtime.projection {
            cancellation.cancel();
        }
        service.disconnect().await?;
        if let Some(chat_rail) = &self.chat_rail {
            chat_rail.set(grok_domain::ChatRail::XaiApiKey);
        }
        runtime.projection = SuperGrokEnrollmentProjection::Idle;
        Ok(v1::response::Result::SupergrokEnrollmentStatus(
            supergrok_status_wire("disconnected", "", "", 0, 0, ""),
        ))
    }

    #[allow(clippy::unused_async)]
    async fn change_managed_integration(
        &self,
        request: v1::ChangeManagedIntegrationRequest,
    ) -> Result<v1::response::Result, ApplicationError> {
        if request.integration_id != "desktop.grok.wisp" && request.integration_id != "wisp" {
            return Err(ApplicationError::NotFound);
        }
        Err(ApplicationError::Unavailable(
            "managed integration changes are unavailable until durable trust verification is qualified"
                .into(),
        ))
    }

    async fn enroll_xai_api_key(
        &self,
        request: v1::EnrollXaiApiKeyRequest,
        key: Option<&str>,
    ) -> Result<v1::response::Result, ApplicationError> {
        let key = mutation_key(key)?;
        let enrollment = self.credential_enrollment.as_ref().ok_or_else(|| {
            ApplicationError::Unavailable("native credential enrollment is not configured".into())
        })?;
        let state = enrollment
            .enroll_xai_api_key(
                CredentialEnrollmentRequest {
                    parent_window_token: request.parent_window_token,
                },
                key,
            )
            .await?;
        Ok(v1::response::Result::AccountState(account_state_to_wire(
            state,
        )))
    }

    async fn delete_xai_api_key(
        &self,
        key: Option<&str>,
    ) -> Result<v1::response::Result, ApplicationError> {
        let state = self
            .credentials
            .delete_xai_api_key(mutation_key(key)?)
            .await?;
        Ok(v1::response::Result::AccountState(account_state_to_wire(
            state,
        )))
    }

    async fn get_host_execution_policy(&self) -> Result<v1::response::Result, ApplicationError> {
        let policy = self
            .host_execution_policy_store()?
            .get_host_execution_policy()
            .await?;
        let ready = match &self.host_work_runtime {
            Some(runtime) => runtime.is_ready().await,
            None => false,
        };
        Ok(v1::response::Result::HostExecutionPolicy(
            host_execution_policy_to_wire(policy, ready),
        ))
    }

    async fn enroll_host_execution(
        &self,
        request: v1::EnrollHostExecutionRequest,
        key: Option<&str>,
    ) -> Result<v1::response::Result, ApplicationError> {
        let key = mutation_key(key)?;
        let mut command_parts = vec![
            request.expected_revision.to_string(),
            request.acknowledgment_version.to_string(),
            request.typed_acknowledgment.clone(),
            request.filesystem_read.to_string(),
            request.filesystem_write.to_string(),
            request.process_execute.to_string(),
            request.broad_scope_acknowledged.to_string(),
        ];
        command_parts.extend(request.path_roots.iter().cloned());
        let command = host_policy_command("enroll_host_execution_v1", key, &command_parts)?;
        let store = self.host_execution_policy_store()?;
        if let Some(policy) = store
            .resolve_host_execution_policy_mutation(&command)
            .await?
        {
            return Ok(v1::response::Result::HostExecutionPolicy(
                host_execution_policy_to_wire(policy, false),
            ));
        }
        let roots = canonical_host_roots(&request.path_roots)?;
        let broad = roots.iter().any(|root| host_root_is_broad(root));
        if broad && !request.broad_scope_acknowledged {
            return Err(ApplicationError::InvalidInput(
                "broad Host Tools scope requires the additional acknowledgment".into(),
            ));
        }
        let classes = HostToolClasses {
            filesystem_read: request.filesystem_read,
            filesystem_write: request.filesystem_write,
            process_execute: request.process_execute,
        };
        let root_strings = roots
            .iter()
            .map(|root| root.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        HostExecutionPolicy::validate_enrollment(
            request.acknowledgment_version,
            &request.typed_acknowledgment,
            classes,
            &root_strings,
        )
        .map_err(|error| ApplicationError::InvalidInput(error.to_string()))?;
        let now = self.clock.now();
        let policy = HostExecutionPolicy {
            revision: request.expected_revision.saturating_add(1),
            active: true,
            acknowledgment_version: request.acknowledgment_version,
            acknowledged_at: now,
            tool_classes: classes,
            canonical_roots: root_strings,
            broad_scope_acknowledged: request.broad_scope_acknowledged,
            updated_at: now,
        };
        let policy = store
            .replace_host_execution_policy(policy, request.expected_revision, &command)
            .await?;
        if let Some(runtime) = &self.host_work_runtime {
            let _ = runtime.deactivate().await;
        }
        Ok(v1::response::Result::HostExecutionPolicy(
            host_execution_policy_to_wire(policy, false),
        ))
    }

    async fn prepare_host_work_runtime(&self) -> Result<v1::response::Result, ApplicationError> {
        let runtime = self.host_work_runtime.as_ref().ok_or_else(|| {
            ApplicationError::InvalidState(
                "The official Grok Build runtime must be ready before Host Tools can be prepared."
                    .into(),
            )
        })?;
        let policy = self
            .host_execution_policy_store()?
            .get_host_execution_policy()
            .await?;
        runtime.prepare(&policy).await.map_err(|error| {
            if error.kind == AgentRuntimeErrorKind::Authentication {
                ApplicationError::InvalidState(
                    "Connect Grok Build in Setup before preparing Host Tools.".into(),
                )
            } else {
                agent_runtime_application_error(error)
            }
        })?;
        Ok(v1::response::Result::HostExecutionPolicy(
            host_execution_policy_to_wire(policy, true),
        ))
    }

    async fn deactivate_host_work_runtime(&self) -> Result<v1::response::Result, ApplicationError> {
        let runtime = self.host_work_runtime.as_ref().ok_or_else(|| {
            ApplicationError::Unavailable("Host Tools runtime is not configured".into())
        })?;
        runtime
            .deactivate()
            .await
            .map_err(agent_runtime_application_error)?;
        let policy = self
            .host_execution_policy_store()?
            .get_host_execution_policy()
            .await?;
        Ok(v1::response::Result::HostExecutionPolicy(
            host_execution_policy_to_wire(policy, false),
        ))
    }

    async fn start_host_work(
        &self,
        request: v1::StartHostWorkRequest,
        key: Option<&str>,
    ) -> Result<v1::response::Result, ApplicationError> {
        let service = self.host_work_service.as_ref().ok_or_else(|| {
            ApplicationError::Unavailable("Host Work service is not configured".into())
        })?;
        let run = service
            .start(
                &request.project_id,
                &request.thread_id,
                &request.prompt,
                mutation_key(key)?,
            )
            .await?;
        Ok(v1::response::Result::HostWorkResult(v1::HostWorkResult {
            run: Some(run_to_wire(run)),
            assistant_text: String::new(),
        }))
    }

    async fn cancel_host_work(
        &self,
        request: v1::CancelHostWorkRequest,
        key: Option<&str>,
    ) -> Result<v1::response::Result, ApplicationError> {
        let service = self.host_work_service.as_ref().ok_or_else(|| {
            ApplicationError::Unavailable("Host Work service is not configured".into())
        })?;
        let run = service.cancel(&request.run_id, mutation_key(key)?).await?;
        Ok(v1::response::Result::HostWorkResult(v1::HostWorkResult {
            run: Some(run_to_wire(run)),
            assistant_text: String::new(),
        }))
    }

    async fn list_host_work_runs(
        &self,
        request: v1::ListHostWorkRunsRequest,
    ) -> Result<v1::response::Result, ApplicationError> {
        if !(1..=100).contains(&request.limit) {
            return Err(ApplicationError::InvalidInput(
                "Host Work list limit must be between 1 and 100".into(),
            ));
        }
        let thread_id = if request.thread_id.is_empty() {
            None
        } else {
            Some(ThreadId::new(request.thread_id)?)
        };
        let items = self
            .runs
            .list_host_work(
                usize::try_from(request.limit).unwrap_or(0),
                thread_id.as_ref(),
            )
            .await?
            .into_iter()
            .map(|(run, approval)| v1::HostWorkSnapshot {
                run: Some(run_to_wire(run)),
                pending_approval: approval.map(approval_to_wire),
            })
            .collect();
        Ok(v1::response::Result::HostWorkList(v1::HostWorkList {
            items,
        }))
    }

    async fn revoke_host_execution(
        &self,
        request: v1::RevokeHostExecutionRequest,
        key: Option<&str>,
    ) -> Result<v1::response::Result, ApplicationError> {
        let key = mutation_key(key)?;
        let command = host_policy_command(
            "revoke_host_execution_v1",
            key,
            &[request.expected_revision.to_string()],
        )?;
        let store = self.host_execution_policy_store()?;
        if let Some(policy) = store
            .resolve_host_execution_policy_mutation(&command)
            .await?
        {
            if let Some(runtime) = &self.host_work_runtime {
                let _ = runtime.deactivate().await;
            }
            return Ok(v1::response::Result::HostExecutionPolicy(
                host_execution_policy_to_wire(policy, false),
            ));
        }
        let mut policy = store.get_host_execution_policy().await?;
        if policy.revision != request.expected_revision {
            return Err(ApplicationError::Conflict);
        }
        policy.revision = policy.revision.saturating_add(1);
        policy.active = false;
        policy.updated_at = self.clock.now();
        let policy = store
            .replace_host_execution_policy(policy, request.expected_revision, &command)
            .await?;
        if let Some(runtime) = &self.host_work_runtime {
            let _ = runtime.deactivate().await;
        }
        Ok(v1::response::Result::HostExecutionPolicy(
            host_execution_policy_to_wire(policy, false),
        ))
    }

    fn host_execution_policy_store(
        &self,
    ) -> Result<&Arc<dyn HostExecutionPolicyStore>, ApplicationError> {
        self.host_execution_policy.as_ref().ok_or_else(|| {
            ApplicationError::Unavailable("Host Tools policy is not configured".into())
        })
    }

    async fn get_desktop_preferences(&self) -> Result<v1::response::Result, ApplicationError> {
        let preferences = self.desktop_preferences()?.get().await?;
        Ok(v1::response::Result::DesktopPreferences(
            desktop_preferences_to_wire(&preferences),
        ))
    }

    async fn update_desktop_preferences(
        &self,
        request: v1::UpdateDesktopPreferencesRequest,
        key: Option<&str>,
    ) -> Result<v1::response::Result, ApplicationError> {
        let preferences = self
            .desktop_preferences()?
            .update(
                UpdateDesktopPreferences {
                    expected_revision: request.expected_revision,
                    keep_running_in_notification_area: request.keep_running_in_notification_area,
                    update_channel: match request.update_channel.as_str() {
                        "stable" => DesktopUpdateChannel::Stable,
                        "beta" => DesktopUpdateChannel::Beta,
                        _ => {
                            return Err(ApplicationError::InvalidInput(
                                "desktop update channel is invalid".into(),
                            ));
                        }
                    },
                },
                mutation_key(key)?,
            )
            .await?;
        Ok(v1::response::Result::DesktopPreferences(
            desktop_preferences_to_wire(&preferences),
        ))
    }

    async fn get_chat_model_catalog(&self) -> Result<v1::response::Result, ApplicationError> {
        let catalog = self.chat_models()?.catalog().await?;
        Ok(v1::response::Result::ChatModelCatalog(
            chat_model_catalog_to_wire(catalog),
        ))
    }

    async fn get_usage_summary(
        &self,
        request: v1::GetUsageSummaryRequest,
    ) -> Result<v1::response::Result, ApplicationError> {
        let input = parse_usage_summary_request(request)?;
        let summary = self.conversation()?.usage_summary(input).await?;
        Ok(v1::response::Result::UsageSummary(usage_summary_to_wire(
            summary,
        )))
    }

    async fn select_chat_model(
        &self,
        request: v1::SelectChatModelRequest,
        key: Option<&str>,
    ) -> Result<v1::response::Result, ApplicationError> {
        let preference = self
            .chat_models()?
            .select(
                SelectChatModel {
                    expected_revision: request.expected_revision,
                    model_id: request.model_id,
                },
                mutation_key(key)?,
            )
            .await?;
        Ok(v1::response::Result::ChatModelPreference(
            chat_model_preference_to_wire(preference),
        ))
    }

    async fn start_conversation_turn(
        &self,
        request: v1::StartConversationTurnRequest,
        key: Option<&str>,
        budget: Duration,
    ) -> Result<v1::response::Result, ApplicationError> {
        let conversation = self.conversation.clone().ok_or_else(|| {
            ApplicationError::Unavailable("conversation execution is not configured".into())
        })?;
        let thread_id = ThreadId::new(&request.thread_id)?;
        if !self
            .runs
            .list_host_work(1, Some(&thread_id))
            .await?
            .is_empty()
        {
            return Err(ApplicationError::InvalidState(
                "Work conversations cannot dispatch unprivileged Chat turns".into(),
            ));
        }
        let key = mutation_key(key)?;
        // Blank optional model_id is absent, not an override. Clients that coerce
        // missing overrides to "" would otherwise Conflict against a bound model.
        let input = StartConversationTurn {
            thread_id: request.thread_id,
            content: request.content,
            model_id: request
                .model_id
                .filter(|model_id| !model_id.trim().is_empty()),
            search_enabled: request.search_enabled,
        };
        if let Some(replay) = conversation.replay_start(&input, key).await?
            && let Some(result) = self.conversation_replay_result(replay).await?
        {
            return Ok(result);
        }

        // Capacity is acquired before a new reservation so saturation cannot
        // leave a durable turn with no daemon task able to claim provider work.
        // Exact active and terminal replays bypass this gate above.
        let permit = self.conversation_tasks.try_acquire()?;
        let started = conversation
            .start(input, key, Box::pin(tokio::time::sleep(budget)))
            .await?;
        self.launch_conversation_turn(conversation, started, permit)
            .await
    }

    async fn retry_conversation_turn(
        &self,
        request: v1::RetryConversationTurnRequest,
        key: Option<&str>,
        budget: Duration,
    ) -> Result<v1::response::Result, ApplicationError> {
        let conversation = self.conversation.clone().ok_or_else(|| {
            ApplicationError::Unavailable("conversation execution is not configured".into())
        })?;
        let key = mutation_key(key)?;
        let input = RetryConversationTurn {
            source_turn_id: request.source_turn_id,
            expected_revision: request.expected_revision,
        };
        if let Some(replay) = conversation.replay_retry(&input, key).await?
            && let Some(result) = self.conversation_replay_result(replay).await?
        {
            return Ok(result);
        }
        let permit = self.conversation_tasks.try_acquire()?;
        let started = conversation
            .retry(input, key, Box::pin(tokio::time::sleep(budget)))
            .await?;
        self.launch_conversation_turn(conversation, started, permit)
            .await
    }

    async fn branch_conversation_thread(
        &self,
        request: v1::BranchConversationThreadRequest,
        key: Option<&str>,
    ) -> Result<v1::response::Result, ApplicationError> {
        let conversation = self.conversation.clone().ok_or_else(|| {
            ApplicationError::Unavailable("conversation execution is not configured".into())
        })?;
        let key = mutation_key(key)?;
        let input = BranchConversationThread {
            source_turn_id: request.source_turn_id,
            expected_revision: request.expected_revision,
        };
        if let Some(replay) = conversation.replay_branch(&input, key).await? {
            return self.conversation_fork_result(replay.snapshot).await;
        }
        let started = conversation.branch(input, key).await?;
        if started.dispatch.is_some() || started.snapshot.started_turn.is_some() {
            return Err(ApplicationError::Integrity(
                "provider-free Branch returned a conversation dispatch".into(),
            ));
        }
        self.conversation_fork_result(started.snapshot).await
    }

    async fn edit_and_branch_conversation_turn(
        &self,
        request: v1::EditAndBranchConversationTurnRequest,
        key: Option<&str>,
        budget: Duration,
    ) -> Result<v1::response::Result, ApplicationError> {
        let conversation = self.conversation.clone().ok_or_else(|| {
            ApplicationError::Unavailable("conversation execution is not configured".into())
        })?;
        let key = mutation_key(key)?;
        let input = EditAndBranchConversationTurn {
            source_turn_id: request.source_turn_id,
            expected_revision: request.expected_revision,
            content: request.content,
        };
        if let Some(replay) = conversation.replay_edit_and_branch(&input, key).await?
            && let Some(result) = self.conversation_fork_replay_result(replay).await?
        {
            return Ok(result);
        }
        let permit = self.conversation_tasks.try_acquire()?;
        let started = conversation
            .edit_and_branch(input, key, Box::pin(tokio::time::sleep(budget)))
            .await?;
        self.launch_conversation_fork(conversation, started, permit)
            .await
    }

    async fn regenerate_conversation_turn(
        &self,
        request: v1::RegenerateConversationTurnRequest,
        key: Option<&str>,
        budget: Duration,
    ) -> Result<v1::response::Result, ApplicationError> {
        let conversation = self.conversation.clone().ok_or_else(|| {
            ApplicationError::Unavailable("conversation execution is not configured".into())
        })?;
        let key = mutation_key(key)?;
        let input = RegenerateConversationTurn {
            source_turn_id: request.source_turn_id,
            expected_revision: request.expected_revision,
        };
        if let Some(replay) = conversation.replay_regenerate(&input, key).await?
            && let Some(result) = self.conversation_fork_replay_result(replay).await?
        {
            return Ok(result);
        }
        let permit = self.conversation_tasks.try_acquire()?;
        let started = conversation
            .regenerate(input, key, Box::pin(tokio::time::sleep(budget)))
            .await?;
        self.launch_conversation_fork(conversation, started, permit)
            .await
    }

    async fn get_conversation_fork_metadata(
        &self,
        request: v1::GetConversationForkMetadataRequest,
    ) -> Result<v1::response::Result, ApplicationError> {
        let thread_id = ThreadId::new(request.thread_id)?;
        let metadata = self.conversation()?.fork_metadata(&thread_id).await?;
        Ok(v1::response::Result::ConversationForkMetadata(
            conversation_fork_metadata_to_wire(metadata),
        ))
    }

    async fn acknowledge_conversation_fork_delivery(
        &self,
        request: v1::AcknowledgeConversationForkDeliveryRequest,
        key: Option<&str>,
    ) -> Result<v1::response::Result, ApplicationError> {
        let delivery = self
            .conversation()?
            .acknowledge_fork_delivery(
                AcknowledgeConversationForkDelivery {
                    child_thread_id: request.child_thread_id,
                    expected_revision: request.expected_revision,
                },
                mutation_key(key)?,
            )
            .await?;
        Ok(v1::response::Result::ConversationForkDelivery(
            conversation_fork_delivery_to_wire(&delivery),
        ))
    }

    async fn conversation_fork_replay_result(
        &self,
        replay: ConversationForkCommandResolution,
    ) -> Result<Option<v1::response::Result>, ApplicationError> {
        let replay = replay.snapshot;
        let Some(turn) = replay.started_turn.as_ref() else {
            return self.conversation_fork_result(replay).await.map(Some);
        };
        if turn.turn.state.is_terminal() {
            self.conversation_tasks
                .release_quarantined(&turn.turn.id)
                .await;
            return self.conversation_fork_result(replay).await.map(Some);
        }
        match self.conversation_tasks.ownership(&turn.turn.id).await {
            Some(ConversationTaskOwnership::Active) => {
                return self.conversation_fork_result(replay).await.map(Some);
            }
            Some(ConversationTaskOwnership::Quarantined) => {
                return Err(ApplicationError::Unavailable(
                    "conversation turn is quarantined until cancellation or restart".into(),
                ));
            }
            None => {}
        }
        if turn.turn.state == ConversationTurnState::ProviderStarted {
            return self.conversation_fork_result(replay).await.map(Some);
        }
        Ok(None)
    }

    async fn launch_conversation_fork(
        &self,
        conversation: Arc<ConversationService>,
        started: StartedConversationFork,
        permit: OwnedSemaphorePermit,
    ) -> Result<v1::response::Result, ApplicationError> {
        if started.reconciled_pending_delivery {
            if started.dispatch.is_some() {
                return Err(ApplicationError::Integrity(
                    "a reconciled conversation fork returned a dispatch".into(),
                ));
            }
            return self.conversation_fork_result(started.snapshot).await;
        }
        let fork_snapshot = started.snapshot;
        let turn_snapshot = fork_snapshot.started_turn.clone().ok_or_else(|| {
            ApplicationError::Integrity("dispatching fork is missing its child turn".into())
        })?;
        if started.dispatch.is_none() && turn_snapshot.turn.state == ConversationTurnState::Reserved
        {
            return Err(ApplicationError::Integrity(
                "reserved fork turn is missing its daemon dispatch".into(),
            ));
        }
        let _ = self
            .launch_conversation_turn(
                conversation,
                StartedConversationTurn {
                    snapshot: turn_snapshot,
                    dispatch: started.dispatch,
                },
                permit,
            )
            .await?;
        self.conversation_fork_result(fork_snapshot).await
    }

    async fn conversation_fork_result(
        &self,
        mut snapshot: ConversationForkSnapshot,
    ) -> Result<v1::response::Result, ApplicationError> {
        let started_turn = snapshot.started_turn.take();
        let mut wire = conversation_fork_to_wire(snapshot);
        if let Some(started_turn) = started_turn {
            let eligibility = self.conversation_retry_eligibility(&started_turn).await?;
            wire.started_turn = Some(conversation_turn_to_wire_with_retry_eligibility(
                started_turn,
                eligibility,
            ));
        }
        Ok(v1::response::Result::ConversationFork(wire))
    }

    async fn conversation_replay_result(
        &self,
        replay: ConversationTurnSnapshot,
    ) -> Result<Option<v1::response::Result>, ApplicationError> {
        if replay.turn.state.is_terminal() {
            self.conversation_tasks
                .release_quarantined(&replay.turn.id)
                .await;
            return self.conversation_turn_result(replay).await.map(Some);
        }
        match self.conversation_tasks.ownership(&replay.turn.id).await {
            Some(ConversationTaskOwnership::Active) => {
                return self.conversation_turn_result(replay).await.map(Some);
            }
            Some(ConversationTaskOwnership::Quarantined) => {
                return Err(ApplicationError::Unavailable(
                    "conversation turn is quarantined until cancellation or restart".into(),
                ));
            }
            None => {}
        }
        if replay.turn.state == ConversationTurnState::ProviderStarted {
            return self.conversation_turn_result(replay).await.map(Some);
        }
        Ok(None)
    }

    async fn launch_conversation_turn(
        &self,
        conversation: Arc<ConversationService>,
        started: StartedConversationTurn,
        permit: OwnedSemaphorePermit,
    ) -> Result<v1::response::Result, ApplicationError> {
        if let Some(dispatch) = started.dispatch {
            let turn_id = dispatch.turn_id().clone();
            if let Some(registration) = self.conversation_tasks.register(turn_id.clone()).await {
                let registry = self.conversation_tasks.clone();
                tokio::spawn(async move {
                    let generation = registration.generation;
                    let cancel = registration.cancel;
                    let cancellation = Box::pin(async move {
                        tokio::select! {
                            _ = cancel => {}
                            () = tokio::time::sleep(MAX_CONVERSATION_DISPATCH_DURATION) => {}
                        }
                    });
                    let outcome = conversation.dispatch(dispatch, cancellation).await;
                    let requires_reconciliation = match &outcome {
                        Ok(snapshot) => snapshot.turn.state == ConversationTurnState::Reserved,
                        Err(_) => true,
                    };
                    let reconciled = if requires_reconciliation {
                        tokio::time::timeout(MAX_CONVERSATION_RECONCILIATION_DURATION, async {
                            for attempt in 0..MAX_CONVERSATION_RECONCILIATION_ATTEMPTS {
                                match conversation.reconcile_dispatch_exit(&turn_id).await {
                                    Ok(_) | Err(ApplicationError::NotFound) => return true,
                                    Err(_)
                                        if attempt + 1
                                            < MAX_CONVERSATION_RECONCILIATION_ATTEMPTS =>
                                    {
                                        tokio::time::sleep(CONVERSATION_RECONCILIATION_RETRY).await;
                                    }
                                    Err(_) => return false,
                                }
                            }
                            false
                        })
                        .await
                        .unwrap_or(false)
                    } else {
                        true
                    };
                    if reconciled {
                        registry.finish(&turn_id, generation).await;
                        drop(permit);
                    } else {
                        tracing::warn!(
                            turn_id = %turn_id,
                            "conversation task-exit reconciliation exhausted its bound"
                        );
                        registry.quarantine(&turn_id, generation, permit).await;
                    }
                    // A failed durable classification retains both its
                    // generation-bound quarantine and capacity permit until an
                    // exact cancellation or process restart recovery.
                });
            }
        }
        self.conversation_turn_result(started.snapshot).await
    }

    async fn conversation_turn_result(
        &self,
        snapshot: ConversationTurnSnapshot,
    ) -> Result<v1::response::Result, ApplicationError> {
        let retry_eligibility = self.conversation_retry_eligibility(&snapshot).await?;
        Ok(v1::response::Result::ConversationTurn(
            conversation_turn_to_wire_with_retry_eligibility(snapshot, retry_eligibility),
        ))
    }

    async fn conversation_retry_eligibility(
        &self,
        snapshot: &ConversationTurnSnapshot,
    ) -> Result<ConversationRetryEligibility, ApplicationError> {
        let candidate = match snapshot.turn.state {
            ConversationTurnState::Reserved | ConversationTurnState::ProviderStarted => {
                return Ok(ConversationRetryEligibility::SourceInProgress);
            }
            ConversationTurnState::Completed => {
                return Ok(ConversationRetryEligibility::SourceCompleted);
            }
            ConversationTurnState::InterruptedNeedsReview => {
                return Ok(ConversationRetryEligibility::SourceInterruptedNeedsReview);
            }
            ConversationTurnState::Failed
                if snapshot
                    .turn
                    .failure
                    .as_ref()
                    .is_none_or(|failure| !failure.retryable) =>
            {
                return Ok(ConversationRetryEligibility::FailureNotRetryable);
            }
            ConversationTurnState::Failed | ConversationTurnState::Cancelled => true,
        };
        debug_assert!(candidate);
        let conversation = self.conversation()?;
        if snapshot.lineage.retry_depth >= 64 {
            return Ok(ConversationRetryEligibility::DepthExhausted);
        }
        if !conversation.retry_source_is_writable(snapshot).await? {
            return Ok(ConversationRetryEligibility::SourceReadOnly);
        }
        if !conversation
            .retry_source_is_latest(&snapshot.turn.id)
            .await?
        {
            return Ok(ConversationRetryEligibility::NotNewest);
        }
        if !conversation
            .retry_source_account_available(snapshot)
            .await?
        {
            return Ok(ConversationRetryEligibility::SourceAccountUnavailable);
        }
        Ok(ConversationRetryEligibility::Allowed)
    }

    async fn cancel_conversation_turn(
        &self,
        request: v1::CancelConversationTurnRequest,
        key: Option<&str>,
    ) -> Result<v1::response::Result, ApplicationError> {
        let turn_id = ConversationTurnId::new(request.turn_id)?;
        let snapshot = self
            .conversation()?
            .cancel(&turn_id, request.expected_revision, mutation_key(key)?)
            .await?;
        if matches!(
            snapshot.turn.state,
            ConversationTurnState::Cancelled | ConversationTurnState::InterruptedNeedsReview
        ) {
            // The durable terminal classification above always precedes this
            // best-effort cooperative provider-task signal.
            self.conversation_tasks.signal(&turn_id).await;
        }
        if snapshot.turn.state.is_terminal() {
            self.conversation_tasks.release_quarantined(&turn_id).await;
        }
        self.conversation_turn_result(snapshot).await
    }

    async fn poll_conversation_turn_events(
        &self,
        request: v1::PollConversationTurnEventsRequest,
    ) -> Result<v1::response::Result, ApplicationError> {
        validate_run_event_cursor(request.after_sequence)?;
        let turn_id = ConversationTurnId::new(request.turn_id)?;
        let limit = usize::try_from(request.limit).unwrap_or(usize::MAX);
        if !(1..=MAX_RUN_EVENT_BATCH_SIZE).contains(&limit) {
            return Err(ApplicationError::InvalidInput(
                "conversation event poll limit must be between 1 and 100".into(),
            ));
        }
        let wait = Duration::from_millis(u64::from(request.wait_timeout_ms));
        if wait > MAX_RUN_EVENT_POLL_WAIT {
            return Err(ApplicationError::InvalidInput(
                "conversation event poll wait must not exceed 20000 milliseconds".into(),
            ));
        }
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let page = self
                .conversation()?
                .events_since(&turn_id, request.after_sequence, limit)
                .await?;
            if !page.events.is_empty() || wait.is_zero() || tokio::time::Instant::now() >= deadline
            {
                return Ok(v1::response::Result::ConversationTurnEventBatch(
                    conversation_turn_event_page_to_wire(page, request.after_sequence),
                ));
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            tokio::time::sleep(RUN_EVENT_POLL_INTERVAL.min(remaining)).await;
        }
    }

    async fn list_conversation_turns(
        &self,
        request: v1::ListConversationTurnsRequest,
    ) -> Result<v1::response::Result, ApplicationError> {
        let thread_id = ThreadId::new(request.thread_id)?;
        let cursor = optional(&request.cursor)
            .map(grok_domain::ConversationTurnId::new)
            .transpose()?;
        let limit = usize::try_from(request.limit).unwrap_or(usize::MAX);
        let page = self
            .conversation()?
            .list_for_thread(&thread_id, cursor.as_ref(), limit)
            .await?;
        let mut turns = Vec::with_capacity(page.items.len());
        for snapshot in page.items {
            if snapshot.turn.state.is_terminal() {
                self.conversation_tasks
                    .release_quarantined(&snapshot.turn.id)
                    .await;
            }
            let eligibility = self.conversation_retry_eligibility(&snapshot).await?;
            turns.push(conversation_turn_to_wire_with_retry_eligibility(
                snapshot,
                eligibility,
            ));
        }
        Ok(v1::response::Result::ConversationTurns(
            v1::ConversationTurnList {
                turns,
                next_cursor: page.next_cursor.unwrap_or_default(),
            },
        ))
    }

    async fn events_since(
        &self,
        request: v1::EventsSinceRequest,
    ) -> Result<v1::response::Result, ApplicationError> {
        validate_run_event_cursor(request.after_sequence)?;
        let id = RunId::new(request.run_id)?;
        let events = self
            .runs
            .events_since(
                &id,
                request.after_sequence,
                usize::try_from(request.limit).unwrap_or(usize::MAX),
            )
            .await?;
        Ok(v1::response::Result::Events(v1::EventsSinceResponse {
            events: events.into_iter().map(event_to_wire).collect(),
        }))
    }

    async fn poll_run_events(
        &self,
        request: v1::PollRunEventsRequest,
    ) -> Result<v1::response::Result, ApplicationError> {
        validate_run_event_cursor(request.after_sequence)?;
        let id = RunId::new(request.run_id)?;
        let limit = usize::try_from(request.limit).unwrap_or(usize::MAX);
        if !(1..=MAX_RUN_EVENT_BATCH_SIZE).contains(&limit) {
            return Err(ApplicationError::InvalidInput(
                "run event poll limit must be between 1 and 100".into(),
            ));
        }
        let wait = Duration::from_millis(u64::from(request.wait_timeout_ms));
        if wait > MAX_RUN_EVENT_POLL_WAIT {
            return Err(ApplicationError::InvalidInput(
                "run event poll wait must not exceed 20000 milliseconds".into(),
            ));
        }
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            // Fetch one extra durable event to make backlog explicit without
            // returning more than the caller's bounded batch.
            let mut events = self
                .runs
                .events_since(&id, request.after_sequence, limit + 1)
                .await?;
            let has_more = events.len() > limit;
            events.truncate(limit);
            if !events.is_empty() || wait.is_zero() || tokio::time::Instant::now() >= deadline {
                let next_sequence = events
                    .last()
                    .map_or(request.after_sequence, |event| event.sequence);
                return Ok(v1::response::Result::RunEventBatch(v1::RunEventBatch {
                    events: events.into_iter().map(event_to_wire).collect(),
                    next_sequence,
                    has_more,
                }));
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            tokio::time::sleep(RUN_EVENT_POLL_INTERVAL.min(remaining)).await;
        }
    }

    async fn decide_approval(
        &self,
        request: v1::DecideApprovalRequest,
        key: Option<&str>,
    ) -> Result<v1::response::Result, ApplicationError> {
        let id = ApprovalId::new(request.approval_id)?;
        let decision = approval_decision_from_wire(request.decision)
            .map_err(|error| ApplicationError::InvalidInput(error.to_string()))?;
        let approval = self
            .approvals
            .decide(&id, request.expected_revision, decision, mutation_key(key)?)
            .await?;
        Ok(v1::response::Result::Approval(approval_to_wire(approval)))
    }

    async fn create_project(
        &self,
        request: v1::CreateProjectRequest,
        key: Option<&str>,
    ) -> Result<v1::response::Result, ApplicationError> {
        let project = self
            .workspace()?
            .create_project(
                CreateProject {
                    name: request.name,
                    description: request.description,
                },
                mutation_key(key)?,
            )
            .await?;
        Ok(v1::response::Result::Project(project_to_wire(project)))
    }

    async fn update_project(
        &self,
        request: v1::UpdateProjectRequest,
        key: Option<&str>,
    ) -> Result<v1::response::Result, ApplicationError> {
        let project = self
            .workspace()?
            .update_project(
                UpdateProject {
                    id: request.project_id,
                    expected_revision: request.expected_revision,
                    name: request.name,
                    description: request.description,
                },
                mutation_key(key)?,
            )
            .await?;
        Ok(v1::response::Result::Project(project_to_wire(project)))
    }

    async fn archive_project(
        &self,
        request: v1::ArchiveProjectRequest,
        key: Option<&str>,
    ) -> Result<v1::response::Result, ApplicationError> {
        let project = self
            .workspace()?
            .archive_project(
                &ProjectId::new(request.project_id)?,
                request.expected_revision,
                mutation_key(key)?,
            )
            .await?;
        Ok(v1::response::Result::Project(project_to_wire(project)))
    }

    async fn get_project(
        &self,
        request: v1::GetProjectRequest,
    ) -> Result<v1::response::Result, ApplicationError> {
        let project = self
            .workspace()?
            .get_project(&ProjectId::new(request.project_id)?)
            .await?;
        Ok(v1::response::Result::Project(project_to_wire(project)))
    }

    async fn list_projects(
        &self,
        request: v1::ListProjectsRequest,
    ) -> Result<v1::response::Result, ApplicationError> {
        let page = self
            .workspace()?
            .list_projects(optional(&request.cursor), request.limit as usize)
            .await?;
        Ok(v1::response::Result::Projects(v1::ProjectList {
            projects: page.items.into_iter().map(project_to_wire).collect(),
            next_cursor: page.next_cursor.unwrap_or_default(),
        }))
    }

    async fn create_thread(
        &self,
        request: v1::CreateThreadRequest,
        key: Option<&str>,
    ) -> Result<v1::response::Result, ApplicationError> {
        let thread = self
            .workspace()?
            .create_thread(
                CreateThread {
                    project_id: request.project_id,
                    title: request.title,
                },
                mutation_key(key)?,
            )
            .await?;
        Ok(v1::response::Result::Thread(thread_to_wire(thread)))
    }

    async fn update_thread(
        &self,
        request: v1::UpdateThreadRequest,
        key: Option<&str>,
    ) -> Result<v1::response::Result, ApplicationError> {
        let thread = self
            .workspace()?
            .update_thread(
                UpdateThread {
                    id: request.thread_id,
                    expected_revision: request.expected_revision,
                    title: request.title,
                },
                mutation_key(key)?,
            )
            .await?;
        Ok(v1::response::Result::Thread(thread_to_wire(thread)))
    }

    async fn archive_thread(
        &self,
        request: v1::ArchiveThreadRequest,
        key: Option<&str>,
    ) -> Result<v1::response::Result, ApplicationError> {
        let thread = self
            .workspace()?
            .archive_thread(
                &ThreadId::new(request.thread_id)?,
                request.expected_revision,
                mutation_key(key)?,
            )
            .await?;
        Ok(v1::response::Result::Thread(thread_to_wire(thread)))
    }

    async fn get_thread(
        &self,
        request: v1::GetThreadRequest,
    ) -> Result<v1::response::Result, ApplicationError> {
        let thread = self
            .workspace()?
            .get_thread(&ThreadId::new(request.thread_id)?)
            .await?;
        Ok(v1::response::Result::Thread(thread_to_wire(thread)))
    }

    async fn list_threads(
        &self,
        request: v1::ListThreadsRequest,
    ) -> Result<v1::response::Result, ApplicationError> {
        let page = self
            .workspace()?
            .list_threads(
                &ProjectId::new(request.project_id)?,
                optional(&request.cursor),
                request.limit as usize,
            )
            .await?;
        Ok(v1::response::Result::Threads(v1::ThreadList {
            threads: page.items.into_iter().map(thread_to_wire).collect(),
            next_cursor: page.next_cursor.unwrap_or_default(),
        }))
    }

    async fn get_message(
        &self,
        request: v1::GetMessageRequest,
    ) -> Result<v1::response::Result, ApplicationError> {
        let message = self
            .workspace()?
            .get_message(&MessageId::new(request.message_id)?)
            .await?;
        Ok(v1::response::Result::Message(message_to_wire(message)))
    }

    async fn list_messages(
        &self,
        request: v1::ListMessagesRequest,
    ) -> Result<v1::response::Result, ApplicationError> {
        let page = self
            .workspace()?
            .list_messages(
                &ThreadId::new(request.thread_id)?,
                optional(&request.cursor),
                request.limit as usize,
            )
            .await?;
        Ok(v1::response::Result::Messages(v1::MessageList {
            messages: page.items.into_iter().map(message_to_wire).collect(),
            next_cursor: page.next_cursor.unwrap_or_default(),
        }))
    }

    async fn get_artifact(
        &self,
        request: v1::GetArtifactRequest,
    ) -> Result<v1::response::Result, ApplicationError> {
        let artifact = self
            .artifacts()?
            .get_artifact(&ArtifactId::new(request.artifact_id)?)
            .await?;
        Ok(v1::response::Result::Artifact(artifact_to_wire(artifact)))
    }

    async fn list_artifacts(
        &self,
        request: v1::ListArtifactsRequest,
    ) -> Result<v1::response::Result, ApplicationError> {
        let page = self
            .artifacts()?
            .list_artifacts(
                &ProjectId::new(request.project_id)?,
                optional(&request.cursor),
                request.limit as usize,
            )
            .await?;
        Ok(v1::response::Result::Artifacts(v1::ArtifactList {
            artifacts: page.items.into_iter().map(artifact_to_wire).collect(),
            next_cursor: page.next_cursor.unwrap_or_default(),
        }))
    }

    async fn import_artifact(
        &self,
        request: v1::ImportArtifactRequest,
        key: Option<&str>,
    ) -> Result<v1::response::Result, ApplicationError> {
        let input = import_artifact_from_wire(request)
            .map_err(|error| ApplicationError::InvalidInput(error.to_string()))?;
        let artifacts = self.artifacts.clone().ok_or_else(|| {
            ApplicationError::Unavailable("artifact service is not configured".into())
        })?;
        let key = mutation_key(key)?.to_owned();
        if !self.artifact_content_available {
            if let Some(artifact) = artifacts.replay_import_if_known(&input, &key).await? {
                return Ok(v1::response::Result::ArtifactOperation(
                    imported_artifact_to_wire(artifact),
                ));
            }
            return Err(ApplicationError::Unavailable(
                "artifact import is not qualified on this runtime".into(),
            ));
        }
        let artifact = tokio::spawn(async move { artifacts.import_artifact(input, &key).await })
            .await
            .map_err(|_| artifact_task_join_failure())??;
        Ok(v1::response::Result::ArtifactOperation(
            imported_artifact_to_wire(artifact),
        ))
    }

    async fn open_artifact(
        &self,
        request: v1::OpenArtifactRequest,
        key: Option<&str>,
    ) -> Result<v1::response::Result, ApplicationError> {
        let input = open_artifact_from_wire(request)
            .map_err(|error| ApplicationError::InvalidInput(error.to_string()))?;
        let artifacts = self.artifacts.clone().ok_or_else(|| {
            ApplicationError::Unavailable("artifact service is not configured".into())
        })?;
        let key = mutation_key(key)?.to_owned();
        if !self.artifact_open_available {
            if let Some(receipt) = artifacts.replay_open_if_known(&input, &key).await? {
                return Ok(v1::response::Result::ArtifactOperation(
                    artifact_open_receipt_to_wire(receipt),
                ));
            }
            return Err(ApplicationError::Unavailable(
                "artifact local open is not qualified on this runtime".into(),
            ));
        }
        let receipt = tokio::spawn(async move { artifacts.open_artifact(input, &key).await })
            .await
            .map_err(|_| artifact_task_join_failure())??;
        Ok(v1::response::Result::ArtifactOperation(
            artifact_open_receipt_to_wire(receipt),
        ))
    }

    async fn remove_artifact(
        &self,
        request: v1::RemoveArtifactRequest,
        key: Option<&str>,
    ) -> Result<v1::response::Result, ApplicationError> {
        let input = remove_artifact_from_wire(request)
            .map_err(|error| ApplicationError::InvalidInput(error.to_string()))?;
        let artifacts = self.artifacts.clone().ok_or_else(|| {
            ApplicationError::Unavailable("artifact service is not configured".into())
        })?;
        let key = mutation_key(key)?.to_owned();
        match artifacts.resolve_removal(&input, &key).await? {
            ArtifactRemovalResolution::Committed { artifact } => {
                return Ok(v1::response::Result::ArtifactOperation(
                    removed_artifact_to_wire(artifact),
                ));
            }
            ArtifactRemovalResolution::Pending { artifact } => {
                self.trigger_artifact_removal_recovery(&artifacts);
                return Ok(v1::response::Result::ArtifactOperation(
                    artifact_removal_pending_to_wire(
                        artifact,
                        input.expected_revision,
                        input.expected_content_version,
                    ),
                ));
            }
            ArtifactRemovalResolution::Unknown => {}
        }

        if !self.artifact_content_available {
            return Err(ApplicationError::Unavailable(
                "artifact removal is not qualified on this runtime".into(),
            ));
        }
        self.trigger_artifact_removal_recovery(&artifacts);
        let permit = Arc::clone(&self.artifact_removal_direct_slots)
            .try_acquire_owned()
            .map_err(|_| {
                ApplicationError::Unavailable("artifact removal dispatch is already active".into())
            })?;
        let task_input = input.clone();
        let task_key = key.clone();
        let task_artifacts = Arc::clone(&artifacts);
        let task_recovery = Arc::clone(&self.artifact_removal_recovery);
        let task_artifacts_weak = Arc::downgrade(&artifacts);
        let task_lifetime = Arc::downgrade(&self.artifact_removal_lifetime);
        let result = tokio::spawn(async move {
            let _guard = ArtifactRemovalDirectTaskGuard {
                recovery: task_recovery,
                artifacts: task_artifacts_weak,
                lifetime: task_lifetime,
                _permit: permit,
            };
            task_artifacts.remove_artifact(task_input, &task_key).await
        })
        .await
        .map_err(|_| artifact_task_join_failure())?;
        match result {
            Ok(artifact) => Ok(v1::response::Result::ArtifactOperation(
                removed_artifact_to_wire(artifact),
            )),
            Err(error) => match artifacts.resolve_removal(&input, &key).await? {
                ArtifactRemovalResolution::Unknown => Err(error),
                ArtifactRemovalResolution::Committed { artifact } => Ok(
                    v1::response::Result::ArtifactOperation(removed_artifact_to_wire(artifact)),
                ),
                ArtifactRemovalResolution::Pending { artifact } => Ok(
                    v1::response::Result::ArtifactOperation(artifact_removal_pending_to_wire(
                        artifact,
                        input.expected_revision,
                        input.expected_content_version,
                    )),
                ),
            },
        }
    }

    fn trigger_artifact_removal_recovery(&self, artifacts: &Arc<ArtifactService>) {
        if self.artifact_content_available {
            trigger_artifact_removal_recovery(
                Arc::clone(&self.artifact_removal_recovery),
                Arc::downgrade(artifacts),
                Arc::downgrade(&self.artifact_removal_lifetime),
            );
        }
    }

    async fn create_automation(
        &self,
        request: v1::CreateAutomationRequest,
        key: Option<&str>,
    ) -> Result<v1::response::Result, ApplicationError> {
        let policies = automation_policies(request.missed_run_policy, request.overlap_policy)?;
        let automation = self
            .workspace()?
            .create_automation(
                CreateAutomation {
                    project_id: request.project_id,
                    title: request.title,
                    prompt: request.prompt,
                    schedule: request.schedule,
                    timezone: request.timezone,
                    missed_run_policy: policies.0,
                    overlap_policy: policies.1,
                    enabled: request.schedule_active && self.automation_execution_armed(),
                },
                mutation_key(key)?,
            )
            .await?;
        Ok(v1::response::Result::Automation(automation_to_wire(
            automation,
        )))
    }

    async fn update_automation(
        &self,
        request: v1::UpdateAutomationRequest,
        key: Option<&str>,
    ) -> Result<v1::response::Result, ApplicationError> {
        let policies = automation_policies(request.missed_run_policy, request.overlap_policy)?;
        let automation = self
            .workspace()?
            .update_automation(
                UpdateAutomation {
                    id: request.automation_id,
                    expected_revision: request.expected_revision,
                    title: request.title,
                    prompt: request.prompt,
                    schedule: request.schedule,
                    timezone: request.timezone,
                    missed_run_policy: policies.0,
                    overlap_policy: policies.1,
                    enabled: request.schedule_active && self.automation_execution_armed(),
                },
                mutation_key(key)?,
            )
            .await?;
        Ok(v1::response::Result::Automation(automation_to_wire(
            automation,
        )))
    }

    async fn archive_automation(
        &self,
        request: v1::ArchiveAutomationRequest,
        key: Option<&str>,
    ) -> Result<v1::response::Result, ApplicationError> {
        let automation = self
            .workspace()?
            .archive_automation(
                &AutomationId::new(request.automation_id)?,
                request.expected_revision,
                mutation_key(key)?,
            )
            .await?;
        Ok(v1::response::Result::Automation(automation_to_wire(
            automation,
        )))
    }

    async fn get_automation(
        &self,
        request: v1::GetAutomationRequest,
    ) -> Result<v1::response::Result, ApplicationError> {
        let automation = self
            .workspace()?
            .get_automation(&AutomationId::new(request.automation_id)?)
            .await?;
        Ok(v1::response::Result::Automation(automation_to_wire(
            automation,
        )))
    }

    async fn list_automations(
        &self,
        request: v1::ListAutomationsRequest,
    ) -> Result<v1::response::Result, ApplicationError> {
        let page = self
            .workspace()?
            .list_automations(
                &ProjectId::new(request.project_id)?,
                optional(&request.cursor),
                request.limit as usize,
            )
            .await?;
        Ok(v1::response::Result::Automations(v1::AutomationList {
            automations: page.items.into_iter().map(automation_to_wire).collect(),
            next_cursor: page.next_cursor.unwrap_or_default(),
        }))
    }

    async fn list_automation_history(
        &self,
        request: v1::ListAutomationHistoryRequest,
    ) -> Result<v1::response::Result, ApplicationError> {
        let entries = self
            .workspace()?
            .automation_history(
                &AutomationId::new(request.automation_id)?,
                request.after_sequence,
                request.limit as usize,
            )
            .await?;
        Ok(v1::response::Result::AutomationHistory(
            v1::AutomationHistoryList {
                entries: entries
                    .into_iter()
                    .map(automation_history_to_wire)
                    .collect(),
            },
        ))
    }

    async fn search_workspace(
        &self,
        request: v1::SearchWorkspaceRequest,
    ) -> Result<v1::response::Result, ApplicationError> {
        let project_id = (!request.project_id.is_empty())
            .then(|| ProjectId::new(request.project_id))
            .transpose()?;
        let offset = request.offset as usize;
        let limit = request.limit as usize;
        let page = self
            .workspace()?
            .search(project_id.as_ref(), &request.query, offset, limit)
            .await?;
        let next_offset = page
            .next_cursor
            .as_deref()
            .map(str::parse::<u32>)
            .transpose()
            .map_err(|_| ApplicationError::Storage("invalid search cursor".into()))?;
        Ok(v1::response::Result::SearchResults(
            v1::WorkspaceSearchResults {
                hits: page
                    .items
                    .into_iter()
                    .map(workspace_search_hit_to_wire)
                    .collect(),
                next_offset: next_offset.unwrap_or(0),
                has_more: next_offset.is_some(),
            },
        ))
    }

    fn workspace(&self) -> Result<&WorkspaceService, ApplicationError> {
        self.workspace
            .as_deref()
            .ok_or_else(|| ApplicationError::Unavailable("workspace store not configured".into()))
    }

    fn artifacts(&self) -> Result<&ArtifactService, ApplicationError> {
        self.artifacts.as_deref().ok_or_else(|| {
            ApplicationError::Unavailable("artifact service is not configured".into())
        })
    }

    fn conversation(&self) -> Result<&ConversationService, ApplicationError> {
        self.conversation.as_deref().ok_or_else(|| {
            ApplicationError::Unavailable("conversation execution is not configured".into())
        })
    }

    fn desktop_preferences(&self) -> Result<&DesktopPreferencesService, ApplicationError> {
        self.desktop_preferences.as_deref().ok_or_else(|| {
            ApplicationError::Unavailable("desktop preferences are not configured".into())
        })
    }

    fn chat_models(&self) -> Result<&ChatModelService, ApplicationError> {
        self.chat_models.as_deref().ok_or_else(|| {
            ApplicationError::Unavailable("chat model discovery is not configured".into())
        })
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.supergrok_lifetime_cancellation.cancel();
    }
}

/// Keeps a valid paged response inside the exact transport envelope budget.
///
/// Conversation turns contain bounded one-megabyte messages, so a fixed item
/// count is not itself a wire-size bound. Trimming at the final encoded
/// envelope preserves dense pages for ordinary turns while guaranteeing that
/// citation-heavy or message-heavy history cannot tear down the IPC stream.
fn bound_response_to_frame(envelope: &mut v1::Envelope) {
    while envelope.encoded_len() > MAX_FRAME_BYTES {
        let trimmed = match envelope.payload.as_mut() {
            Some(v1::envelope::Payload::Response(response)) => match response.result.as_mut() {
                Some(v1::response::Result::ConversationTurns(page)) if page.turns.len() > 1 => {
                    page.turns.pop();
                    page.next_cursor = page
                        .turns
                        .last()
                        .map_or_else(String::new, |turn| turn.turn_id.clone());
                    true
                }
                _ => false,
            },
            _ => false,
        };
        if trimmed {
            continue;
        }

        if let Some(v1::envelope::Payload::Response(response)) = envelope.payload.as_mut() {
            response.result = Some(v1::response::Result::Error(v1::ErrorResponse {
                code: v1::ErrorCode::Internal as i32,
                message: "daemon response exceeded the local IPC limit".into(),
                retryable: false,
            }));
        }
        break;
    }
    debug_assert!(envelope.encoded_len() <= MAX_FRAME_BYTES);
}

fn validate_run_event_cursor(after_sequence: u64) -> Result<(), ApplicationError> {
    if after_sequence > MAX_RUN_EVENT_SEQUENCE {
        return Err(ApplicationError::InvalidInput(
            "run event cursor is outside the durable sequence range".into(),
        ));
    }
    Ok(())
}

fn managed_integration_to_wire(
    record: &crate::IntegrationRecord,
    signature_verified: bool,
) -> v1::ManagedIntegration {
    v1::ManagedIntegration {
        id: record.id.clone(),
        state: match record.state {
            crate::ManagedIntegrationState::Available => "available",
            crate::ManagedIntegrationState::Installed => "installed",
            crate::ManagedIntegrationState::UpdateAvailable => "update_available",
            crate::ManagedIntegrationState::RollbackAvailable => "rollback_available",
        }
        .into(),
        installed_version: record.installed_version.clone().unwrap_or_default(),
        available_version: record.available_version.clone(),
        rollback_version: record.rollback_version.clone().unwrap_or_default(),
        revision: record.revision,
        signature_verified,
    }
}

fn parse_usage_summary_request(
    request: v1::GetUsageSummaryRequest,
) -> Result<GetUsageSummary, ApplicationError> {
    let window = match request.window.as_str() {
        "last_7_days" => UsageWindow::Last7Days,
        "last_30_days" => UsageWindow::Last30Days,
        "all_time" => UsageWindow::AllTime,
        _ => {
            return Err(ApplicationError::InvalidInput(
                "usage summary window is invalid".into(),
            ));
        }
    };
    let scope = match request.scope_kind.as_str() {
        "workspace" => {
            if !request.scope_id.is_empty() {
                return Err(ApplicationError::InvalidInput(
                    "workspace usage summary must not include a scope id".into(),
                ));
            }
            UsageScope::Workspace
        }
        "project" => UsageScope::Project(ProjectId::new(request.scope_id)?),
        "thread" => UsageScope::Thread(ThreadId::new(request.scope_id)?),
        _ => {
            return Err(ApplicationError::InvalidInput(
                "usage summary scope is invalid".into(),
            ));
        }
    };
    Ok(GetUsageSummary { scope, window })
}

fn grok_build_auth_status_to_wire(
    status: GrokBuildAuthStatus,
    authenticated: bool,
) -> v1::GrokBuildAuthStatus {
    let state = match status {
        GrokBuildAuthStatus::NotAuthenticated => "not_authenticated",
        GrokBuildAuthStatus::InProgress => "in_progress",
        GrokBuildAuthStatus::Authenticated => "authenticated",
        GrokBuildAuthStatus::Failed => "failed",
    };
    v1::GrokBuildAuthStatus {
        state: state.into(),
        authenticated,
    }
}

fn runtime_probe_to_wire(probe: AgentRuntimeProbe) -> v1::AgentRuntimeHealth {
    let capabilities = probe.capabilities;
    v1::AgentRuntimeHealth {
        configured: true,
        healthy: true,
        protocol_version: u32::from(probe.protocol_version),
        agent_name: probe.agent_name.unwrap_or_default(),
        agent_version: probe.agent_version.unwrap_or_default(),
        auth_methods: probe
            .auth_methods
            .into_iter()
            .map(|method| v1::AgentAuthenticationMethod {
                id: method.id,
                name: method.name,
                description: method.description.unwrap_or_default(),
            })
            .collect(),
        capabilities: Some(v1::AgentRuntimeCapabilities {
            load_session: capabilities.load_session,
            embedded_context: capabilities.embedded_context,
            image_input: capabilities.image_input,
            audio_input: capabilities.audio_input,
            mcp_http: capabilities.mcp_http,
            mcp_sse: capabilities.mcp_sse,
        }),
        reason_code: String::new(),
    }
}

const fn runtime_error_code(kind: AgentRuntimeErrorKind) -> &'static str {
    match kind {
        AgentRuntimeErrorKind::ComponentVerification => "component_verification_failed",
        AgentRuntimeErrorKind::ConfigurationIsolation => "agent_configuration_isolation_failed",
        AgentRuntimeErrorKind::Process => "agent_process_unavailable",
        AgentRuntimeErrorKind::Protocol => "agent_protocol_unavailable",
        AgentRuntimeErrorKind::InvalidRequest => "agent_request_invalid",
        AgentRuntimeErrorKind::Authentication => "agent_authentication_failed",
        AgentRuntimeErrorKind::Permission => "permission_channel_unavailable",
        AgentRuntimeErrorKind::Cancelled => "agent_request_cancelled",
        AgentRuntimeErrorKind::Unavailable => "agent_runtime_unavailable",
    }
}

fn error_result(error: &ApplicationError) -> v1::response::Result {
    let (code, message, retryable) = match error {
        ApplicationError::InvalidInput(message) => {
            (v1::ErrorCode::InvalidArgument, message.clone(), false)
        }
        ApplicationError::NotFound => (v1::ErrorCode::NotFound, "entity not found".into(), false),
        ApplicationError::Conflict => (
            v1::ErrorCode::Conflict,
            "entity changed or idempotency key was reused".into(),
            true,
        ),
        ApplicationError::InvalidState(message) => {
            (v1::ErrorCode::InvalidState, message.clone(), false)
        }
        ApplicationError::Unavailable(_) => (
            v1::ErrorCode::Unavailable,
            "required local dependency is unavailable".into(),
            true,
        ),
        ApplicationError::Integrity(_) => (
            v1::ErrorCode::IntegrityFailure,
            "trusted local component integrity check failed".into(),
            false,
        ),
        ApplicationError::Unauthorized(_) => (
            v1::ErrorCode::Unauthorized,
            "xAI rejected the API key".into(),
            false,
        ),
        ApplicationError::Storage(_) => (
            v1::ErrorCode::Internal,
            "internal storage operation failed".into(),
            false,
        ),
        ApplicationError::DeadlineExceeded => (
            v1::ErrorCode::DeadlineExceeded,
            "daemon request deadline was exceeded".into(),
            true,
        ),
        ApplicationError::Cancelled => (
            v1::ErrorCode::Cancelled,
            "native credential enrollment was cancelled".into(),
            false,
        ),
    };
    v1::response::Result::Error(v1::ErrorResponse {
        code: code as i32,
        message,
        retryable,
    })
}

fn deadline_exceeded_result() -> v1::response::Result {
    v1::response::Result::Error(v1::ErrorResponse {
        code: v1::ErrorCode::DeadlineExceeded as i32,
        message: "daemon request deadline was exceeded".into(),
        retryable: true,
    })
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{
            Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    use grok_application::{
        ApprovalService, ArtifactContentPublication, ArtifactContentPurge,
        ArtifactContentRetention, ArtifactContentStatus, ArtifactContentStore,
        ArtifactImportFailureCode, ArtifactOpenError, ArtifactOpener, ArtifactRetentionFailureCode,
        ArtifactService, ArtifactStore, CancelConversationTurnCommit, ChatModelService,
        ContentPart, ConversationModel, ConversationModelFactory, ConversationRequest,
        ConversationRole, ConversationStream, ConversationThreadCredentialBinding,
        ConversationThreadModelBinding, ConversationTurnEventPage, ConversationTurnReservation,
        ConversationTurnReservationSource, ConversationTurnSnapshot, ConversationTurnStore,
        CreateMessage, CreateProject, CreateRun, CreateThread, CredentialEnrollment,
        CredentialEnrollmentError, CredentialEnrollmentRequest, CredentialEnrollmentService,
        CredentialMutationStore, CredentialService, DEFAULT_XAI_CHAT_MODEL_ID, DeviceAuthorization,
        ExecutionStore, IdGenerator, ModelDescriptor, ModelError, ModelErrorKind,
        ModelFailureCertainty, MutationCommand, NewRunEvent, OAuthFailure, OAuthTokenGrant,
        PRODUCT_CHAT_SYSTEM_PROMPT_V2, PreparedArtifactContent, ProviderStartCommit,
        RequestApproval, SecretName, SecretValue, SecretVault, SelectedSourcePath, StoreError,
        SuperGrokOAuth, TerminalTurnCommit, WorkspaceService, WorkspaceStore, XaiApiKeyValidation,
        XaiApiKeyValidationError, XaiApiKeyValidator,
    };
    use grok_artifact_storage::UnavailableArtifactContent;
    use grok_domain::{
        ApprovalRisk, ApprovalScope, ConversationCitation, ConversationTurn, ConversationTurnEvent,
        ConversationTurnEventKind, ConversationTurnId, ConversationTurnLineage, ConversationUsage,
        EffectId, EffectKind, EffectState, Idempotency, MAX_CONVERSATION_CITATION_TOTAL_BYTES,
        MAX_CONVERSATION_USAGE_VALUE, MAX_MESSAGE_BYTES, Message, MessageId, MessageRole,
        ProjectId, RequestedAction, Run, RunEventKind, RunId, RunState, SideEffect, ThreadId,
    };
    use grok_memory::{
        FixedClock, InMemoryExecutionStore, InMemorySecretVault, SequentialIdGenerator,
    };
    use grok_protocol::v1::{envelope, request, response};
    use prost::Message as _;
    use sha2::{Digest, Sha256};
    use tokio::sync::{Barrier, Notify};

    use super::*;

    #[derive(Debug)]
    struct AcceptXaiKey;

    struct PendingSuperGrokOAuth;

    #[async_trait::async_trait]
    impl SuperGrokOAuth for PendingSuperGrokOAuth {
        async fn begin_device_authorization(
            &self,
            _now_ms: i64,
        ) -> Result<DeviceAuthorization, OAuthFailure> {
            DeviceAuthorization::new(
                "https://accounts.x.ai/device".into(),
                "ABCD-EFGH".into(),
                "secret-device-code".into(),
                i64::try_from(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis(),
                )
                .unwrap()
                .saturating_add(60_000),
                60,
            )
        }

        async fn poll_device_token(
            &self,
            _device_code: &str,
            _now_ms: i64,
        ) -> Result<OAuthTokenGrant, OAuthFailure> {
            Err(OAuthFailure::Pending)
        }

        async fn refresh_token(
            &self,
            _refresh_token: &str,
            _now_ms: i64,
        ) -> Result<OAuthTokenGrant, OAuthFailure> {
            Err(OAuthFailure::Unavailable)
        }
    }

    #[derive(Debug, Default)]
    struct SuccessfulArtifactContent {
        prepare_calls: AtomicUsize,
        publish_calls: AtomicUsize,
        purge_calls: AtomicUsize,
        purge_failures: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl ArtifactContentRetention for SuccessfulArtifactContent {
        async fn purge_content(
            &self,
            _content: &grok_domain::ArtifactVersion,
            _deadline_unix_ms: u64,
        ) -> Result<ArtifactContentPurge, ArtifactRetentionFailureCode> {
            self.purge_calls.fetch_add(1, Ordering::SeqCst);
            if self
                .purge_failures
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                return Err(ArtifactRetentionFailureCode::ContentStoreUnavailable);
            }
            Ok(ArtifactContentPurge::Purged)
        }
    }

    #[async_trait::async_trait]
    impl ArtifactContentStore for SuccessfulArtifactContent {
        async fn prepare_import_content(
            &self,
            _source: &SelectedSourcePath,
            _artifact_id: &ArtifactId,
            _content_version: u32,
            media_type: &str,
            _max_bytes: u64,
            _deadline_unix_ms: u64,
        ) -> Result<PreparedArtifactContent, ArtifactImportFailureCode> {
            self.prepare_calls.fetch_add(1, Ordering::SeqCst);
            Ok(PreparedArtifactContent {
                sha256: Sha256::digest(b"daemon artifact bytes").into(),
                media_type: media_type.into(),
                byte_size: 21,
            })
        }

        async fn publish_content(
            &self,
            _content: &grok_domain::ArtifactVersion,
            _deadline_unix_ms: u64,
        ) -> Result<ArtifactContentPublication, ArtifactImportFailureCode> {
            self.publish_calls.fetch_add(1, Ordering::SeqCst);
            Ok(ArtifactContentPublication::Published)
        }

        async fn content_status(
            &self,
            _content: &grok_domain::ArtifactVersion,
            _deadline_unix_ms: u64,
        ) -> Result<ArtifactContentStatus, ArtifactImportFailureCode> {
            Ok(ArtifactContentStatus::Published)
        }

        async fn discard_prepared_content(
            &self,
            _content: &grok_domain::ArtifactVersion,
        ) -> Result<(), ArtifactImportFailureCode> {
            Ok(())
        }

        async fn discard_reserved_content(
            &self,
            _artifact_id: &ArtifactId,
            _content_version: u32,
        ) -> Result<(), ArtifactImportFailureCode> {
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct SuccessfulArtifactOpener {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl ArtifactOpener for SuccessfulArtifactOpener {
        async fn open_artifact(
            &self,
            _content: &grok_domain::ArtifactVersion,
            _deadline_unix_ms: u64,
        ) -> Result<(), ArtifactOpenError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct GatedArtifactContent {
        prepare_started: Notify,
        release_prepare: Notify,
        purge_started: Notify,
        release_purge: Notify,
        gate_prepare: AtomicBool,
        prepare_calls: AtomicUsize,
        publish_calls: AtomicUsize,
        purge_calls: AtomicUsize,
        purge_failures: AtomicUsize,
    }

    impl GatedArtifactContent {
        fn new() -> Self {
            Self {
                prepare_started: Notify::new(),
                release_prepare: Notify::new(),
                purge_started: Notify::new(),
                release_purge: Notify::new(),
                gate_prepare: AtomicBool::new(true),
                prepare_calls: AtomicUsize::new(0),
                publish_calls: AtomicUsize::new(0),
                purge_calls: AtomicUsize::new(0),
                purge_failures: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl ArtifactContentStore for GatedArtifactContent {
        async fn prepare_import_content(
            &self,
            _source: &SelectedSourcePath,
            _artifact_id: &ArtifactId,
            _content_version: u32,
            media_type: &str,
            _max_bytes: u64,
            _deadline_unix_ms: u64,
        ) -> Result<PreparedArtifactContent, ArtifactImportFailureCode> {
            self.prepare_calls.fetch_add(1, Ordering::SeqCst);
            if self.gate_prepare.load(Ordering::SeqCst) {
                self.prepare_started.notify_one();
                self.release_prepare.notified().await;
            }
            Ok(PreparedArtifactContent {
                sha256: Sha256::digest(b"detached daemon artifact bytes").into(),
                media_type: media_type.into(),
                byte_size: 30,
            })
        }

        async fn publish_content(
            &self,
            _content: &grok_domain::ArtifactVersion,
            _deadline_unix_ms: u64,
        ) -> Result<ArtifactContentPublication, ArtifactImportFailureCode> {
            self.publish_calls.fetch_add(1, Ordering::SeqCst);
            Ok(ArtifactContentPublication::Published)
        }

        async fn content_status(
            &self,
            _content: &grok_domain::ArtifactVersion,
            _deadline_unix_ms: u64,
        ) -> Result<ArtifactContentStatus, ArtifactImportFailureCode> {
            Ok(ArtifactContentStatus::Published)
        }

        async fn discard_prepared_content(
            &self,
            _content: &grok_domain::ArtifactVersion,
        ) -> Result<(), ArtifactImportFailureCode> {
            Ok(())
        }

        async fn discard_reserved_content(
            &self,
            _artifact_id: &ArtifactId,
            _content_version: u32,
        ) -> Result<(), ArtifactImportFailureCode> {
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl ArtifactContentRetention for GatedArtifactContent {
        async fn purge_content(
            &self,
            _content: &grok_domain::ArtifactVersion,
            _deadline_unix_ms: u64,
        ) -> Result<ArtifactContentPurge, ArtifactRetentionFailureCode> {
            let call = self.purge_calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                self.purge_started.notify_one();
                self.release_purge.notified().await;
            }
            if self
                .purge_failures
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                return Err(ArtifactRetentionFailureCode::ContentStoreUnavailable);
            }
            Ok(ArtifactContentPurge::Purged)
        }
    }

    struct GatedArtifactOpener {
        started: Notify,
        release: Notify,
        calls: AtomicUsize,
    }

    impl GatedArtifactOpener {
        fn new() -> Self {
            Self {
                started: Notify::new(),
                release: Notify::new(),
                calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl ArtifactOpener for GatedArtifactOpener {
        async fn open_artifact(
            &self,
            _content: &grok_domain::ArtifactVersion,
            _deadline_unix_ms: u64,
        ) -> Result<(), ArtifactOpenError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.started.notify_one();
            self.release.notified().await;
            Ok(())
        }
    }

    fn seed_test_xai_credential(vault: &InMemorySecretVault) {
        vault
            .set(
                &SecretName::new("xai.api-key.primary").expect("secret name"),
                &SecretValue::new(b"xai-user-key".to_vec()).expect("secret"),
            )
            .expect("configured key");
        vault
            .set(
                &SecretName::new("xai.api-key.local-binding").expect("binding name"),
                &SecretValue::new(format!("xai-binding-{}", "1".repeat(64)).into_bytes())
                    .expect("binding"),
            )
            .expect("configured binding");
    }

    #[derive(Debug)]
    struct SequenceCredentialEnrollment {
        outcomes: Mutex<VecDeque<Result<Vec<u8>, CredentialEnrollmentError>>>,
    }

    impl SequenceCredentialEnrollment {
        fn keys(keys: impl IntoIterator<Item = &'static [u8]>) -> Self {
            Self {
                outcomes: Mutex::new(keys.into_iter().map(|key| Ok(key.to_vec())).collect()),
            }
        }

        fn error(error: CredentialEnrollmentError) -> Self {
            Self {
                outcomes: Mutex::new(VecDeque::from([Err(error)])),
            }
        }
    }

    #[async_trait::async_trait]
    impl CredentialEnrollment for SequenceCredentialEnrollment {
        async fn collect_xai_api_key(
            &self,
            request: CredentialEnrollmentRequest,
        ) -> Result<SecretValue, CredentialEnrollmentError> {
            if request.parent_window_token == 0 {
                return Err(CredentialEnrollmentError::Unavailable);
            }
            let outcome = self
                .outcomes
                .lock()
                .map_err(|_| CredentialEnrollmentError::Unavailable)?
                .pop_front()
                .ok_or(CredentialEnrollmentError::Unavailable)?;
            let bytes = outcome?;
            SecretValue::new(bytes).map_err(|_| CredentialEnrollmentError::Integrity)
        }
    }

    #[async_trait::async_trait]
    impl XaiApiKeyValidator for AcceptXaiKey {
        async fn validate(
            &self,
            _api_key: &SecretValue,
        ) -> Result<XaiApiKeyValidation, XaiApiKeyValidationError> {
            Ok(XaiApiKeyValidation::CapabilitiesResolved)
        }
    }

    #[derive(Debug)]
    struct PendingXaiKey {
        cancelled: Arc<AtomicBool>,
    }

    #[derive(Debug)]
    struct StaticCatalogModel {
        models: Vec<ModelDescriptor>,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl ConversationModel for StaticCatalogModel {
        async fn list_models(&self) -> Result<Vec<ModelDescriptor>, ModelError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.models.clone())
        }

        async fn stream(
            &self,
            _request: ConversationRequest,
        ) -> Result<ConversationStream, ModelError> {
            Err(ModelError {
                kind: ModelErrorKind::Unavailable,
                message: "test model does not execute conversations".into(),
                retryable: false,
                certainty: ModelFailureCertainty::KnownFailure,
            })
        }
    }

    #[derive(Debug)]
    struct StaticCatalogFactory(Arc<StaticCatalogModel>);

    impl ConversationModelFactory for StaticCatalogFactory {
        fn create(&self, _api_key: SecretValue) -> Result<Arc<dyn ConversationModel>, ModelError> {
            Ok(self.0.clone())
        }
    }

    #[derive(Debug)]
    struct PendingConversationModel {
        stream_calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl ConversationModel for PendingConversationModel {
        async fn list_models(&self) -> Result<Vec<ModelDescriptor>, ModelError> {
            Ok(vec![ModelDescriptor {
                id: DEFAULT_XAI_CHAT_MODEL_ID.into(),
                aliases: Vec::new(),
                input_modalities: vec!["text".into()],
                output_modalities: vec!["text".into()],
            }])
        }

        async fn stream(
            &self,
            _request: ConversationRequest,
        ) -> Result<ConversationStream, ModelError> {
            self.stream_calls.fetch_add(1, Ordering::SeqCst);
            std::future::pending().await
        }
    }

    #[derive(Debug)]
    struct PendingConversationFactory(Arc<PendingConversationModel>);

    impl ConversationModelFactory for PendingConversationFactory {
        fn create(&self, _api_key: SecretValue) -> Result<Arc<dyn ConversationModel>, ModelError> {
            Ok(self.0.clone())
        }
    }

    #[derive(Debug)]
    struct RecordingPendingConversationModel {
        list_calls: AtomicUsize,
        stream_calls: AtomicUsize,
        requests: Mutex<Vec<ConversationRequest>>,
    }

    #[async_trait::async_trait]
    impl ConversationModel for RecordingPendingConversationModel {
        async fn list_models(&self) -> Result<Vec<ModelDescriptor>, ModelError> {
            self.list_calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![ModelDescriptor {
                id: DEFAULT_XAI_CHAT_MODEL_ID.into(),
                aliases: Vec::new(),
                input_modalities: vec!["text".into()],
                output_modalities: vec!["text".into()],
            }])
        }

        async fn stream(
            &self,
            request: ConversationRequest,
        ) -> Result<ConversationStream, ModelError> {
            self.stream_calls.fetch_add(1, Ordering::SeqCst);
            self.requests.lock().expect("request lock").push(request);
            std::future::pending().await
        }
    }

    #[derive(Debug)]
    struct RecordingPendingConversationFactory(Arc<RecordingPendingConversationModel>);

    impl ConversationModelFactory for RecordingPendingConversationFactory {
        fn create(&self, _api_key: SecretValue) -> Result<Arc<dyn ConversationModel>, ModelError> {
            Ok(self.0.clone())
        }
    }

    struct BlockingProviderStartStore {
        inner: Arc<InMemoryExecutionStore>,
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
        fail_once: AtomicBool,
        persist_before_failure: bool,
        fail_reconciliation: bool,
        reconciliation_calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl ConversationTurnStore for BlockingProviderStartStore {
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
            self.inner
                .reserve_turn(turn, lineage, source, user_message, run, event, turn_event)
                .await
        }

        async fn load_turn_by_command(
            &self,
            command: &MutationCommand,
        ) -> Result<Option<ConversationTurnSnapshot>, StoreError> {
            self.inner.load_turn_by_command(command).await
        }

        async fn load_turn(
            &self,
            id: &ConversationTurnId,
        ) -> Result<Option<ConversationTurnSnapshot>, StoreError> {
            self.inner.load_turn(id).await
        }

        async fn load_turn_context(
            &self,
            id: &ConversationTurnId,
        ) -> Result<Vec<Message>, StoreError> {
            self.inner.load_turn_context(id).await
        }

        async fn commit_provider_start(
            &self,
            commit: ProviderStartCommit,
        ) -> Result<ConversationTurnSnapshot, StoreError> {
            if self.fail_once.swap(false, Ordering::SeqCst) {
                self.entered.wait().await;
                self.release.wait().await;
                if self.persist_before_failure {
                    self.inner.commit_provider_start(commit).await?;
                }
                return Err(StoreError::Internal(
                    "injected pre-provider persistence failure".into(),
                ));
            }
            self.inner.commit_provider_start(commit).await
        }

        async fn commit_terminal(
            &self,
            commit: TerminalTurnCommit,
        ) -> Result<ConversationTurnSnapshot, StoreError> {
            self.inner.commit_terminal(commit).await
        }

        async fn commit_cancellation(
            &self,
            commit: CancelConversationTurnCommit,
        ) -> Result<ConversationTurnSnapshot, StoreError> {
            self.inner.commit_cancellation(commit).await
        }

        async fn commit_dispatch_exit_reconciliation(
            &self,
            commit: CancelConversationTurnCommit,
        ) -> Result<ConversationTurnSnapshot, StoreError> {
            self.reconciliation_calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_reconciliation {
                return Err(StoreError::Internal(
                    "injected task-exit reconciliation failure".into(),
                ));
            }
            self.inner.commit_dispatch_exit_reconciliation(commit).await
        }

        async fn append_turn_text(
            &self,
            turn_id: &ConversationTurnId,
            expected_turn_revision: u64,
            start_utf8_offset: u64,
            text: String,
        ) -> Result<Vec<ConversationTurnEvent>, StoreError> {
            self.inner
                .append_turn_text(turn_id, expected_turn_revision, start_utf8_offset, text)
                .await
        }

        async fn list_turn_events_since(
            &self,
            turn_id: &ConversationTurnId,
            after_sequence: u64,
            limit: usize,
        ) -> Result<ConversationTurnEventPage, StoreError> {
            self.inner
                .list_turn_events_since(turn_id, after_sequence, limit)
                .await
        }

        async fn list_incomplete_turns_for_recovery(
            &self,
            limit: usize,
        ) -> Result<Vec<ConversationTurnSnapshot>, StoreError> {
            self.inner.list_incomplete_turns_for_recovery(limit).await
        }

        async fn list_thread_turns(
            &self,
            thread_id: &ThreadId,
            after: Option<&ConversationTurnId>,
            limit: usize,
        ) -> Result<Vec<ConversationTurnSnapshot>, StoreError> {
            self.inner.list_thread_turns(thread_id, after, limit).await
        }

        async fn retry_source_is_latest(
            &self,
            id: &ConversationTurnId,
        ) -> Result<bool, StoreError> {
            self.inner.retry_source_is_latest(id).await
        }

        async fn thread_credential_binding(
            &self,
            thread_id: &ThreadId,
        ) -> Result<ConversationThreadCredentialBinding, StoreError> {
            self.inner.thread_credential_binding(thread_id).await
        }

        async fn thread_model_binding(
            &self,
            thread_id: &ThreadId,
        ) -> Result<ConversationThreadModelBinding, StoreError> {
            self.inner.thread_model_binding(thread_id).await
        }
    }

    async fn daemon_with_pending_conversation() -> (Daemon, [String; 2], Arc<AtomicUsize>) {
        let store = Arc::new(InMemoryExecutionStore::new());
        daemon_with_pending_conversation_store(store.clone(), store).await
    }

    async fn daemon_with_pending_conversation_store(
        store: Arc<InMemoryExecutionStore>,
        conversation_store: Arc<dyn ConversationTurnStore>,
    ) -> (Daemon, [String; 2], Arc<AtomicUsize>) {
        let vault = Arc::new(InMemorySecretVault::new());
        seed_test_xai_credential(&vault);
        let clock = Arc::new(FixedClock::new(10));
        let ids: Arc<dyn IdGenerator> = Arc::new(SequentialIdGenerator::new());
        let runs = Arc::new(RunService::new(store.clone(), clock.clone(), ids.clone()));
        let approvals = Arc::new(ApprovalService::new(
            store.clone(),
            clock.clone(),
            ids.clone(),
        ));
        let workspace = Arc::new(WorkspaceService::new(
            store.clone(),
            clock.clone(),
            ids.clone(),
        ));
        let project = workspace
            .create_project(
                CreateProject {
                    name: "Async conversation".into(),
                    description: String::new(),
                },
                "async-conversation-project",
            )
            .await
            .expect("conversation project");
        let thread = workspace
            .create_thread(
                CreateThread {
                    project_id: project.id.to_string(),
                    title: "Async conversation".into(),
                },
                "async-conversation-thread",
            )
            .await
            .expect("conversation thread");
        let second_thread = workspace
            .create_thread(
                CreateThread {
                    project_id: project.id.to_string(),
                    title: "Saturated conversation".into(),
                },
                "saturated-conversation-thread",
            )
            .await
            .expect("second conversation thread");
        let credentials = Arc::new(CredentialService::new(
            vault,
            store.clone(),
            Arc::new(AcceptXaiKey),
        ));
        let stream_calls = Arc::new(AtomicUsize::new(0));
        let factory: Arc<dyn ConversationModelFactory> = Arc::new(PendingConversationFactory(
            Arc::new(PendingConversationModel {
                stream_calls: stream_calls.clone(),
            }),
        ));
        let conversation = Arc::new(ConversationService::new(
            conversation_store,
            workspace.clone(),
            credentials.clone(),
            factory,
            clock.clone(),
            ids,
            store,
        ));
        (
            Daemon::new(
                runs,
                approvals,
                credentials,
                clock,
                [7; 32],
                "instance-conversation".into(),
            )
            .with_workspace(workspace)
            .with_conversation(conversation),
            [thread.id.to_string(), second_thread.id.to_string()],
            stream_calls,
        )
    }

    async fn daemon_with_completed_fork_source() -> (
        Daemon,
        ConversationTurnSnapshot,
        Arc<InMemoryExecutionStore>,
        Arc<RecordingPendingConversationModel>,
    ) {
        let store = Arc::new(InMemoryExecutionStore::new());
        let vault = Arc::new(InMemorySecretVault::new());
        seed_test_xai_credential(&vault);
        let clock = Arc::new(FixedClock::new(10));
        let ids: Arc<dyn IdGenerator> = Arc::new(SequentialIdGenerator::new());
        let runs = Arc::new(RunService::new(store.clone(), clock.clone(), ids.clone()));
        let approvals = Arc::new(ApprovalService::new(
            store.clone(),
            clock.clone(),
            ids.clone(),
        ));
        let workspace = Arc::new(WorkspaceService::new(
            store.clone(),
            clock.clone(),
            ids.clone(),
        ));
        let project = workspace
            .create_project(
                CreateProject {
                    name: "Fork handler".into(),
                    description: String::new(),
                },
                "fork-handler-project",
            )
            .await
            .expect("fork project");
        let thread = workspace
            .create_thread(
                CreateThread {
                    project_id: project.id.to_string(),
                    title: "Fork source".into(),
                },
                "fork-handler-thread",
            )
            .await
            .expect("fork thread");
        let credentials = Arc::new(CredentialService::new(
            vault,
            store.clone(),
            Arc::new(AcceptXaiKey),
        ));
        let model = Arc::new(RecordingPendingConversationModel {
            list_calls: AtomicUsize::new(0),
            stream_calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
        });
        let factory: Arc<dyn ConversationModelFactory> =
            Arc::new(RecordingPendingConversationFactory(model.clone()));
        let conversation = Arc::new(ConversationService::new(
            store.clone(),
            workspace.clone(),
            credentials.clone(),
            factory,
            clock.clone(),
            ids,
            store.clone(),
        ));
        let reserved = conversation
            .start(
                StartConversationTurn {
                    thread_id: thread.id.to_string(),
                    content: "Canonical source prompt".into(),
                    model_id: None,
                    search_enabled: false,
                },
                "fork-handler-source",
                Box::pin(std::future::pending()),
            )
            .await
            .expect("source reservation");
        let source = complete_fork_source(&store, reserved.snapshot).await;
        drop(reserved.dispatch);
        clock.set(20);
        (
            Daemon::new(
                runs,
                approvals,
                credentials,
                clock,
                [7; 32],
                "instance-forks".into(),
            )
            .with_workspace(workspace)
            .with_conversation(conversation),
            source,
            store,
            model,
        )
    }

    #[allow(clippy::too_many_lines)]
    async fn complete_fork_source(
        store: &InMemoryExecutionStore,
        reserved: ConversationTurnSnapshot,
    ) -> ConversationTurnSnapshot {
        let context = store
            .load_turn_context(&reserved.turn.id)
            .await
            .expect("source context");
        let transition_at = reserved.turn.updated_at + 1;
        let mut turn = reserved.turn.clone();
        let mut run = reserved.run.clone();
        run.transition(RunState::Planning, transition_at)
            .expect("source planning");
        run.transition(RunState::Running, transition_at)
            .expect("source running");
        let mut effect = SideEffect::prepare(
            EffectId::new("fork-handler-source-effect").expect("source effect ID"),
            run.id.clone(),
            EffectKind::ExternalMutation,
            format!("official xAI Responses API model {}", turn.model_id),
            Idempotency::NonIdempotent,
            transition_at,
        );
        effect.start(transition_at).expect("source effect start");
        turn.start_provider(
            effect.id.clone(),
            test_provider_request_fingerprint(&turn.model_id, &context),
            transition_at,
        )
        .expect("source provider start");
        let started = store
            .commit_provider_start(ProviderStartCommit {
                turn,
                expected_turn_revision: reserved.turn.revision,
                run,
                expected_run_revision: reserved.run.revision,
                effect,
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
                            effect_id: EffectId::new("fork-handler-source-effect")
                                .expect("source effect ID"),
                        },
                    },
                ],
                turn_event: ConversationTurnEventKind::StateChanged {
                    from: ConversationTurnState::Reserved,
                    to: ConversationTurnState::ProviderStarted,
                },
            })
            .await
            .expect("source provider start commit");
        store
            .append_turn_text(
                &started.turn.id,
                started.turn.revision,
                0,
                "Canonical source answer".into(),
            )
            .await
            .expect("source assistant text");

        let completed_at = started.turn.updated_at + 1;
        let mut turn = started.turn.clone();
        let mut run = started.run.clone();
        let mut effect = started.effect.clone().expect("source provider effect");
        let assistant = Message::new(
            grok_domain::MessageId::new("fork-handler-source-assistant")
                .expect("source assistant ID"),
            turn.thread_id.clone(),
            MessageRole::Assistant,
            "Canonical source answer".into(),
            completed_at,
        )
        .expect("source assistant");
        turn.complete(
            assistant.id.clone(),
            Some("fork-handler-source-response".into()),
            vec![ConversationCitation {
                title: Some("Canonical source".into()),
                url: "https://example.test/canonical-source".into(),
            }],
            ConversationUsage {
                input_tokens: 11,
                output_tokens: 7,
                cost_in_usd_ticks: 19,
            },
            Some(true),
            completed_at,
        )
        .expect("source completion");
        run.transition(RunState::Completed, completed_at)
            .expect("source run completion");
        effect
            .finish(true, completed_at)
            .expect("source effect finish");
        store
            .commit_terminal(TerminalTurnCommit {
                turn,
                expected_turn_revision: started.turn.revision,
                run,
                expected_run_revision: started.run.revision,
                expected_effect_revision: started.effect.as_ref().map(|value| value.revision),
                effect: Some(effect),
                assistant_message: Some(assistant),
                events: vec![NewRunEvent {
                    occurred_at: completed_at,
                    kind: RunEventKind::StateChanged {
                        from: RunState::Running,
                        to: RunState::Completed,
                    },
                }],
                turn_event: ConversationTurnEventKind::StateChanged {
                    from: ConversationTurnState::ProviderStarted,
                    to: ConversationTurnState::Completed,
                },
            })
            .await
            .expect("source terminal commit")
    }

    fn test_provider_request_fingerprint(model_id: &str, context: &[Message]) -> [u8; 32] {
        fn hash_part(hasher: &mut Sha256, value: &[u8]) {
            hasher.update(
                u64::try_from(value.len())
                    .expect("test hash-part length")
                    .to_be_bytes(),
            );
            hasher.update(value);
        }
        let mut hasher = Sha256::new();
        hash_part(&mut hasher, model_id.as_bytes());
        hash_part(&mut hasher, &[0]);
        hash_part(&mut hasher, b"continuation:none");
        hash_part(&mut hasher, b"system");
        hash_part(&mut hasher, b"text");
        hash_part(&mut hasher, PRODUCT_CHAT_SYSTEM_PROMPT_V2.as_bytes());
        for message in context {
            hash_part(
                &mut hasher,
                match message.role {
                    MessageRole::System => b"system",
                    MessageRole::User => b"user",
                    MessageRole::Assistant => b"assistant",
                },
            );
            hash_part(&mut hasher, b"text");
            hash_part(&mut hasher, message.content.as_bytes());
        }
        hasher.finalize().into()
    }

    async fn daemon_with_blocked_provider_start(
        persist_before_failure: bool,
        fail_reconciliation: bool,
    ) -> (
        Daemon,
        [String; 2],
        Arc<AtomicUsize>,
        Arc<InMemoryExecutionStore>,
        Arc<Barrier>,
        Arc<Barrier>,
        Arc<AtomicUsize>,
    ) {
        let inner = Arc::new(InMemoryExecutionStore::new());
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let reconciliation_calls = Arc::new(AtomicUsize::new(0));
        let store: Arc<dyn ConversationTurnStore> = Arc::new(BlockingProviderStartStore {
            inner: inner.clone(),
            entered: entered.clone(),
            release: release.clone(),
            fail_once: AtomicBool::new(true),
            persist_before_failure,
            fail_reconciliation,
            reconciliation_calls: reconciliation_calls.clone(),
        });
        let (daemon, thread_ids, stream_calls) =
            daemon_with_pending_conversation_store(inner.clone(), store).await;
        (
            daemon,
            thread_ids,
            stream_calls,
            inner,
            entered,
            release,
            reconciliation_calls,
        )
    }

    fn daemon_with_model_catalog() -> (Daemon, Arc<AtomicUsize>) {
        let store = Arc::new(InMemoryExecutionStore::new());
        let vault = Arc::new(InMemorySecretVault::new());
        seed_test_xai_credential(&vault);
        let clock = Arc::new(FixedClock::new(10));
        let ids: Arc<dyn IdGenerator> = Arc::new(SequentialIdGenerator::new());
        let runs = Arc::new(RunService::new(store.clone(), clock.clone(), ids.clone()));
        let approvals = Arc::new(ApprovalService::new(
            store.clone(),
            clock.clone(),
            ids.clone(),
        ));
        let workspace = Arc::new(WorkspaceService::new(store.clone(), clock.clone(), ids));
        let credentials = Arc::new(CredentialService::new(
            vault,
            store.clone(),
            Arc::new(AcceptXaiKey),
        ));
        let calls = Arc::new(AtomicUsize::new(0));
        let factory: Arc<dyn ConversationModelFactory> =
            Arc::new(StaticCatalogFactory(Arc::new(StaticCatalogModel {
                models: vec![ModelDescriptor {
                    id: "grok-alternative".into(),
                    aliases: vec!["grok-current".into()],
                    input_modalities: vec!["text".into()],
                    output_modalities: vec!["text".into()],
                }],
                calls: calls.clone(),
            })));
        let chat_models = Arc::new(ChatModelService::new(
            store.clone(),
            credentials.clone(),
            factory,
            clock.clone(),
        ));
        (
            Daemon::new(
                runs,
                approvals,
                credentials,
                clock,
                [7; 32],
                "instance-models".into(),
            )
            .with_workspace(workspace)
            .with_desktop_preferences(Arc::new(DesktopPreferencesService::new(
                store,
                Arc::new(FixedClock::new(10)),
            )))
            .with_chat_models(chat_models)
            .with_runtime_capability_facts(CapabilityFacts {
                online: true,
                ..CapabilityFacts::default()
            }),
            calls,
        )
    }

    struct CancellationProbe(Arc<AtomicBool>);

    impl Drop for CancellationProbe {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[async_trait::async_trait]
    impl XaiApiKeyValidator for PendingXaiKey {
        async fn validate(
            &self,
            _api_key: &SecretValue,
        ) -> Result<XaiApiKeyValidation, XaiApiKeyValidationError> {
            let _probe = CancellationProbe(self.cancelled.clone());
            std::future::pending().await
        }
    }

    fn daemon() -> (Daemon, Arc<FixedClock>) {
        daemon_with_credentials(Arc::new(InMemorySecretVault::new()), Arc::new(AcceptXaiKey))
    }

    fn daemon_with_pending_supergrok() -> (Daemon, Arc<FixedClock>) {
        let (daemon, clock) = daemon();
        let service = SuperGrokEnrollmentService::new(
            Arc::new(PendingSuperGrokOAuth),
            Arc::new(InMemorySecretVault::new()),
        )
        .expect("SuperGrok service");
        (
            daemon.with_supergrok_enrollment(
                Arc::new(service),
                Arc::new(grok_application::ChatRailSelection::new(
                    grok_domain::ChatRail::XaiApiKey,
                )),
            ),
            clock,
        )
    }

    fn daemon_with_credentials(
        vault: Arc<InMemorySecretVault>,
        validator: Arc<dyn XaiApiKeyValidator>,
    ) -> (Daemon, Arc<FixedClock>) {
        let store = Arc::new(InMemoryExecutionStore::new());
        daemon_with_credentials_and_store(vault, validator, store)
    }

    fn daemon_with_credentials_and_store(
        vault: Arc<InMemorySecretVault>,
        validator: Arc<dyn XaiApiKeyValidator>,
        store: Arc<InMemoryExecutionStore>,
    ) -> (Daemon, Arc<FixedClock>) {
        daemon_with_credentials_store_and_enrollment(
            vault,
            validator,
            store,
            Arc::new(SequenceCredentialEnrollment::keys([
                b"xai-user-owned-key".as_slice()
            ])),
        )
    }

    fn daemon_with_credentials_store_and_enrollment(
        vault: Arc<InMemorySecretVault>,
        validator: Arc<dyn XaiApiKeyValidator>,
        store: Arc<InMemoryExecutionStore>,
        enrollment: Arc<dyn CredentialEnrollment>,
    ) -> (Daemon, Arc<FixedClock>) {
        let execution_store: Arc<dyn ExecutionStore> = store.clone();
        let credential_store: Arc<dyn CredentialMutationStore> = store.clone();
        let workspace_store: Arc<dyn WorkspaceStore> = store.clone();
        let artifact_store: Arc<dyn ArtifactStore> = store.clone();
        let clock = Arc::new(FixedClock::new(10));
        let ids: Arc<dyn IdGenerator> = Arc::new(SequentialIdGenerator::new());
        let runs = Arc::new(RunService::new(
            execution_store.clone(),
            clock.clone(),
            ids.clone(),
        ));
        let approvals = Arc::new(ApprovalService::new(
            execution_store,
            clock.clone(),
            ids.clone(),
        ));
        let workspace = Arc::new(WorkspaceService::new(
            workspace_store.clone(),
            clock.clone(),
            ids.clone(),
        ));
        let unavailable = Arc::new(UnavailableArtifactContent);
        let artifacts = Arc::new(ArtifactService::new(
            artifact_store,
            unavailable.clone(),
            unavailable,
            workspace_store,
            clock.clone(),
            ids,
        ));
        let desktop_preferences =
            Arc::new(DesktopPreferencesService::new(store.clone(), clock.clone()));
        let credentials = Arc::new(CredentialService::new(
            vault,
            credential_store.clone(),
            validator,
        ));
        let enrollment = Arc::new(CredentialEnrollmentService::new(
            credentials.clone(),
            credential_store,
            enrollment,
        ));
        (
            Daemon::new(
                runs,
                approvals,
                credentials,
                clock.clone(),
                [7; 32],
                "instance-1".into(),
            )
            .with_workspace(workspace)
            .with_artifacts(artifacts, false, false)
            .with_desktop_preferences(desktop_preferences)
            .with_host_execution_policy(store)
            .with_credential_enrollment(enrollment),
            clock,
        )
    }

    fn daemon_with_artifact_adapters<T>(
        content: Arc<T>,
        opener: Arc<dyn ArtifactOpener>,
    ) -> (Daemon, Arc<FixedClock>, Arc<InMemoryExecutionStore>)
    where
        T: ArtifactContentStore + ArtifactContentRetention + 'static,
    {
        let store = Arc::new(InMemoryExecutionStore::new());
        let (daemon, clock) = daemon_with_credentials_and_store(
            Arc::new(InMemorySecretVault::new()),
            Arc::new(AcceptXaiKey),
            store.clone(),
        );
        let artifacts = Arc::new(
            ArtifactService::new(
                store.clone(),
                content.clone(),
                opener,
                store.clone(),
                clock.clone(),
                Arc::new(SequentialIdGenerator::new()),
            )
            .with_content_retention(content),
        );
        (
            daemon
                .with_artifacts(artifacts, true, true)
                .with_runtime_capability_facts(CapabilityFacts {
                    artifact_content_ready: true,
                    ..CapabilityFacts::default()
                }),
            clock,
            store,
        )
    }

    fn request(operation: request::Operation) -> v1::Envelope {
        v1::Envelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: "request-1".into(),
            startup_nonce: vec![7; 32],
            deadline_unix_ms: 100,
            idempotency_key: "command-1".into(),
            payload: Some(envelope::Payload::Request(v1::Request {
                operation: Some(operation),
            })),
        }
    }

    fn legacy_single_string_request(tag: [u8; 2], value: &str) -> Vec<u8> {
        let value_len = u8::try_from(value.len()).expect("bounded legacy string");
        let payload_len = value_len.checked_add(2).expect("bounded legacy payload");
        let mut encoded = Vec::from(tag);
        encoded.extend_from_slice(&[payload_len, 0x0a, value_len]);
        encoded.extend_from_slice(value.as_bytes());
        encoded
    }

    fn legacy_create_message_request(thread_id: &str, content: &str) -> Vec<u8> {
        let thread_len = u8::try_from(thread_id.len()).expect("bounded legacy thread ID");
        let content_len = u8::try_from(content.len()).expect("bounded legacy message content");
        let mut payload = vec![0x0a, thread_len];
        payload.extend_from_slice(thread_id.as_bytes());
        payload.extend_from_slice(&[0x10, v1::MessageRole::User as u8, 0x1a, content_len]);
        payload.extend_from_slice(content.as_bytes());
        let payload_len = u8::try_from(payload.len()).expect("bounded legacy create payload");
        let mut encoded = vec![0x92, 0x01, payload_len];
        encoded.extend_from_slice(&payload);
        encoded
    }

    fn legacy_update_message_request(message_id: &str, content: &str) -> Vec<u8> {
        let message_len = u8::try_from(message_id.len()).expect("bounded legacy message ID");
        let content_len = u8::try_from(content.len()).expect("bounded legacy message content");
        let mut payload = vec![0x0a, message_len];
        payload.extend_from_slice(message_id.as_bytes());
        // expected_revision=0 is the seeded message's current revision and is
        // omitted by canonical proto3 encoding.
        payload.extend_from_slice(&[0x1a, content_len]);
        payload.extend_from_slice(content.as_bytes());
        let payload_len = u8::try_from(payload.len()).expect("bounded legacy update payload");
        let mut encoded = vec![0x9a, 0x01, payload_len];
        encoded.extend_from_slice(&payload);
        encoded
    }

    fn legacy_automation_request(
        outer_tag: [u8; 2],
        mut payload: Vec<u8>,
        enabled_tag: u8,
    ) -> v1::Request {
        // Append the removed nested proto3 bool as `true` before wrapping the
        // operation. Keeping this fixture below the one-byte varint boundary
        // makes the old wire shape explicit and reviewable.
        payload.extend_from_slice(&[enabled_tag, 0x01]);
        let payload_len = u8::try_from(payload.len()).expect("bounded legacy automation payload");
        assert!(payload_len < 0x80);
        let mut encoded = Vec::from(outer_tag);
        encoded.push(payload_len);
        encoded.extend_from_slice(&payload);
        v1::Request::decode(encoded.as_slice()).expect("legacy automation request")
    }

    fn maximum_wire_conversation_turn(id: &str) -> v1::ConversationTurnResult {
        const CITATION_COUNT: usize = 125;
        const CITATION_URL_BYTES: usize = MAX_CONVERSATION_CITATION_TOTAL_BYTES / CITATION_COUNT;
        let citation_url = format!(
            "https://example.test/{}",
            "x".repeat(CITATION_URL_BYTES - "https://example.test/".len())
        );
        let citations = (0..CITATION_COUNT)
            .map(|_| v1::ConversationCitation {
                title: String::new(),
                url: citation_url.clone(),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            citations
                .iter()
                .map(|citation| citation.url.len())
                .sum::<usize>(),
            MAX_CONVERSATION_CITATION_TOTAL_BYTES
        );
        let message = |suffix: &str, sequence, role| v1::Message {
            id: format!("message-{id}-{suffix}"),
            thread_id: "thread-1".into(),
            sequence,
            role: role as i32,
            content: "m".repeat(MAX_MESSAGE_BYTES),
            state: v1::MessageState::Active as i32,
            revision: 0,
            created_at_unix_ms: 1,
            updated_at_unix_ms: 1,
            derivation: Some(v1::ConversationMessageDerivation {
                origin: Some(v1::conversation_message_derivation::Origin::Original(
                    v1::ConversationOriginalMessageDerivation {},
                )),
            }),
        };
        v1::ConversationTurnResult {
            turn_id: id.into(),
            state: v1::ConversationTurnState::Completed as i32,
            model_id: "m".repeat(512),
            search_enabled: false,
            user_message: Some(message("user", 1, v1::MessageRole::User)),
            assistant_message: Some(message("assistant", 2, v1::MessageRole::Assistant)),
            run: Some(v1::Run {
                id: format!("run-{id}"),
                project_id: "project-1".into(),
                thread_id: "thread-1".into(),
                state: v1::RunState::Completed as i32,
                revision: 2,
                created_at_unix_ms: 1,
                updated_at_unix_ms: 2,
                kind: v1::RunKind::Chat as i32,
                work_backend: v1::WorkExecutionBackend::Unspecified as i32,
            }),
            failure: None,
            citations,
            usage: Some(v1::ConversationUsage {
                input_tokens: MAX_CONVERSATION_USAGE_VALUE,
                output_tokens: MAX_CONVERSATION_USAGE_VALUE,
                cost_in_usd_ticks: MAX_CONVERSATION_USAGE_VALUE,
            }),
            zero_data_retention: Some(true),
            revision: 2,
            lineage: Some(v1::ConversationTurnLineage {
                origin: v1::ConversationTurnOrigin::Original as i32,
                source_turn_id: String::new(),
                retry_depth: 0,
            }),
            retry_eligibility: v1::ConversationRetryEligibility::SourceCompleted as i32,
        }
    }

    fn conversation_page_envelope(turns: Vec<v1::ConversationTurnResult>) -> v1::Envelope {
        v1::Envelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: "request-frame-bound".into(),
            startup_nonce: vec![7; 32],
            deadline_unix_ms: 0,
            idempotency_key: String::new(),
            payload: Some(envelope::Payload::Response(v1::Response {
                result: Some(response::Result::ConversationTurns(
                    v1::ConversationTurnList {
                        turns,
                        next_cursor: String::new(),
                    },
                )),
            })),
        }
    }

    #[test]
    fn conversation_turn_pages_fit_the_exact_transport_frame_budget() {
        let maximum = maximum_wire_conversation_turn("turn-1");
        let single = conversation_page_envelope(vec![maximum.clone()]);
        assert!(single.encoded_len() <= MAX_FRAME_BYTES);

        let mut multiple =
            conversation_page_envelope(vec![maximum, maximum_wire_conversation_turn("turn-2")]);
        assert!(multiple.encoded_len() > MAX_FRAME_BYTES);
        bound_response_to_frame(&mut multiple);
        assert!(multiple.encoded_len() <= MAX_FRAME_BYTES);

        let Some(envelope::Payload::Response(response)) = multiple.payload else {
            panic!("response payload")
        };
        let Some(response::Result::ConversationTurns(page)) = response.result else {
            panic!("conversation page")
        };
        assert_eq!(page.turns.len(), 1);
        assert_eq!(page.next_cursor, "turn-1");
    }

    #[tokio::test]
    async fn conversation_task_registry_is_bounded_deduplicated_and_signalled() {
        let registry = ConversationTaskRegistry::new(1);
        let permit = registry.try_acquire().expect("first task slot");
        assert_eq!(registry.available_slots(), 0);
        assert!(matches!(
            registry.try_acquire(),
            Err(ApplicationError::Unavailable(_))
        ));

        let turn_id = ConversationTurnId::new("turn-registry").expect("turn id");
        let registration = registry
            .register(turn_id.clone())
            .await
            .expect("first registration");
        let first_generation = registration.generation;
        assert!(registry.register(turn_id.clone()).await.is_none());
        assert_eq!(registry.active_count().await, 1);
        assert!(registry.signal(&turn_id).await);
        registration.cancel.await.expect("cooperative cancellation");
        assert_eq!(registry.active_count().await, 0);

        let replacement = registry
            .register(turn_id.clone())
            .await
            .expect("replacement registration");
        registry.finish(&turn_id, first_generation).await;
        assert_eq!(registry.active_count().await, 1);
        assert!(registry.signal(&turn_id).await);
        replacement.cancel.await.expect("replacement cancellation");

        drop(permit);
        assert_eq!(registry.available_slots(), 1);
        assert!(registry.try_acquire().is_ok());
    }

    #[tokio::test]
    async fn health_response_is_correlated_and_versioned() {
        let (daemon, _) = daemon();
        let response = daemon
            .handle(request(request::Operation::Health(v1::HealthRequest {})))
            .await
            .expect("health");
        assert_eq!(response.request_id, "request-1");
        let envelope::Payload::Response(response) = response.payload.expect("payload") else {
            panic!("response")
        };
        let Some(response::Result::Health(health)) = response.result else {
            panic!("health response")
        };
        assert_eq!(health.protocol_version, PROTOCOL_VERSION);
        assert_eq!(
            health.automation_scheduler,
            v1::AutomationSchedulerHealth::DegradedExecutionDisabled as i32
        );
        let agent = health.agent_runtime.expect("agent health");
        assert!(!agent.configured);
        assert!(!agent.healthy);
        assert_eq!(agent.reason_code, "not_configured");
    }

    #[tokio::test]
    async fn scheduler_health_states_never_arm_execution_in_epoch_twenty() {
        for (lifecycle, expected) in [
            (
                AutomationSchedulerLifecycle::KernelInitializedExecutionDisabled,
                v1::AutomationSchedulerHealth::KernelInitializedExecutionDisabled,
            ),
            (
                AutomationSchedulerLifecycle::KernelInitializedExecutionEnabled,
                v1::AutomationSchedulerHealth::KernelInitializedExecutionDisabled,
            ),
            (
                AutomationSchedulerLifecycle::RecoveryPendingExecutionDisabled,
                v1::AutomationSchedulerHealth::RecoveryPendingExecutionDisabled,
            ),
            (
                AutomationSchedulerLifecycle::DegradedExecutionDisabled,
                v1::AutomationSchedulerHealth::DegradedExecutionDisabled,
            ),
        ] {
            let (daemon, clock) = daemon();
            let scheduler_store = Arc::new(InMemoryExecutionStore::new());
            let scheduler = Arc::new(AutomationSchedulerService::new(
                scheduler_store.clone(),
                clock,
                Arc::new(SequentialIdGenerator::new()),
            ));
            let daemon = daemon.with_automation_scheduler(scheduler, lifecycle);
            let health = daemon
                .handle(request(request::Operation::Health(v1::HealthRequest {})))
                .await
                .expect("scheduler health");
            let envelope::Payload::Response(response) = health.payload.expect("health payload")
            else {
                panic!("health response")
            };
            let Some(response::Result::Health(health)) = response.result else {
                panic!("scheduler health result")
            };
            assert_eq!(health.automation_scheduler, expected as i32);

            let capabilities = daemon
                .handle(request(request::Operation::ResolveCapabilities(
                    v1::ResolveCapabilitiesRequest::default(),
                )))
                .await
                .expect("capabilities");
            assert_eq!(
                capability_availability(&capabilities, v1::Capability::Automations),
                v1::CapabilityAvailability::Limited as i32
            );
            assert_eq!(
                capability_availability(&capabilities, v1::Capability::Chat),
                v1::CapabilityAvailability::Limited as i32
            );

            let journal =
                grok_application::AutomationSchedulerStore::automation_scheduler_journal_status(
                    scheduler_store.as_ref(),
                )
                .await
                .expect("journal status");
            assert!(journal.lease.is_none());
            assert_eq!(journal.cursor_count, 0);
            assert_eq!(journal.pending_count, 0);
            assert_eq!(journal.claimed_count, 0);
            assert_eq!(journal.run_linked_count, 0);
        }
    }

    #[tokio::test]
    async fn supergrok_device_enrollment_exposes_only_non_secret_state_and_cancels() {
        let (daemon, _) = daemon_with_pending_supergrok();
        let response = daemon
            .handle(request(request::Operation::BeginSupergrokDeviceEnrollment(
                v1::BeginSuperGrokDeviceEnrollmentRequest {},
            )))
            .await
            .expect("begin response");
        let envelope::Payload::Response(response) = response.payload.expect("payload") else {
            panic!("response payload");
        };
        let Some(response::Result::SupergrokEnrollmentStatus(status)) = response.result else {
            panic!("enrollment status");
        };
        assert_eq!(status.state, "awaiting_user");
        assert_eq!(status.verification_uri, "https://accounts.x.ai/device");
        assert_eq!(status.user_code, "ABCD-EFGH");
        assert!(status.expires_at_unix_ms > 0);
        assert_eq!(status.credential_generation, 0);
        assert!(status.reason_code.is_empty());
        let encoded = status.encode_to_vec();
        assert!(
            !encoded
                .windows(b"secret-device-code".len())
                .any(|window| window == b"secret-device-code")
        );

        let response = daemon
            .handle(request(request::Operation::CancelSupergrokEnrollment(
                v1::CancelSuperGrokEnrollmentRequest {},
            )))
            .await
            .expect("cancel response");
        let envelope::Payload::Response(response) = response.payload.expect("payload") else {
            panic!("response payload");
        };
        let Some(response::Result::SupergrokEnrollmentStatus(status)) = response.result else {
            panic!("enrollment status");
        };
        assert_eq!(status.state, "disconnected");
        assert!(status.verification_uri.is_empty());
        assert!(status.user_code.is_empty());
    }

    #[tokio::test]
    async fn epoch_twenty_managed_integration_mutations_fail_closed() {
        let (daemon, _) = daemon();
        let error = daemon
            .change_managed_integration(v1::ChangeManagedIntegrationRequest {
                integration_id: "desktop.grok.wisp".into(),
                action: "install".into(),
                expected_revision: 0,
            })
            .await
            .expect_err("managed lifecycle mutation must remain unavailable");

        assert!(matches!(error, ApplicationError::Unavailable(_)));
    }

    #[tokio::test]
    async fn removed_legacy_enable_bits_cannot_enable_automation_definitions() {
        let (daemon, _) = daemon();
        let project = daemon
            .workspace()
            .expect("workspace")
            .create_project(
                CreateProject {
                    name: "Legacy automation".into(),
                    description: String::new(),
                },
                "legacy-automation-project",
            )
            .await
            .expect("project");
        let create = v1::CreateAutomationRequest {
            project_id: project.id.to_string(),
            title: "Brief".into(),
            prompt: "Summarize".into(),
            schedule: "v1;daily;09:00".into(),
            timezone: "UTC".into(),
            missed_run_policy: v1::MissedRunPolicy::Skip as i32,
            overlap_policy: v1::OverlapPolicy::Skip as i32,
            schedule_active: false,
        };
        // Request field 28, with removed CreateAutomationRequest field 8.
        let create = legacy_automation_request([0xe2, 0x01], create.encode_to_vec(), 0x40);
        let mut create_envelope = request(request::Operation::Health(v1::HealthRequest {}));
        create_envelope.idempotency_key = "legacy-enable-create".into();
        create_envelope.payload = Some(envelope::Payload::Request(create));
        let created = daemon
            .handle(create_envelope)
            .await
            .expect("legacy create response");
        let response::Result::Automation(created) = response_result(created) else {
            panic!("created automation")
        };
        assert_eq!(created.state, v1::AutomationState::Disabled as i32);

        let update = v1::UpdateAutomationRequest {
            automation_id: created.id.clone(),
            expected_revision: created.revision,
            title: "Brief updated".into(),
            prompt: "Summarize safely".into(),
            schedule: created.schedule.clone(),
            timezone: created.timezone.clone(),
            missed_run_policy: created.missed_run_policy,
            overlap_policy: created.overlap_policy,
            schedule_active: false,
        };
        // Request field 29, with removed UpdateAutomationRequest field 9.
        let update = legacy_automation_request([0xea, 0x01], update.encode_to_vec(), 0x48);
        let mut update_envelope = request(request::Operation::Health(v1::HealthRequest {}));
        update_envelope.request_id = "request-legacy-update".into();
        update_envelope.idempotency_key = "legacy-enable-update".into();
        update_envelope.payload = Some(envelope::Payload::Request(update));
        let updated = daemon
            .handle(update_envelope)
            .await
            .expect("legacy update response");
        let response::Result::Automation(updated) = response_result(updated) else {
            panic!("updated automation")
        };
        assert_eq!(updated.state, v1::AutomationState::Disabled as i32);
        assert_eq!(updated.revision, created.revision + 1);
    }

    #[tokio::test]
    async fn desktop_close_behavior_is_revisioned_and_idempotent() {
        let (daemon, clock) = daemon();
        let initial = daemon
            .handle(request(request::Operation::GetDesktopPreferences(
                v1::GetDesktopPreferencesRequest {},
            )))
            .await
            .expect("initial preferences");
        assert!(desktop_preferences(initial).keep_running_in_notification_area);

        let operation =
            request::Operation::UpdateDesktopPreferences(v1::UpdateDesktopPreferencesRequest {
                update_channel: "stable".into(),
                expected_revision: 0,
                keep_running_in_notification_area: false,
            });
        let updated = daemon
            .handle(request(operation.clone()))
            .await
            .expect("updated preferences");
        let updated = desktop_preferences(updated);
        assert!(!updated.keep_running_in_notification_area);
        assert_eq!(updated.revision, 1);

        clock.set(20);
        let replay = daemon
            .handle(request(operation))
            .await
            .expect("replayed preferences");
        assert_eq!(desktop_preferences(replay), updated);

        let conflict = daemon
            .handle(request(request::Operation::UpdateDesktopPreferences(
                v1::UpdateDesktopPreferencesRequest {
                    update_channel: "stable".into(),
                    expected_revision: 0,
                    keep_running_in_notification_area: true,
                },
            )))
            .await
            .expect("conflict response");
        let envelope::Payload::Response(response) = conflict.payload.expect("payload") else {
            panic!("response")
        };
        assert!(matches!(response.result, Some(response::Result::Error(_))));
    }

    #[tokio::test]
    async fn host_enrollment_is_canonical_revisioned_and_does_not_imply_readiness() {
        let (daemon, clock) = daemon();
        let directory = tempfile::tempdir().expect("Host Tools root");
        let enrolled = daemon
            .handle(request_with_key(
                request::Operation::EnrollHostExecution(v1::EnrollHostExecutionRequest {
                    expected_revision: 0,
                    acknowledgment_version: HOST_ACKNOWLEDGMENT_VERSION,
                    typed_acknowledgment: grok_domain::HOST_ACKNOWLEDGMENT_PHRASE.into(),
                    filesystem_read: true,
                    filesystem_write: true,
                    process_execute: true,
                    path_roots: vec![directory.path().to_string_lossy().into_owned()],
                    broad_scope_acknowledged: false,
                }),
                "enroll-host-tools",
            ))
            .await
            .expect("enroll response");
        let response::Result::HostExecutionPolicy(enrolled) = response_result(enrolled) else {
            panic!("Host policy")
        };
        assert!(enrolled.active);
        assert_eq!(enrolled.revision, 1);
        assert!(!enrolled.runtime_prepared);
        assert_eq!(
            enrolled.unavailable_reason_code,
            "host_tools_runtime_not_prepared"
        );
        clock.set(11);
        let replayed = daemon
            .handle(request_with_key(
                request::Operation::EnrollHostExecution(v1::EnrollHostExecutionRequest {
                    expected_revision: 0,
                    acknowledgment_version: HOST_ACKNOWLEDGMENT_VERSION,
                    typed_acknowledgment: grok_domain::HOST_ACKNOWLEDGMENT_PHRASE.into(),
                    filesystem_read: true,
                    filesystem_write: true,
                    process_execute: true,
                    path_roots: vec![directory.path().to_string_lossy().into_owned()],
                    broad_scope_acknowledged: false,
                }),
                "enroll-host-tools",
            ))
            .await
            .expect("exact enrollment replay");
        let response::Result::HostExecutionPolicy(replayed) = response_result(replayed) else {
            panic!("replayed Host policy")
        };
        assert_eq!(replayed, enrolled);

        let capabilities = daemon
            .handle(request(request::Operation::ResolveCapabilities(
                v1::ResolveCapabilitiesRequest::default(),
            )))
            .await
            .expect("capabilities");
        let response::Result::Capabilities(capabilities) = response_result(capabilities) else {
            panic!("capabilities")
        };
        assert_eq!(
            capabilities.work_execution_backend,
            v1::WorkExecutionBackend::Unspecified as i32
        );

        clock.set(20);
        let revoked = daemon
            .handle(request_with_key(
                request::Operation::RevokeHostExecution(v1::RevokeHostExecutionRequest {
                    expected_revision: 1,
                }),
                "revoke-host-tools",
            ))
            .await
            .expect("revoke response");
        let response::Result::HostExecutionPolicy(revoked) = response_result(revoked) else {
            panic!("revoked policy")
        };
        assert!(!revoked.active);
        assert_eq!(revoked.revision, 2);
    }

    #[tokio::test]
    async fn capability_projection_reports_non_terminal_host_bound_runs_authoritatively() {
        let (daemon, _) = daemon();
        let workspace = daemon.workspace().expect("workspace");
        let project = workspace
            .create_project(
                CreateProject {
                    name: "Host warning".into(),
                    description: String::new(),
                },
                "host-warning-project",
            )
            .await
            .expect("project");
        let thread = workspace
            .create_thread(
                CreateThread {
                    project_id: project.id.to_string(),
                    title: "Host warning".into(),
                },
                "host-warning-thread",
            )
            .await
            .expect("thread");
        let run = daemon
            .runs
            .create_work(
                CreateRun {
                    project_id: project.id.to_string(),
                    thread_id: thread.id.to_string(),
                },
                grok_domain::WorkExecutionBackend::HostDirect,
                "host-warning-run",
            )
            .await
            .expect("run");

        let active = daemon
            .handle(request(request::Operation::ResolveCapabilities(
                v1::ResolveCapabilitiesRequest::default(),
            )))
            .await
            .expect("active capabilities");
        let response::Result::Capabilities(active) = response_result(active) else {
            panic!("capabilities")
        };
        assert!(active.host_bound_run_active);

        daemon
            .runs
            .transition(
                &run.id,
                run.revision,
                grok_domain::RunState::Cancelled,
                "cancel-host-warning",
            )
            .await
            .expect("cancel run");
        let terminal = daemon
            .handle(request(request::Operation::ResolveCapabilities(
                v1::ResolveCapabilitiesRequest::default(),
            )))
            .await
            .expect("terminal capabilities");
        let response::Result::Capabilities(terminal) = response_result(terminal) else {
            panic!("capabilities")
        };
        assert!(!terminal.host_bound_run_active);
    }

    #[tokio::test]
    async fn live_selected_model_controls_chat_capability_and_selection_replay() {
        let (daemon, discovery_calls) = daemon_with_model_catalog();
        let initial_capabilities = daemon
            .handle(request(request::Operation::ResolveCapabilities(
                v1::ResolveCapabilitiesRequest::default(),
            )))
            .await
            .expect("initial capabilities");
        assert_eq!(
            capability_availability(&initial_capabilities, v1::Capability::Chat),
            v1::CapabilityAvailability::Limited as i32
        );
        assert_eq!(discovery_calls.load(Ordering::SeqCst), 1);

        let selection = request::Operation::SelectChatModel(v1::SelectChatModelRequest {
            expected_revision: 0,
            model_id: "grok-current".into(),
        });
        let selected = daemon
            .handle(request(selection.clone()))
            .await
            .expect("selected model");
        let envelope::Payload::Response(response) = selected.payload.expect("payload") else {
            panic!("response")
        };
        let Some(response::Result::ChatModelPreference(selected)) = response.result else {
            panic!("model preference")
        };
        assert_eq!(selected.selected_model_id, "grok-alternative");
        assert_eq!(selected.revision, 1);
        assert_eq!(discovery_calls.load(Ordering::SeqCst), 2);

        let replay = daemon
            .handle(request(selection))
            .await
            .expect("selection replay");
        let envelope::Payload::Response(response) = replay.payload.expect("payload") else {
            panic!("response")
        };
        let Some(response::Result::ChatModelPreference(replayed)) = response.result else {
            panic!("model preference")
        };
        assert_eq!(replayed, selected);
        assert_eq!(discovery_calls.load(Ordering::SeqCst), 2);

        let catalog = daemon
            .handle(request(request::Operation::GetChatModelCatalog(
                v1::GetChatModelCatalogRequest {},
            )))
            .await
            .expect("model catalog");
        let envelope::Payload::Response(response) = catalog.payload.expect("payload") else {
            panic!("response")
        };
        let Some(response::Result::ChatModelCatalog(catalog)) = response.result else {
            panic!("model catalog")
        };
        assert!(catalog.selected_model_ready);
        assert!(!catalog.default_model_ready);
        assert_eq!(
            catalog.preference.expect("preference").selected_model_id,
            "grok-alternative"
        );

        let capabilities = daemon
            .handle(request(request::Operation::ResolveCapabilities(
                v1::ResolveCapabilitiesRequest::default(),
            )))
            .await
            .expect("updated capabilities");
        assert_eq!(
            capability_availability(&capabilities, v1::Capability::Chat),
            v1::CapabilityAvailability::Available as i32
        );
    }

    #[tokio::test]
    #[allow(deprecated)]
    async fn caller_capability_facts_are_ignored() {
        let (daemon, _) = daemon();
        let response = daemon
            .handle(request(request::Operation::ResolveCapabilities(
                v1::ResolveCapabilitiesRequest {
                    facts: Some(v1::CapabilityFacts {
                        subscription_authenticated: true,
                        xai_api_key_configured: true,
                        online: true,
                        strong_isolation_ready: true,
                        managed_browser_ready: true,
                        computer_use_ready: true,
                    }),
                },
            )))
            .await
            .expect("capabilities");
        let envelope::Payload::Response(response) = response.payload.expect("payload") else {
            panic!("response")
        };
        let Some(response::Result::Capabilities(capabilities)) = response.result else {
            panic!("capabilities")
        };
        for capability in [v1::Capability::Work, v1::Capability::Research] {
            let status = capabilities
                .statuses
                .iter()
                .find(|status| status.capability == capability as i32)
                .expect("capability status");
            assert_eq!(
                status.availability,
                v1::CapabilityAvailability::Unavailable as i32
            );
        }
    }

    #[tokio::test]
    async fn native_xai_key_enrollment_is_idempotent_and_never_returns_the_secret() {
        let (daemon, _) = daemon();
        let daemon = daemon.with_runtime_capability_facts(CapabilityFacts {
            online: true,
            ..CapabilityFacts::default()
        });
        let configured = daemon
            .handle(request(request::Operation::EnrollXaiApiKey(
                v1::EnrollXaiApiKeyRequest {
                    parent_window_token: 42,
                },
            )))
            .await
            .expect("configure");
        assert!(account_state(configured.clone()).xai_api_key_configured);
        assert!(
            !configured
                .encode_to_vec()
                .windows(b"xai-user-owned-key".len())
                .any(|window| window == b"xai-user-owned-key")
        );

        let capabilities = daemon
            .handle(request(request::Operation::ResolveCapabilities(
                v1::ResolveCapabilitiesRequest::default(),
            )))
            .await
            .expect("capabilities");
        assert_eq!(
            capability_availability(&capabilities, v1::Capability::Chat),
            v1::CapabilityAvailability::Available as i32
        );
        assert_eq!(
            capability_availability(&capabilities, v1::Capability::Research),
            v1::CapabilityAvailability::Unavailable as i32
        );
        assert_eq!(
            capability_availability(&capabilities, v1::Capability::Work),
            v1::CapabilityAvailability::Unavailable as i32
        );

        let replay = daemon
            .handle(request(request::Operation::EnrollXaiApiKey(
                v1::EnrollXaiApiKeyRequest {
                    parent_window_token: 42,
                },
            )))
            .await
            .expect("replay response");
        assert!(account_state(replay).xai_api_key_configured);

        let state = daemon
            .handle(request(request::Operation::GetAccountState(
                v1::GetAccountStateRequest {},
            )))
            .await
            .expect("state");
        assert!(account_state(state).xai_api_key_configured);
        let deleted = daemon
            .handle(request(request::Operation::DeleteXaiApiKey(
                v1::DeleteXaiApiKeyRequest {},
            )))
            .await
            .expect("delete");
        assert!(!account_state(deleted).xai_api_key_configured);
    }

    #[tokio::test]
    async fn missing_enrollment_idempotency_key_does_not_consume_native_entry() {
        let (daemon, _) = daemon();
        let mut missing = request(request::Operation::EnrollXaiApiKey(
            v1::EnrollXaiApiKeyRequest {
                parent_window_token: 42,
            },
        ));
        missing.idempotency_key.clear();

        let response = daemon.handle(missing).await.expect("missing key response");
        let envelope::Payload::Response(response) = response.payload.expect("payload") else {
            panic!("response")
        };
        assert!(matches!(
            response.result,
            Some(response::Result::Error(v1::ErrorResponse { code, .. }))
                if code == v1::ErrorCode::InvalidArgument as i32
        ));

        let configured = daemon
            .handle(request(request::Operation::EnrollXaiApiKey(
                v1::EnrollXaiApiKeyRequest {
                    parent_window_token: 42,
                },
            )))
            .await
            .expect("valid enrollment still has native input");
        assert!(account_state(configured).xai_api_key_configured);
    }

    #[tokio::test]
    async fn credential_deadline_requires_a_fresh_generation_before_another_prompt() {
        let vault = Arc::new(InMemorySecretVault::new());
        let store = Arc::new(InMemoryExecutionStore::new());
        let cancelled = Arc::new(AtomicBool::new(false));
        let (daemon, _) = daemon_with_credentials_and_store(
            vault.clone(),
            Arc::new(PendingXaiKey {
                cancelled: cancelled.clone(),
            }),
            store.clone(),
        );
        let mut configure = request(request::Operation::EnrollXaiApiKey(
            v1::EnrollXaiApiKeyRequest {
                parent_window_token: 42,
            },
        ));
        configure.deadline_unix_ms = 35;

        let response = daemon.handle(configure).await.expect("deadline response");
        let envelope::Payload::Response(response) = response.payload.expect("payload") else {
            panic!("response")
        };
        assert!(matches!(
            response.result,
            Some(response::Result::Error(v1::ErrorResponse { code, retryable: true, .. }))
                if code == v1::ErrorCode::DeadlineExceeded as i32
        ));
        assert!(cancelled.load(Ordering::SeqCst));
        assert!(matches!(
            vault.get(&SecretName::new("xai.api-key.primary").expect("secret name")),
            Err(grok_application::VaultError::NotFound)
        ));

        let state = daemon
            .handle(request(request::Operation::GetAccountState(
                v1::GetAccountStateRequest {},
            )))
            .await
            .expect("follow-up request");
        assert!(!account_state(state).xai_api_key_configured);

        let credential_store: Arc<dyn CredentialMutationStore> = store;
        let recovery_credentials = Arc::new(CredentialService::new(
            vault,
            credential_store.clone(),
            Arc::new(AcceptXaiKey),
        ));
        let recovery = CredentialEnrollmentService::new(
            recovery_credentials,
            credential_store,
            Arc::new(SequenceCredentialEnrollment::keys([
                b"xai-retry-key".as_slice()
            ])),
        );
        assert!(matches!(
            recovery
                .enroll_xai_api_key(
                    CredentialEnrollmentRequest {
                        parent_window_token: 42,
                    },
                    "command-1",
                )
                .await,
            Err(ApplicationError::Integrity(_))
        ));
        assert!(
            recovery
                .enroll_xai_api_key(
                    CredentialEnrollmentRequest {
                        parent_window_token: 42,
                    },
                    "command-2",
                )
                .await
                .expect("fresh enrollment generation")
                .xai_api_key_configured
        );
    }

    #[tokio::test]
    async fn cancelled_native_enrollment_has_a_stable_non_retryable_result() {
        let (daemon, _) = daemon_with_credentials_store_and_enrollment(
            Arc::new(InMemorySecretVault::new()),
            Arc::new(AcceptXaiKey),
            Arc::new(InMemoryExecutionStore::new()),
            Arc::new(SequenceCredentialEnrollment::error(
                CredentialEnrollmentError::Cancelled,
            )),
        );
        let response = daemon
            .handle(request(request::Operation::EnrollXaiApiKey(
                v1::EnrollXaiApiKeyRequest {
                    parent_window_token: 42,
                },
            )))
            .await
            .expect("cancelled enrollment response");
        let envelope::Payload::Response(response) = response.payload.expect("payload") else {
            panic!("response")
        };
        assert!(matches!(
            response.result,
            Some(response::Result::Error(v1::ErrorResponse {
                code,
                retryable: false,
                ..
            })) if code == v1::ErrorCode::Cancelled as i32
        ));
    }

    #[tokio::test]
    async fn native_enrollment_integrity_failure_is_not_retryable() {
        let (daemon, _) = daemon_with_credentials_store_and_enrollment(
            Arc::new(InMemorySecretVault::new()),
            Arc::new(AcceptXaiKey),
            Arc::new(InMemoryExecutionStore::new()),
            Arc::new(SequenceCredentialEnrollment::error(
                CredentialEnrollmentError::Integrity,
            )),
        );
        let response = daemon
            .handle(request(request::Operation::EnrollXaiApiKey(
                v1::EnrollXaiApiKeyRequest {
                    parent_window_token: 42,
                },
            )))
            .await
            .expect("integrity response");
        let envelope::Payload::Response(response) = response.payload.expect("payload") else {
            panic!("response")
        };
        assert!(matches!(
            response.result,
            Some(response::Result::Error(v1::ErrorResponse {
                code,
                retryable: false,
                ..
            })) if code == v1::ErrorCode::IntegrityFailure as i32
        ));
    }

    #[tokio::test]
    async fn stale_renderer_nonce_is_rejected_before_dispatch() {
        let (daemon, _) = daemon();
        let mut message = request(request::Operation::Health(v1::HealthRequest {}));
        message.startup_nonce = vec![8; 32];
        assert!(matches!(
            daemon.handle(message).await,
            Err(HandlerError::Envelope(EnvelopeError::InvalidStartupNonce))
        ));
    }

    #[tokio::test]
    async fn project_mutation_replays_and_conflicting_key_reuse_is_rejected() {
        let (daemon, _) = daemon();
        let operation = request::Operation::CreateProject(v1::CreateProjectRequest {
            name: "Research".into(),
            description: "Launch program".into(),
        });
        let first = daemon
            .handle(request(operation.clone()))
            .await
            .expect("first create");
        let replay = daemon
            .handle(request(operation))
            .await
            .expect("replayed create");
        assert_eq!(project_id(first), project_id(replay));

        let conflict = daemon
            .handle(request(request::Operation::CreateProject(
                v1::CreateProjectRequest {
                    name: "Different".into(),
                    description: String::new(),
                },
            )))
            .await
            .expect("conflict response");
        let envelope::Payload::Response(response) = conflict.payload.expect("payload") else {
            panic!("response")
        };
        assert!(matches!(
            response.result,
            Some(response::Result::Error(v1::ErrorResponse { code, .. }))
                if code == v1::ErrorCode::Conflict as i32
        ));
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn artifact_import_open_and_removal_are_path_free_and_never_repeat_side_effects() {
        let content = Arc::new(SuccessfulArtifactContent::default());
        let opener = Arc::new(SuccessfulArtifactOpener::default());
        let (mut daemon, _, _) = daemon_with_artifact_adapters(content.clone(), opener.clone());
        let project = daemon
            .workspace()
            .expect("workspace")
            .create_project(
                CreateProject {
                    name: "Artifact operations".into(),
                    description: String::new(),
                },
                "artifact-project",
            )
            .await
            .expect("project");
        let source_path = test_artifact_source_path("customer-secret.txt");
        let import = request::Operation::ImportArtifact(v1::ImportArtifactRequest {
            project_id: project.id.to_string(),
            thread_id: None,
            display_name: "customer-secret.txt".into(),
            media_type: "text/plain".into(),
            source_path: source_path.clone(),
        });
        let first = daemon
            .handle(artifact_request_with_key(import.clone(), "artifact-import"))
            .await
            .expect("import");
        daemon.artifact_content_available = false;
        let mut replay_import = import;
        if let request::Operation::ImportArtifact(request) = &mut replay_import {
            request.source_path = test_artifact_source_path("different-selection.txt");
        }
        let replay = daemon
            .handle(artifact_request_with_key(replay_import, "artifact-import"))
            .await
            .expect("import replay");
        assert_eq!(content.prepare_calls.load(Ordering::SeqCst), 1);
        assert_eq!(content.publish_calls.load(Ordering::SeqCst), 1);
        assert!(
            !first
                .encode_to_vec()
                .windows(source_path.len())
                .any(|window| { window == source_path.as_bytes() })
        );

        let imported = match response_result(first) {
            response::Result::ArtifactOperation(result) => match result.result {
                Some(v1::artifact_operation_result::Result::ImportedArtifact(artifact)) => artifact,
                _ => panic!("import result"),
            },
            _ => panic!("artifact operation"),
        };
        let replayed = match response_result(replay) {
            response::Result::ArtifactOperation(result) => match result.result {
                Some(v1::artifact_operation_result::Result::ImportedArtifact(artifact)) => artifact,
                _ => panic!("import replay result"),
            },
            _ => panic!("artifact replay operation"),
        };
        assert_eq!(imported, replayed);
        assert_eq!(imported.state, v1::ArtifactState::Available as i32);
        assert_eq!(imported.content_version, Some(1));

        daemon.artifact_content_available = true;
        let open = request::Operation::OpenArtifact(v1::OpenArtifactRequest {
            artifact_id: imported.id.clone(),
            content_version: 1,
        });
        let open_response = daemon
            .handle(artifact_request_with_key(open.clone(), "artifact-open"))
            .await
            .expect("open");
        daemon.artifact_open_available = false;
        let opened_replay = daemon
            .handle(artifact_request_with_key(open, "artifact-open"))
            .await
            .expect("open replay");
        assert_eq!(opener.calls.load(Ordering::SeqCst), 1);
        for envelope in [open_response, opened_replay] {
            let response::Result::ArtifactOperation(result) = response_result(envelope) else {
                panic!("open operation")
            };
            let Some(v1::artifact_operation_result::Result::OpenReceipt(receipt)) = result.result
            else {
                panic!("open receipt")
            };
            assert_eq!(receipt.artifact_id, imported.id);
            assert_eq!(receipt.content_version, 1);
            assert_eq!(receipt.status, v1::ArtifactOpenReceiptStatus::Opened as i32);
        }

        let removal = request::Operation::RemoveArtifact(v1::RemoveArtifactRequest {
            artifact_id: imported.id.clone(),
            expected_revision: imported.revision,
            expected_content_version: 1,
        });
        let removed = daemon
            .handle(artifact_request_with_key(
                removal.clone(),
                "artifact-removal",
            ))
            .await
            .expect("remove");
        daemon.artifact_content_available = false;
        let removed_replay = daemon
            .handle(artifact_request_with_key(removal, "artifact-removal"))
            .await
            .expect("removal replay");
        assert_eq!(content.purge_calls.load(Ordering::SeqCst), 1);
        for envelope in [removed, removed_replay] {
            let response::Result::ArtifactOperation(result) = response_result(envelope) else {
                panic!("removal operation")
            };
            let Some(v1::artifact_operation_result::Result::RemovedArtifact(artifact)) =
                result.result
            else {
                panic!("removed artifact")
            };
            assert_eq!(artifact.id, imported.id);
            assert_eq!(artifact.state, v1::ArtifactState::Deleted as i32);
            assert_eq!(artifact.content_version, None);
            assert_eq!(artifact.revision, imported.revision + 1);
        }

        // A historical terminal open receipt remains exactly replayable after
        // the current artifact is tombstoned and its bytes are purged.
        let historical_open = daemon
            .handle(artifact_request_with_key(
                request::Operation::OpenArtifact(v1::OpenArtifactRequest {
                    artifact_id: imported.id.clone(),
                    content_version: 1,
                }),
                "artifact-open",
            ))
            .await
            .expect("historical open replay");
        let response::Result::ArtifactOperation(result) = response_result(historical_open) else {
            panic!("historical open operation")
        };
        assert!(matches!(
            result.result,
            Some(v1::artifact_operation_result::Result::OpenReceipt(
                v1::ArtifactOpenReceipt { status, .. }
            )) if status == v1::ArtifactOpenReceiptStatus::Opened as i32
        ));
        assert_eq!(opener.calls.load(Ordering::SeqCst), 1);

        let capabilities = daemon
            .handle(request(request::Operation::ResolveCapabilities(
                v1::ResolveCapabilitiesRequest::default(),
            )))
            .await
            .expect("capabilities");
        assert_eq!(
            capability_availability(&capabilities, v1::Capability::Files),
            v1::CapabilityAvailability::Available as i32
        );
    }

    #[tokio::test]
    async fn reserved_removal_returns_a_path_free_pending_receipt_and_daemon_recovers_it_live() {
        let content = Arc::new(SuccessfulArtifactContent::default());
        let opener = Arc::new(SuccessfulArtifactOpener::default());
        let (daemon, _, store) = daemon_with_artifact_adapters(Arc::clone(&content), opener);
        let project = daemon
            .workspace()
            .expect("workspace")
            .create_project(
                CreateProject {
                    name: "Pending artifact removal".into(),
                    description: String::new(),
                },
                "pending-removal-project",
            )
            .await
            .expect("project");
        let imported = daemon
            .handle(artifact_request_with_key(
                request::Operation::ImportArtifact(v1::ImportArtifactRequest {
                    project_id: project.id.to_string(),
                    thread_id: None,
                    display_name: "pending.txt".into(),
                    media_type: "text/plain".into(),
                    source_path: test_artifact_source_path("pending.txt"),
                }),
                "pending-removal-import",
            ))
            .await
            .expect("import");
        let response::Result::ArtifactOperation(imported) = response_result(imported) else {
            panic!("artifact import operation")
        };
        let Some(v1::artifact_operation_result::Result::ImportedArtifact(imported)) =
            imported.result
        else {
            panic!("imported artifact")
        };

        content.purge_failures.store(1, Ordering::SeqCst);
        let removal = request::Operation::RemoveArtifact(v1::RemoveArtifactRequest {
            artifact_id: imported.id.clone(),
            expected_revision: imported.revision,
            expected_content_version: imported.content_version.expect("content version"),
        });
        let pending = daemon
            .handle(artifact_request_with_key(
                removal.clone(),
                "pending-removal",
            ))
            .await
            .expect("pending removal response");
        let response::Result::ArtifactOperation(pending) = response_result(pending) else {
            panic!("pending artifact operation")
        };
        let tombstone = match pending.result {
            Some(v1::artifact_operation_result::Result::RemovalPending(receipt)) => {
                assert_eq!(receipt.artifact_id, imported.id);
                assert_eq!(receipt.expected_revision, imported.revision);
                assert_eq!(receipt.expected_content_version, 1);
                receipt.tombstone.expect("canonical tombstone")
            }
            Some(v1::artifact_operation_result::Result::RemovedArtifact(artifact)) => artifact,
            other => panic!("pending or recovered removal receipt: {other:?}"),
        };
        assert_eq!(tombstone.id, imported.id);
        assert_eq!(tombstone.state, v1::ArtifactState::Deleted as i32);
        assert_eq!(tombstone.revision, imported.revision + 1);
        assert_eq!(tombstone.content_version, None);

        tokio::time::timeout(Duration::from_secs(2), async {
            while !store
                .list_incomplete_removals(1)
                .await
                .expect("pending removals")
                .is_empty()
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("daemon-owned removal recovery completed");
        assert_eq!(content.purge_calls.load(Ordering::SeqCst), 2);

        let terminal = daemon
            .handle(artifact_request_with_key(removal, "pending-removal"))
            .await
            .expect("terminal removal replay");
        let response::Result::ArtifactOperation(terminal) = response_result(terminal) else {
            panic!("terminal artifact operation")
        };
        assert!(matches!(
            terminal.result,
            Some(v1::artifact_operation_result::Result::RemovedArtifact(artifact))
                if artifact == tombstone
        ));
        assert_eq!(content.purge_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn timed_out_failed_removal_is_recovered_and_direct_dispatch_stays_bounded() {
        let content = Arc::new(GatedArtifactContent::new());
        content.gate_prepare.store(false, Ordering::SeqCst);
        let opener = Arc::new(SuccessfulArtifactOpener::default());
        let (daemon, _, store) = daemon_with_artifact_adapters(Arc::clone(&content), opener);
        let project = daemon
            .workspace()
            .expect("workspace")
            .create_project(
                CreateProject {
                    name: "Bounded removal dispatch".into(),
                    description: String::new(),
                },
                "bounded-removal-project",
            )
            .await
            .expect("project");
        let first = import_test_artifact(
            &daemon,
            project.id.as_str(),
            "first-removal.txt",
            "bounded-removal-first-import",
        )
        .await;
        let second = import_test_artifact(
            &daemon,
            project.id.as_str(),
            "second-removal.txt",
            "bounded-removal-second-import",
        )
        .await;
        let first_removal = removal_operation(&first);
        let second_removal = removal_operation(&second);

        content.purge_failures.store(1, Ordering::SeqCst);
        let timed_out = daemon
            .handle_with_dispatch_limit(
                artifact_request_with_key(first_removal.clone(), "bounded-removal-first"),
                Some(Duration::from_millis(5)),
            )
            .await
            .expect("outer removal timeout response");
        assert_error_code(timed_out, v1::ErrorCode::DeadlineExceeded);
        tokio::time::timeout(Duration::from_secs(1), content.purge_started.notified())
            .await
            .expect("detached removal purge started");
        assert_eq!(content.purge_calls.load(Ordering::SeqCst), 1);
        assert_eq!(daemon.artifact_removal_direct_slots.available_permits(), 0);

        let pending = daemon
            .handle(artifact_request_with_key(
                first_removal.clone(),
                "bounded-removal-first",
            ))
            .await
            .expect("durable pending replay");
        let response::Result::ArtifactOperation(pending) = response_result(pending) else {
            panic!("pending removal operation")
        };
        assert!(matches!(
            pending.result,
            Some(v1::artifact_operation_result::Result::RemovalPending(_))
        ));

        for _ in 0..8 {
            let busy = daemon
                .handle_with_dispatch_limit(
                    artifact_request_with_key(second_removal.clone(), "bounded-removal-second"),
                    Some(Duration::from_millis(5)),
                )
                .await
                .expect("bounded removal dispatch response");
            assert_error_code(busy, v1::ErrorCode::Unavailable);
        }
        assert_eq!(content.purge_calls.load(Ordering::SeqCst), 1);
        assert_eq!(daemon.artifact_removal_direct_slots.available_permits(), 0);
        assert_eq!(
            store
                .list_incomplete_removals(10)
                .await
                .expect("pending removal count")
                .len(),
            1
        );

        content.release_purge.notify_one();
        wait_for_removal_recovery(&store).await;
        assert_eq!(content.purge_calls.load(Ordering::SeqCst), 2);
        assert_eq!(daemon.artifact_removal_direct_slots.available_permits(), 1);

        let terminal = daemon
            .handle(artifact_request_with_key(
                first_removal,
                "bounded-removal-first",
            ))
            .await
            .expect("terminal detached removal replay");
        let response::Result::ArtifactOperation(terminal) = response_result(terminal) else {
            panic!("terminal detached removal operation")
        };
        assert!(matches!(
            terminal.result,
            Some(v1::artifact_operation_result::Result::RemovedArtifact(_))
        ));
        assert_eq!(content.purge_calls.load(Ordering::SeqCst), 2);

        let second_terminal = daemon
            .handle(artifact_request_with_key(
                second_removal,
                "bounded-removal-second",
            ))
            .await
            .expect("second removal after direct slot release");
        let response::Result::ArtifactOperation(second_terminal) = response_result(second_terminal)
        else {
            panic!("second terminal removal operation")
        };
        assert!(matches!(
            second_terminal.result,
            Some(v1::artifact_operation_result::Result::RemovedArtifact(_))
        ));
        assert_eq!(content.purge_calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn running_scanner_wakes_for_a_second_pending_command_after_the_first_commits() {
        let content = Arc::new(SuccessfulArtifactContent::default());
        let opener = Arc::new(SuccessfulArtifactOpener::default());
        let (daemon, _, store) = daemon_with_artifact_adapters(Arc::clone(&content), opener);
        let artifacts = Arc::clone(daemon.artifacts.as_ref().expect("artifacts"));
        let project = daemon
            .workspace()
            .expect("workspace")
            .create_project(
                CreateProject {
                    name: "Scanner handoff".into(),
                    description: String::new(),
                },
                "scanner-handoff-project",
            )
            .await
            .expect("project");
        let first = import_test_artifact(
            &daemon,
            project.id.as_str(),
            "scanner-first.txt",
            "scanner-first-import",
        )
        .await;
        let second = import_test_artifact(
            &daemon,
            project.id.as_str(),
            "scanner-second.txt",
            "scanner-second-import",
        )
        .await;

        for (artifact, key, expected_purge_calls) in [
            (&first, "scanner-first-removal", 2),
            (&second, "scanner-second-removal", 4),
        ] {
            let operation = removal_operation(artifact);
            let request::Operation::RemoveArtifact(wire) = operation.clone() else {
                unreachable!("removal operation")
            };
            let input = remove_artifact_from_wire(wire).expect("valid removal request");
            content.purge_failures.store(1, Ordering::SeqCst);
            artifacts
                .remove_artifact(input, key)
                .await
                .expect_err("seed durable pending removal");

            let pending = daemon
                .handle(artifact_request_with_key(operation.clone(), key))
                .await
                .expect("pending removal replay");
            let response::Result::ArtifactOperation(pending) = response_result(pending) else {
                panic!("pending scanner removal operation")
            };
            assert!(matches!(
                pending.result,
                Some(v1::artifact_operation_result::Result::RemovalPending(_))
            ));

            wait_for_removal_recovery(&store).await;
            assert_eq!(
                content.purge_calls.load(Ordering::SeqCst),
                expected_purge_calls
            );
            assert!(
                daemon
                    .artifact_removal_recovery
                    .running
                    .load(Ordering::Acquire)
            );

            let terminal = daemon
                .handle(artifact_request_with_key(operation, key))
                .await
                .expect("terminal scanner removal replay");
            let response::Result::ArtifactOperation(terminal) = response_result(terminal) else {
                panic!("terminal scanner removal operation")
            };
            assert!(matches!(
                terminal.result,
                Some(v1::artifact_operation_result::Result::RemovedArtifact(_))
            ));
            assert_eq!(
                content.purge_calls.load(Ordering::SeqCst),
                expected_purge_calls
            );
        }
    }

    #[tokio::test]
    async fn removal_scanner_stops_with_daemon_while_artifact_service_is_retained() {
        let content = Arc::new(SuccessfulArtifactContent::default());
        let opener = Arc::new(SuccessfulArtifactOpener::default());
        let (daemon, _, _) = daemon_with_artifact_adapters(content, opener);
        let artifacts = Arc::clone(daemon.artifacts.as_ref().expect("artifacts"));
        let recovery = Arc::clone(&daemon.artifact_removal_recovery);
        daemon.trigger_artifact_removal_recovery(&artifacts);
        assert!(recovery.running.load(Ordering::Acquire));

        drop(daemon);
        tokio::time::timeout(Duration::from_secs(1), async {
            while recovery.running.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("removal scanner stopped with daemon lifetime");

        assert!(Arc::strong_count(&artifacts) > 0);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn short_artifact_budgets_fail_before_reservation_or_dispatch() {
        let content = Arc::new(SuccessfulArtifactContent::default());
        let opener = Arc::new(SuccessfulArtifactOpener::default());
        let (daemon, _, store) = daemon_with_artifact_adapters(content.clone(), opener.clone());
        let project = daemon
            .workspace()
            .expect("workspace")
            .create_project(
                CreateProject {
                    name: "Artifact budget".into(),
                    description: String::new(),
                },
                "artifact-budget-project",
            )
            .await
            .expect("project");
        let import = request::Operation::ImportArtifact(v1::ImportArtifactRequest {
            project_id: project.id.to_string(),
            thread_id: None,
            display_name: "budget.txt".into(),
            media_type: "text/plain".into(),
            source_path: test_artifact_source_path("budget.txt"),
        });
        let import_minimum =
            artifact_operation_minimum_budget(Some(&import)).expect("import minimum budget");
        let rejected = daemon
            .handle(artifact_request_with_budget(
                import.clone(),
                "artifact-budget-import",
                import_minimum
                    .checked_sub(Duration::from_millis(1))
                    .expect("positive import minimum"),
            ))
            .await
            .expect("short import response");
        assert_error_code(rejected, v1::ErrorCode::DeadlineExceeded);
        assert_eq!(content.prepare_calls.load(Ordering::SeqCst), 0);
        assert!(
            store
                .list_artifacts(&project.id, None, 10)
                .await
                .expect("artifacts after rejected import")
                .is_empty()
        );

        let imported = daemon
            .handle(artifact_request_with_key(import, "artifact-budget-import"))
            .await
            .expect("import with sufficient budget");
        let response::Result::ArtifactOperation(imported) = response_result(imported) else {
            panic!("artifact import response")
        };
        let Some(v1::artifact_operation_result::Result::ImportedArtifact(imported)) =
            imported.result
        else {
            panic!("imported artifact")
        };
        assert_eq!(content.prepare_calls.load(Ordering::SeqCst), 1);

        let open = request::Operation::OpenArtifact(v1::OpenArtifactRequest {
            artifact_id: imported.id.clone(),
            content_version: imported.content_version.expect("content version"),
        });
        let open_minimum =
            artifact_operation_minimum_budget(Some(&open)).expect("open minimum budget");
        let rejected = daemon
            .handle(artifact_request_with_budget(
                open,
                "artifact-budget-open",
                open_minimum
                    .checked_sub(Duration::from_millis(1))
                    .expect("positive open minimum"),
            ))
            .await
            .expect("short open response");
        assert_error_code(rejected, v1::ErrorCode::DeadlineExceeded);
        assert_eq!(opener.calls.load(Ordering::SeqCst), 0);
        assert!(
            store
                .list_incomplete_opens(10)
                .await
                .expect("opens after rejected request")
                .is_empty()
        );

        let removal = request::Operation::RemoveArtifact(v1::RemoveArtifactRequest {
            artifact_id: imported.id,
            expected_revision: imported.revision,
            expected_content_version: imported.content_version.expect("content version"),
        });
        let removal_minimum =
            artifact_operation_minimum_budget(Some(&removal)).expect("removal minimum budget");
        let rejected = daemon
            .handle(artifact_request_with_budget(
                removal,
                "artifact-budget-removal",
                removal_minimum
                    .checked_sub(Duration::from_millis(1))
                    .expect("positive removal minimum"),
            ))
            .await
            .expect("short removal response");
        assert_error_code(rejected, v1::ErrorCode::DeadlineExceeded);
        assert_eq!(content.purge_calls.load(Ordering::SeqCst), 0);
        assert!(
            store
                .list_incomplete_removals(10)
                .await
                .expect("removals after rejected request")
                .is_empty()
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn artifact_tasks_finish_after_outer_timeout_and_replay_without_duplicate_io() {
        let content = Arc::new(GatedArtifactContent::new());
        let opener = Arc::new(GatedArtifactOpener::new());
        let (daemon, _, _) = daemon_with_artifact_adapters(content.clone(), opener.clone());
        let project = daemon
            .workspace()
            .expect("workspace")
            .create_project(
                CreateProject {
                    name: "Detached artifacts".into(),
                    description: String::new(),
                },
                "detached-artifact-project",
            )
            .await
            .expect("project");
        let import = request::Operation::ImportArtifact(v1::ImportArtifactRequest {
            project_id: project.id.to_string(),
            thread_id: None,
            display_name: "detached.txt".into(),
            media_type: "text/plain".into(),
            source_path: test_artifact_source_path("detached.txt"),
        });
        let timed_out = daemon
            .handle_with_dispatch_limit(
                artifact_request_with_key(import.clone(), "detached-import"),
                Some(Duration::from_millis(5)),
            )
            .await
            .expect("outer import timeout response");
        assert_error_code(timed_out, v1::ErrorCode::DeadlineExceeded);
        tokio::time::timeout(Duration::from_secs(1), content.prepare_started.notified())
            .await
            .expect("detached import started");
        assert_eq!(content.prepare_calls.load(Ordering::SeqCst), 1);
        content.release_prepare.notify_one();

        let imported =
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let replay = daemon
                        .handle(artifact_request_with_key(import.clone(), "detached-import"))
                        .await
                        .expect("import replay response");
                    match response_result(replay) {
                        response::Result::ArtifactOperation(result) => {
                            let Some(v1::artifact_operation_result::Result::ImportedArtifact(
                                artifact,
                            )) = result.result
                            else {
                                panic!("import replay variant")
                            };
                            break artifact;
                        }
                        response::Result::Error(error)
                            if error.code == v1::ErrorCode::Unavailable as i32 =>
                        {
                            tokio::task::yield_now().await;
                        }
                        other => panic!("unexpected import replay result: {other:?}"),
                    }
                }
            })
            .await
            .expect("detached import reached terminal state");
        assert_eq!(content.prepare_calls.load(Ordering::SeqCst), 1);
        assert_eq!(content.publish_calls.load(Ordering::SeqCst), 1);

        let open = request::Operation::OpenArtifact(v1::OpenArtifactRequest {
            artifact_id: imported.id.clone(),
            content_version: imported.content_version.expect("content version"),
        });
        let timed_out = daemon
            .handle_with_dispatch_limit(
                artifact_request_with_key(open.clone(), "detached-open"),
                Some(Duration::from_millis(5)),
            )
            .await
            .expect("outer open timeout response");
        assert_error_code(timed_out, v1::ErrorCode::DeadlineExceeded);
        tokio::time::timeout(Duration::from_secs(1), opener.started.notified())
            .await
            .expect("detached open started");
        assert_eq!(opener.calls.load(Ordering::SeqCst), 1);
        opener.release.notify_one();

        let open_receipt = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let replay = daemon
                    .handle(artifact_request_with_key(open.clone(), "detached-open"))
                    .await
                    .expect("open replay response");
                match response_result(replay) {
                    response::Result::ArtifactOperation(result) => {
                        let Some(v1::artifact_operation_result::Result::OpenReceipt(receipt)) =
                            result.result
                        else {
                            panic!("open replay variant")
                        };
                        break receipt;
                    }
                    response::Result::Error(error)
                        if error.code == v1::ErrorCode::Unavailable as i32 =>
                    {
                        tokio::task::yield_now().await;
                    }
                    other => panic!("unexpected open replay result: {other:?}"),
                }
            }
        })
        .await
        .expect("detached open reached terminal state");
        assert_eq!(open_receipt.artifact_id, imported.id);
        assert_eq!(
            open_receipt.status,
            v1::ArtifactOpenReceiptStatus::Opened as i32
        );
        assert_eq!(opener.calls.load(Ordering::SeqCst), 1);

        let replay = daemon
            .handle(artifact_request_with_key(open, "detached-open"))
            .await
            .expect("terminal open replay");
        let response::Result::ArtifactOperation(result) = response_result(replay) else {
            panic!("terminal open replay result")
        };
        assert!(matches!(
            result.result,
            Some(v1::artifact_operation_result::Result::OpenReceipt(receipt))
                if receipt == open_receipt
        ));
        assert_eq!(opener.calls.load(Ordering::SeqCst), 1);

        let removal = request::Operation::RemoveArtifact(v1::RemoveArtifactRequest {
            artifact_id: imported.id.clone(),
            expected_revision: imported.revision,
            expected_content_version: imported.content_version.expect("content version"),
        });
        let timed_out = daemon
            .handle_with_dispatch_limit(
                artifact_request_with_key(removal.clone(), "detached-removal"),
                Some(Duration::from_millis(5)),
            )
            .await
            .expect("outer removal timeout response");
        assert_error_code(timed_out, v1::ErrorCode::DeadlineExceeded);
        tokio::time::timeout(Duration::from_secs(1), content.purge_started.notified())
            .await
            .expect("detached purge started");
        assert_eq!(content.purge_calls.load(Ordering::SeqCst), 1);
        content.release_purge.notify_one();

        let removed = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let replay = daemon
                    .handle(artifact_request_with_key(
                        removal.clone(),
                        "detached-removal",
                    ))
                    .await
                    .expect("removal replay response");
                match response_result(replay) {
                    response::Result::ArtifactOperation(result) => match result.result {
                        Some(v1::artifact_operation_result::Result::RemovedArtifact(artifact)) => {
                            break artifact;
                        }
                        Some(v1::artifact_operation_result::Result::RemovalPending(_)) => {
                            tokio::task::yield_now().await;
                        }
                        other => panic!("removal replay variant: {other:?}"),
                    },
                    response::Result::Error(error)
                        if error.code == v1::ErrorCode::Unavailable as i32 =>
                    {
                        tokio::task::yield_now().await;
                    }
                    other => panic!("unexpected removal replay result: {other:?}"),
                }
            }
        })
        .await
        .expect("detached removal reached terminal state");
        assert_eq!(removed.id, imported.id);
        assert_eq!(removed.state, v1::ArtifactState::Deleted as i32);
        assert_eq!(removed.content_version, None);
        assert_eq!(content.purge_calls.load(Ordering::SeqCst), 1);

        let replay = daemon
            .handle(artifact_request_with_key(removal, "detached-removal"))
            .await
            .expect("terminal removal replay");
        let response::Result::ArtifactOperation(result) = response_result(replay) else {
            panic!("terminal removal replay result")
        };
        assert!(matches!(
            result.result,
            Some(v1::artifact_operation_result::Result::RemovedArtifact(artifact))
                if artifact == removed
        ));
        assert_eq!(content.purge_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn exact_approval_decisions_round_trip_with_revision_and_idempotency_guards() {
        let (daemon, _) = daemon();
        let pending = seed_approval(&daemon, "grant").await;
        let grant = request::Operation::DecideApproval(v1::DecideApprovalRequest {
            approval_id: pending.id.to_string(),
            expected_revision: 0,
            decision: v1::ApprovalDecision::Grant as i32,
        });

        let granted = daemon
            .handle(request_with_key(grant.clone(), "decide-grant"))
            .await
            .expect("grant response");
        let replayed = daemon
            .handle(request_with_key(grant, "decide-grant"))
            .await
            .expect("grant replay");
        let granted = approval(granted);
        let replayed = approval(replayed);
        assert_eq!(granted, replayed);
        assert_eq!(granted.id, pending.id.to_string());
        assert_eq!(granted.revision, 1);
        assert_eq!(granted.status, v1::ApprovalStatus::Granted as i32);

        let stale = daemon
            .handle(request_with_key(
                request::Operation::DecideApproval(v1::DecideApprovalRequest {
                    approval_id: pending.id.to_string(),
                    expected_revision: 99,
                    decision: v1::ApprovalDecision::Grant as i32,
                }),
                "decide-stale",
            ))
            .await
            .expect("stale response");
        assert_error_code(stale, v1::ErrorCode::Conflict);

        let conflicting_reuse = daemon
            .handle(request_with_key(
                request::Operation::DecideApproval(v1::DecideApprovalRequest {
                    approval_id: pending.id.to_string(),
                    expected_revision: 0,
                    decision: v1::ApprovalDecision::Deny as i32,
                }),
                "decide-grant",
            ))
            .await
            .expect("key conflict response");
        assert_error_code(conflicting_reuse, v1::ErrorCode::Conflict);

        let pending_denial = seed_approval(&daemon, "deny").await;
        let denied = daemon
            .handle(request_with_key(
                request::Operation::DecideApproval(v1::DecideApprovalRequest {
                    approval_id: pending_denial.id.to_string(),
                    expected_revision: 0,
                    decision: v1::ApprovalDecision::Deny as i32,
                }),
                "decide-deny",
            ))
            .await
            .expect("deny response");
        let denied = approval(denied);
        assert_eq!(denied.id, pending_denial.id.to_string());
        assert_eq!(denied.revision, 1);
        assert_eq!(denied.status, v1::ApprovalStatus::Denied as i32);
        let denial_events = daemon
            .runs
            .events_since(&pending_denial.run_id, 0, 100)
            .await
            .expect("denial events");
        assert!(matches!(
            denial_events.last().map(|event| &event.kind),
            Some(RunEventKind::StateChanged {
                to: RunState::Paused,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn expired_approval_is_durably_paused_and_replays_the_same_error() {
        let (daemon, clock) = daemon();
        let pending = seed_approval(&daemon, "expired").await;
        clock.set(91);
        let decision = request::Operation::DecideApproval(v1::DecideApprovalRequest {
            approval_id: pending.id.to_string(),
            expected_revision: 0,
            decision: v1::ApprovalDecision::Grant as i32,
        });

        let first = daemon
            .handle(request_with_key(decision.clone(), "decide-expired"))
            .await
            .expect("expired response");
        let replay = daemon
            .handle(request_with_key(decision, "decide-expired"))
            .await
            .expect("expired replay");
        assert_error_code(first, v1::ErrorCode::InvalidState);
        assert_error_code(replay, v1::ErrorCode::InvalidState);

        let events = daemon
            .runs
            .events_since(&pending.run_id, 0, 100)
            .await
            .expect("expired decision events");
        assert!(matches!(
            events.last().map(|event| &event.kind),
            Some(RunEventKind::StateChanged {
                to: RunState::Paused,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn removed_producer_fields_and_old_epochs_cannot_dispatch_effects() {
        let (daemon, _) = daemon();
        let created = daemon
            .runs
            .create(
                CreateRun {
                    project_id: "project-audit".into(),
                    thread_id: "thread-audit".into(),
                },
                "seed-audit-run",
            )
            .await
            .expect("seed audit run");
        let before = daemon
            .runs
            .events_since(&created.id, 0, 100)
            .await
            .expect("events before");

        let workspace = daemon.workspace().expect("workspace");
        let project = workspace
            .create_project(
                CreateProject {
                    name: "Artifact authority audit".into(),
                    description: String::new(),
                },
                "seed-artifact-audit-project",
            )
            .await
            .expect("seed artifact project");
        // Epoch-5 CreateRun/TransitionRun/RequestApproval occupied oneof fields
        // 3, 4, and 6. Prost discards those now-reserved unknown fields, so the
        // current handler sees no operation and must reject without dispatch.
        for encoded_request in [vec![0x1a, 0x00], vec![0x22, 0x00], vec![0x32, 0x00]] {
            let decoded = v1::Request::decode(encoded_request.as_slice()).expect("legacy request");
            assert!(decoded.operation.is_none());
            let mut envelope = request(request::Operation::Health(v1::HealthRequest {}));
            envelope.payload = Some(envelope::Payload::Request(decoded));
            let response = daemon
                .handle(envelope)
                .await
                .expect("rejected legacy field");
            assert_error_code(response, v1::ErrorCode::InvalidArgument);
        }

        // Epoch-10 Create/Update/DeleteArtifact occupied fields 23-25. Use
        // non-empty legacy payloads, including the live artifact identifier,
        // to prove the current handler can never dispatch any of them.
        for encoded_request in [
            legacy_single_string_request([0xba, 0x01], project.id.as_str()),
            legacy_single_string_request([0xc2, 0x01], "artifact-authority-audit"),
            legacy_single_string_request([0xca, 0x01], "artifact-authority-audit"),
        ] {
            let decoded = v1::Request::decode(encoded_request.as_slice())
                .expect("legacy artifact mutation request");
            assert!(decoded.operation.is_none());
            let mut envelope = request(request::Operation::Health(v1::HealthRequest {}));
            envelope.payload = Some(envelope::Payload::Request(decoded));
            let response = daemon
                .handle(envelope)
                .await
                .expect("rejected legacy artifact mutation");
            assert_error_code(response, v1::ErrorCode::InvalidArgument);
        }

        for version in 0..PROTOCOL_VERSION {
            let mut envelope = request(request::Operation::Health(v1::HealthRequest {}));
            envelope.protocol_version = version;
            assert!(matches!(
                daemon.handle(envelope).await,
                Err(HandlerError::Envelope(EnvelopeError::UnsupportedVersion(actual)))
                    if actual == version
            ));
        }

        let after = daemon
            .runs
            .events_since(&created.id, 0, 100)
            .await
            .expect("events after");
        assert_eq!(after, before);
    }

    #[tokio::test]
    async fn legacy_message_mutations_are_rejected_without_changing_daemon_owned_history() {
        let (daemon, _) = daemon();
        let workspace = daemon.workspace().expect("workspace");
        let project = workspace
            .create_project(
                CreateProject {
                    name: "Message authority audit".into(),
                    description: String::new(),
                },
                "seed-message-audit-project",
            )
            .await
            .expect("seed message project");
        let thread = workspace
            .create_thread(
                CreateThread {
                    project_id: project.id.to_string(),
                    title: "Message authority audit".into(),
                },
                "seed-message-audit-thread",
            )
            .await
            .expect("seed message thread");
        let message = workspace
            .create_message(
                CreateMessage {
                    thread_id: thread.id.to_string(),
                    role: MessageRole::User,
                    content: "Canonical history".into(),
                },
                "seed-daemon-owned-message",
            )
            .await
            .expect("seed message");

        // Epoch-11 Create/Update/DeleteMessage occupied fields 18-20. These
        // valid legacy mutation shapes target live entities, but epoch 12
        // discards them as reserved unknown fields before dispatch.
        for encoded_request in [
            legacy_create_message_request(thread.id.as_str(), "Injected history"),
            legacy_update_message_request(message.id.as_str(), "Mutated history"),
            legacy_single_string_request([0xa2, 0x01], message.id.as_str()),
        ] {
            let decoded = v1::Request::decode(encoded_request.as_slice())
                .expect("legacy message mutation request");
            assert!(decoded.operation.is_none());
            assert!(decoded.encode_to_vec().is_empty());
            let mut envelope = request(request::Operation::Health(v1::HealthRequest {}));
            envelope.payload = Some(envelope::Payload::Request(decoded));
            let response = daemon
                .handle(envelope)
                .await
                .expect("rejected legacy message mutation");
            assert_error_code(response, v1::ErrorCode::InvalidArgument);
        }

        let get_response = daemon
            .handle(request(request::Operation::GetMessage(
                v1::GetMessageRequest {
                    message_id: message.id.to_string(),
                },
            )))
            .await
            .expect("message read remains available");
        assert_eq!(wire_message(get_response), message_to_wire(message.clone()));

        let list_response = daemon
            .handle(request(request::Operation::ListMessages(
                v1::ListMessagesRequest {
                    thread_id: thread.id.to_string(),
                    cursor: String::new(),
                    limit: 10,
                },
            )))
            .await
            .expect("message list remains available");
        let messages = wire_messages(list_response);
        assert_eq!(messages.messages, vec![message_to_wire(message.clone())]);
        assert!(messages.next_cursor.is_empty());
        assert_eq!(
            workspace
                .get_message(&message.id)
                .await
                .expect("message after legacy mutation attempts"),
            message
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn async_conversation_start_poll_cancel_and_saturated_replays_are_exact() {
        let (daemon, [thread_id, second_thread_id], stream_calls) =
            daemon_with_pending_conversation().await;

        let first_start = daemon
            .handle(conversation_start(
                &thread_id,
                "First pending turn",
                "first-start",
            ))
            .await
            .expect("first start response");
        let first = conversation_turn(first_start);
        assert_eq!(first.state, v1::ConversationTurnState::Reserved as i32);
        assert_eq!(first.revision, 0);
        assert_eq!(daemon.conversation_tasks.active_count().await, 1);

        let first_started_events = wait_for_conversation_state_event(
            &daemon,
            &first.turn_id,
            0,
            v1::ConversationTurnState::ProviderStarted,
        )
        .await;
        assert_eq!(stream_calls.load(Ordering::SeqCst), 1);
        let first_cancel = daemon
            .handle(conversation_cancel(&first.turn_id, 1, "first-cancel"))
            .await
            .expect("first cancel response");
        let first_terminal = conversation_turn(first_cancel);
        assert_eq!(
            first_terminal.state,
            v1::ConversationTurnState::InterruptedNeedsReview as i32
        );
        assert_eq!(first_terminal.revision, 2);

        let terminal_events = conversation_event_batch(
            daemon
                .handle(conversation_event_poll(
                    &first.turn_id,
                    first_started_events.next_sequence,
                    100,
                    0,
                ))
                .await
                .expect("terminal event poll"),
        );
        assert!(terminal_events.events.iter().any(|event| {
            event.kind == v1::ConversationTurnEventKind::StateChanged as i32
                && event.to_state == v1::ConversationTurnState::InterruptedNeedsReview as i32
        }));

        let second_start = daemon
            .handle(conversation_start(
                &thread_id,
                "Second pending turn",
                "second-start",
            ))
            .await
            .expect("second start response");
        let second = conversation_turn(second_start);
        assert_eq!(second.state, v1::ConversationTurnState::Reserved as i32);
        wait_for_conversation_state_event(
            &daemon,
            &second.turn_id,
            0,
            v1::ConversationTurnState::ProviderStarted,
        )
        .await;
        assert_eq!(stream_calls.load(Ordering::SeqCst), 2);

        let mut held_slots = Vec::new();
        while daemon.conversation_tasks.available_slots() > 0 {
            held_slots.push(
                daemon
                    .conversation_tasks
                    .try_acquire()
                    .expect("fill conversation capacity"),
            );
        }
        assert_eq!(daemon.conversation_tasks.available_slots(), 0);

        let active_replay = conversation_turn(
            daemon
                .handle(conversation_start(
                    &thread_id,
                    "Second pending turn",
                    "second-start",
                ))
                .await
                .expect("active replay under saturation"),
        );
        assert_eq!(active_replay.turn_id, second.turn_id);
        assert_eq!(
            active_replay.state,
            v1::ConversationTurnState::ProviderStarted as i32
        );

        let terminal_replay = conversation_turn(
            daemon
                .handle(conversation_start(
                    &thread_id,
                    "First pending turn",
                    "first-start",
                ))
                .await
                .expect("terminal replay under saturation"),
        );
        assert_eq!(terminal_replay, first_terminal);

        let saturated_new = daemon
            .handle(conversation_start(
                &second_thread_id,
                "Must not reserve",
                "saturated-new-start",
            ))
            .await
            .expect("bounded saturation response");
        assert_error_code(saturated_new, v1::ErrorCode::Unavailable);
        let unreserved = daemon
            .conversation
            .as_ref()
            .expect("conversation service")
            .replay_start(
                &StartConversationTurn {
                    thread_id: second_thread_id,
                    content: "Must not reserve".into(),
                    model_id: None,
                    search_enabled: false,
                },
                "saturated-new-start",
            )
            .await
            .expect("reservation precheck");
        assert!(unreserved.is_none());

        let second_terminal = conversation_turn(
            daemon
                .handle(conversation_cancel(&second.turn_id, 1, "second-cancel"))
                .await
                .expect("second cancel response"),
        );
        assert_eq!(
            second_terminal.state,
            v1::ConversationTurnState::InterruptedNeedsReview as i32
        );
        drop(held_slots);
        tokio::time::timeout(Duration::from_secs(2), async {
            while daemon.conversation_tasks.available_slots() != MAX_CONVERSATION_TASKS {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("provider task capacity released");
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn orphan_reserved_replay_is_reclaimed_only_when_capacity_is_available() {
        let (daemon, [first_thread, second_thread], stream_calls) =
            daemon_with_pending_conversation().await;
        let conversation = daemon
            .conversation
            .as_ref()
            .expect("conversation service")
            .clone();

        let orphan = conversation
            .start(
                StartConversationTurn {
                    thread_id: first_thread.clone(),
                    content: "Reclaim this reservation".into(),
                    model_id: None,
                    search_enabled: false,
                },
                "reclaim-orphan",
                Box::pin(std::future::pending()),
            )
            .await
            .expect("orphan reservation");
        let orphan_id = orphan.snapshot.turn.id.clone();
        assert_eq!(orphan.snapshot.turn.state, ConversationTurnState::Reserved);
        assert!(
            daemon
                .conversation_tasks
                .ownership(&orphan_id)
                .await
                .is_none()
        );
        drop(orphan);

        let reclaimed = conversation_turn(
            daemon
                .handle(conversation_start(
                    &first_thread,
                    "Reclaim this reservation",
                    "reclaim-orphan",
                ))
                .await
                .expect("orphan replay"),
        );
        assert_eq!(reclaimed.turn_id, orphan_id.to_string());
        wait_for_conversation_state_event(
            &daemon,
            &reclaimed.turn_id,
            0,
            v1::ConversationTurnState::ProviderStarted,
        )
        .await;
        assert_eq!(stream_calls.load(Ordering::SeqCst), 1);
        conversation_turn(
            daemon
                .handle(conversation_cancel(
                    &reclaimed.turn_id,
                    1,
                    "reclaimed-cancel",
                ))
                .await
                .expect("reclaimed cancellation"),
        );
        wait_for_conversation_capacity(&daemon).await;

        let saturated_orphan = conversation
            .start(
                StartConversationTurn {
                    thread_id: second_thread.clone(),
                    content: "Remain reserved while saturated".into(),
                    model_id: None,
                    search_enabled: false,
                },
                "saturated-orphan",
                Box::pin(std::future::pending()),
            )
            .await
            .expect("saturated orphan reservation");
        let saturated_orphan_id = saturated_orphan.snapshot.turn.id.clone();
        drop(saturated_orphan);

        let mut held_slots = Vec::new();
        while daemon.conversation_tasks.available_slots() > 0 {
            held_slots.push(
                daemon
                    .conversation_tasks
                    .try_acquire()
                    .expect("fill conversation capacity"),
            );
        }
        let unavailable = daemon
            .handle(conversation_start(
                &second_thread,
                "Remain reserved while saturated",
                "saturated-orphan",
            ))
            .await
            .expect("saturated orphan response");
        assert_error_code(unavailable, v1::ErrorCode::Unavailable);
        assert!(
            daemon
                .conversation_tasks
                .ownership(&saturated_orphan_id)
                .await
                .is_none()
        );
        let still_reserved = conversation
            .replay_start(
                &StartConversationTurn {
                    thread_id: second_thread.clone(),
                    content: "Remain reserved while saturated".into(),
                    model_id: None,
                    search_enabled: false,
                },
                "saturated-orphan",
            )
            .await
            .expect("saturated orphan load")
            .expect("durable orphan");
        assert_eq!(still_reserved.turn.state, ConversationTurnState::Reserved);
        assert_eq!(still_reserved.turn.revision, 0);
        assert_eq!(stream_calls.load(Ordering::SeqCst), 1);

        drop(held_slots);
        let reclaimed_after_capacity = conversation_turn(
            daemon
                .handle(conversation_start(
                    &second_thread,
                    "Remain reserved while saturated",
                    "saturated-orphan",
                ))
                .await
                .expect("orphan replay after capacity"),
        );
        assert_eq!(
            reclaimed_after_capacity.turn_id,
            saturated_orphan_id.to_string()
        );
        wait_for_conversation_state_event(
            &daemon,
            &reclaimed_after_capacity.turn_id,
            0,
            v1::ConversationTurnState::ProviderStarted,
        )
        .await;
        assert_eq!(stream_calls.load(Ordering::SeqCst), 2);
        conversation_turn(
            daemon
                .handle(conversation_cancel(
                    &reclaimed_after_capacity.turn_id,
                    1,
                    "saturated-orphan-cancel",
                ))
                .await
                .expect("saturated orphan cancellation"),
        );
        wait_for_conversation_capacity(&daemon).await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn replay_during_pre_provider_exit_is_terminal_before_ownership_finishes() {
        let (daemon, [thread_id, poison_thread], stream_calls, store, entered, release, _) =
            daemon_with_blocked_provider_start(false, false).await;
        let started = conversation_turn(
            daemon
                .handle(conversation_start(
                    &thread_id,
                    "Barrier handoff",
                    "barrier-handoff-start",
                ))
                .await
                .expect("initial start"),
        );
        assert_eq!(started.state, v1::ConversationTurnState::Reserved as i32);
        let turn_id = ConversationTurnId::new(started.turn_id.clone()).expect("turn id");

        tokio::time::timeout(Duration::from_secs(2), entered.wait())
            .await
            .expect("provider-start commit barrier");
        assert_eq!(
            daemon.conversation_tasks.ownership(&turn_id).await,
            Some(ConversationTaskOwnership::Active)
        );
        let poison = daemon
            .conversation
            .as_ref()
            .expect("conversation service")
            .start(
                StartConversationTurn {
                    thread_id: poison_thread,
                    content: "Poison external namespace only".into(),
                    model_id: None,
                    search_enabled: false,
                },
                "poison-reservation",
                Box::pin(std::future::pending()),
            )
            .await
            .expect("poison reservation");
        let poison_key = test_dispatch_exit_idempotency_key(&turn_id);
        let poisoned = conversation_turn(
            daemon
                .handle(conversation_cancel(
                    poison.snapshot.turn.id.as_str(),
                    poison.snapshot.turn.revision,
                    &poison_key,
                ))
                .await
                .expect("external namespace poison"),
        );
        assert_eq!(poisoned.state, v1::ConversationTurnState::Cancelled as i32);
        let replay = conversation_turn(
            daemon
                .handle(conversation_start(
                    &thread_id,
                    "Barrier handoff",
                    "barrier-handoff-start",
                ))
                .await
                .expect("owned reserved replay"),
        );
        assert_eq!(replay, started);
        assert_eq!(daemon.conversation_tasks.active_count().await, 1);

        release.wait().await;
        let terminal = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let snapshot = store
                    .load_turn(&turn_id)
                    .await
                    .expect("turn load")
                    .expect("turn");
                if snapshot.turn.state.is_terminal()
                    && daemon.conversation_tasks.active_count().await == 0
                {
                    break snapshot;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("durable task-exit reconciliation");
        assert_eq!(terminal.turn.state, ConversationTurnState::Cancelled);
        assert_eq!(terminal.turn.revision, 1);
        assert!(terminal.effect.is_none());
        assert_eq!(stream_calls.load(Ordering::SeqCst), 0);

        let events = store
            .list_turn_events_since(&turn_id, 0, 100)
            .await
            .expect("turn events");
        assert!(matches!(
            events.events.last().map(|event| &event.kind),
            Some(ConversationTurnEventKind::StateChanged {
                from: ConversationTurnState::Reserved,
                to: ConversationTurnState::Cancelled,
            })
        ));
        let terminal_replay = conversation_turn(
            daemon
                .handle(conversation_start(
                    &thread_id,
                    "Barrier handoff",
                    "barrier-handoff-start",
                ))
                .await
                .expect("terminal handoff replay"),
        );
        assert_eq!(
            terminal_replay.state,
            v1::ConversationTurnState::Cancelled as i32
        );
        assert_eq!(terminal_replay.revision, 1);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn epoch_eight_retry_is_lineage_bound_exact_and_daemon_dispatched() {
        let (daemon, [thread_id, _], stream_calls, _store, entered, release, _) =
            daemon_with_blocked_provider_start(false, false).await;
        let source = conversation_turn(
            daemon
                .handle(conversation_start(
                    &thread_id,
                    "Retry this daemon-owned prompt",
                    "retry-source-start",
                ))
                .await
                .expect("source start"),
        );
        assert_eq!(source.state, v1::ConversationTurnState::Reserved as i32);
        assert_eq!(
            source.retry_eligibility,
            v1::ConversationRetryEligibility::SourceInProgress as i32
        );
        tokio::time::timeout(Duration::from_secs(2), entered.wait())
            .await
            .expect("blocked provider-start commit");

        let cancelled = conversation_turn(
            daemon
                .handle(conversation_cancel(
                    &source.turn_id,
                    source.revision,
                    "retry-source-cancel",
                ))
                .await
                .expect("source cancellation"),
        );
        assert_eq!(cancelled.state, v1::ConversationTurnState::Cancelled as i32);
        assert_eq!(cancelled.revision, 1);
        assert_eq!(
            cancelled.retry_eligibility,
            v1::ConversationRetryEligibility::Allowed as i32
        );
        release.wait().await;
        wait_for_conversation_capacity(&daemon).await;
        assert_eq!(stream_calls.load(Ordering::SeqCst), 0);

        let retried = conversation_turn(
            daemon
                .handle(conversation_retry(
                    &cancelled.turn_id,
                    cancelled.revision,
                    "retry-command",
                ))
                .await
                .expect("retry reservation"),
        );
        assert_ne!(retried.turn_id, cancelled.turn_id);
        assert_eq!(
            retried
                .user_message
                .as_ref()
                .map(|message| (&message.role, &message.content)),
            cancelled
                .user_message
                .as_ref()
                .map(|message| (&message.role, &message.content)),
        );
        assert_ne!(
            retried.user_message.as_ref().map(|message| &message.id),
            cancelled.user_message.as_ref().map(|message| &message.id),
        );
        assert_eq!(retried.model_id, cancelled.model_id);
        assert_eq!(
            retried.retry_eligibility,
            v1::ConversationRetryEligibility::SourceInProgress as i32
        );
        let lineage = retried.lineage.as_ref().expect("retry lineage");
        assert_eq!(lineage.origin, v1::ConversationTurnOrigin::Retry as i32);
        assert_eq!(lineage.source_turn_id, cancelled.turn_id);
        assert_eq!(lineage.retry_depth, 1);
        wait_for_conversation_state_event(
            &daemon,
            &retried.turn_id,
            0,
            v1::ConversationTurnState::ProviderStarted,
        )
        .await;
        assert_eq!(stream_calls.load(Ordering::SeqCst), 1);

        let replay = conversation_turn(
            daemon
                .handle(conversation_retry(
                    &cancelled.turn_id,
                    cancelled.revision,
                    "retry-command",
                ))
                .await
                .expect("active retry replay"),
        );
        assert_eq!(replay.turn_id, retried.turn_id);
        assert_eq!(
            replay.state,
            v1::ConversationTurnState::ProviderStarted as i32
        );
        assert_eq!(stream_calls.load(Ordering::SeqCst), 1);

        let conflicting = daemon
            .handle(conversation_retry(
                &cancelled.turn_id,
                cancelled.revision.saturating_sub(1),
                "retry-command",
            ))
            .await
            .expect("conflicting replay response");
        assert_error_code(conflicting, v1::ErrorCode::Conflict);

        let terminal = conversation_turn(
            daemon
                .handle(conversation_cancel(
                    &retried.turn_id,
                    replay.revision,
                    "retry-stop",
                ))
                .await
                .expect("retry stop"),
        );
        assert_eq!(
            terminal.state,
            v1::ConversationTurnState::InterruptedNeedsReview as i32
        );
        assert_eq!(
            terminal.retry_eligibility,
            v1::ConversationRetryEligibility::SourceInterruptedNeedsReview as i32
        );
        wait_for_conversation_capacity(&daemon).await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn epoch_ten_branch_delivery_is_reconciled_acknowledged_and_provider_free() {
        let (daemon, source, store, model) = daemon_with_completed_fork_source().await;
        let parent_before = store
            .list_messages(&source.turn.thread_id, None, 100)
            .await
            .expect("parent history before Branch");
        let baseline_list_calls = model.list_calls.load(Ordering::SeqCst);
        let baseline_stream_calls = model.stream_calls.load(Ordering::SeqCst);

        let root_metadata = conversation_fork_metadata(
            daemon
                .handle(conversation_get_fork_metadata(
                    source.turn.thread_id.as_str(),
                ))
                .await
                .expect("root metadata response"),
        );
        assert_eq!(root_metadata.family_threads.len(), 1);
        assert!(root_metadata.inherited_assistant_outcomes.is_empty());
        let root_lineage = root_metadata.lineage.as_ref().expect("root lineage");
        assert_eq!(root_lineage.root_thread_id, source.turn.thread_id.as_str());
        assert_eq!(root_lineage.fork_depth, 0);
        assert!(matches!(
            root_lineage.origin,
            Some(v1::conversation_thread_lineage::Origin::Original(_))
        ));

        let mut held_slots = Vec::new();
        while daemon.conversation_tasks.available_slots() > 0 {
            held_slots.push(
                daemon
                    .conversation_tasks
                    .try_acquire()
                    .expect("saturate provider task capacity"),
            );
        }
        assert_eq!(daemon.conversation_tasks.available_slots(), 0);

        let first = conversation_fork(
            daemon
                .handle(conversation_branch(
                    source.turn.id.as_str(),
                    source.turn.revision,
                    "branch-handler-command",
                ))
                .await
                .expect("Branch response"),
        );
        assert!(first.started_turn.is_none());
        let child = first.child_thread.as_ref().expect("Branch child");
        let first_delivery = first.delivery.as_ref().expect("Branch delivery");
        assert_eq!(first_delivery.child_thread_id, child.id);
        assert_eq!(
            first_delivery.state,
            v1::ConversationForkDeliveryState::Pending as i32
        );
        assert_eq!(first_delivery.revision, 0);
        assert_ne!(child.id, source.turn.thread_id.as_str());
        assert_eq!(child.project_id, source.turn.project_id.as_str());
        assert_eq!(child.title, "Fork source");
        let child_lineage = child.lineage.as_ref().expect("Branch lineage");
        assert_eq!(child_lineage.root_thread_id, source.turn.thread_id.as_str());
        assert_eq!(child_lineage.fork_depth, 1);
        let Some(v1::conversation_thread_lineage::Origin::Fork(branch_origin)) =
            child_lineage.origin.as_ref()
        else {
            panic!("Branch fork origin");
        };
        assert_eq!(
            branch_origin.parent_thread_id,
            source.turn.thread_id.as_str()
        );
        assert_eq!(branch_origin.source_turn_id, source.turn.id.as_str());
        assert_eq!(
            branch_origin.source_message_id,
            source
                .assistant_message
                .as_ref()
                .expect("source assistant")
                .id
                .as_str()
        );
        assert_eq!(branch_origin.kind, v1::ConversationForkKind::Branch as i32);
        assert_eq!(daemon.conversation_tasks.active_count().await, 0);
        assert_eq!(daemon.conversation_tasks.available_slots(), 0);
        assert_eq!(model.list_calls.load(Ordering::SeqCst), baseline_list_calls);
        assert_eq!(
            model.stream_calls.load(Ordering::SeqCst),
            baseline_stream_calls
        );

        let replay = conversation_fork(
            daemon
                .handle(conversation_branch(
                    source.turn.id.as_str(),
                    source.turn.revision,
                    "branch-handler-command",
                ))
                .await
                .expect("Branch replay"),
        );
        assert_eq!(replay, first);
        assert_eq!(daemon.conversation_tasks.active_count().await, 0);
        assert_eq!(model.list_calls.load(Ordering::SeqCst), baseline_list_calls);
        assert_eq!(
            model.stream_calls.load(Ordering::SeqCst),
            baseline_stream_calls
        );

        let alias = conversation_fork(
            daemon
                .handle(conversation_branch(
                    source.turn.id.as_str(),
                    source.turn.revision,
                    "branch-handler-alias",
                ))
                .await
                .expect("pending Branch alias reconciliation"),
        );
        assert_eq!(alias, first);
        assert_eq!(daemon.conversation_tasks.available_slots(), 0);
        assert_eq!(model.list_calls.load(Ordering::SeqCst), baseline_list_calls);
        assert_eq!(
            model.stream_calls.load(Ordering::SeqCst),
            baseline_stream_calls
        );

        let acknowledged = conversation_fork_delivery(
            daemon
                .handle(conversation_ack_fork_delivery(
                    &child.id,
                    0,
                    "branch-handler-delivery-ack",
                ))
                .await
                .expect("Branch delivery acknowledgement"),
        );
        assert_eq!(acknowledged.child_thread_id, child.id);
        assert_eq!(
            acknowledged.state,
            v1::ConversationForkDeliveryState::Acknowledged as i32
        );
        assert_eq!(acknowledged.revision, 1);
        let acknowledged_replay = conversation_fork_delivery(
            daemon
                .handle(conversation_ack_fork_delivery(
                    &child.id,
                    0,
                    "branch-handler-delivery-ack",
                ))
                .await
                .expect("Branch delivery acknowledgement replay"),
        );
        assert_eq!(acknowledged_replay, acknowledged);
        let conflicting_ack = daemon
            .handle(conversation_ack_fork_delivery(
                &child.id,
                0,
                "branch-handler-delivery-ack-new-key",
            ))
            .await
            .expect("conflicting Branch delivery acknowledgement response");
        assert_error_code(conflicting_ack, v1::ErrorCode::Conflict);

        let alias_after_ack = conversation_fork(
            daemon
                .handle(conversation_branch(
                    source.turn.id.as_str(),
                    source.turn.revision,
                    "branch-handler-alias",
                ))
                .await
                .expect("exact Branch alias after acknowledgement"),
        );
        assert_eq!(alias_after_ack.child_thread, first.child_thread);
        assert_eq!(
            alias_after_ack
                .delivery
                .as_ref()
                .expect("acknowledged alias delivery")
                .state,
            v1::ConversationForkDeliveryState::Acknowledged as i32
        );

        let conflicting = daemon
            .handle(conversation_branch(
                source.turn.id.as_str(),
                source.turn.revision.saturating_sub(1),
                "branch-handler-command",
            ))
            .await
            .expect("conflicting Branch response");
        assert_error_code(conflicting, v1::ErrorCode::Conflict);
        assert_eq!(daemon.conversation_tasks.active_count().await, 0);
        drop(held_slots);
        wait_for_conversation_capacity(&daemon).await;

        let metadata = conversation_fork_metadata(
            daemon
                .handle(conversation_get_fork_metadata(&child.id))
                .await
                .expect("Branch metadata response"),
        );
        assert_eq!(metadata.lineage.as_ref(), Some(child_lineage));
        assert_eq!(metadata.family_threads.len(), 2);
        assert!(
            metadata
                .family_threads
                .iter()
                .any(|thread| thread.id == child.id)
        );
        assert!(metadata.family_threads.iter().any(|thread| {
            thread.id == source.turn.thread_id.as_str()
                && matches!(
                    thread
                        .lineage
                        .as_ref()
                        .and_then(|lineage| lineage.origin.as_ref()),
                    Some(v1::conversation_thread_lineage::Origin::Original(_))
                )
        }));
        let [outcome] = metadata.inherited_assistant_outcomes.as_slice() else {
            panic!("one inherited assistant outcome");
        };
        assert_ne!(
            outcome.child_assistant_message_id,
            source
                .assistant_message
                .as_ref()
                .expect("source assistant")
                .id
                .as_str()
        );
        assert_eq!(outcome.source_turn_id, source.turn.id.as_str());
        assert_eq!(outcome.model_id, source.turn.model_id);
        assert_eq!(outcome.citations.len(), 1);
        assert_eq!(
            outcome.citations[0].url,
            "https://example.test/canonical-source"
        );
        assert_eq!(
            outcome.usage.as_ref().map(|usage| usage.input_tokens),
            Some(11)
        );
        assert_eq!(
            outcome.usage.as_ref().map(|usage| usage.output_tokens),
            Some(7)
        );
        assert_eq!(outcome.zero_data_retention, Some(true));
        assert_eq!(
            store
                .list_messages(&source.turn.thread_id, None, 100)
                .await
                .expect("parent history after Branch"),
            parent_before
        );

        let deliberate_second = conversation_fork(
            daemon
                .handle(conversation_branch(
                    source.turn.id.as_str(),
                    source.turn.revision,
                    "branch-handler-deliberate-second",
                ))
                .await
                .expect("deliberate second Branch after acknowledgement"),
        );
        assert_ne!(deliberate_second.child_thread, first.child_thread);
        let second_delivery = deliberate_second
            .delivery
            .as_ref()
            .expect("second Branch pending delivery");
        assert_eq!(
            second_delivery.state,
            v1::ConversationForkDeliveryState::Pending as i32
        );
        assert_eq!(second_delivery.revision, 0);
        assert_eq!(model.list_calls.load(Ordering::SeqCst), baseline_list_calls);
        assert_eq!(
            model.stream_calls.load(Ordering::SeqCst),
            baseline_stream_calls
        );

        let malformed = daemon
            .handle(conversation_branch(
                "",
                source.turn.revision,
                "malformed-branch-command",
            ))
            .await
            .expect("malformed Branch response");
        assert_error_code(malformed, v1::ErrorCode::InvalidArgument);

        let ineligible = daemon
            .conversation
            .as_ref()
            .expect("conversation service")
            .start(
                StartConversationTurn {
                    thread_id: source.turn.thread_id.to_string(),
                    content: "Still in progress".into(),
                    model_id: None,
                    search_enabled: false,
                },
                "ineligible-branch-source",
                Box::pin(std::future::pending()),
            )
            .await
            .expect("ineligible source reservation");
        let ineligible_response = daemon
            .handle(conversation_branch(
                ineligible.snapshot.turn.id.as_str(),
                ineligible.snapshot.turn.revision,
                "ineligible-branch-command",
            ))
            .await
            .expect("ineligible Branch response");
        assert_error_code(ineligible_response, v1::ErrorCode::InvalidState);
        assert_eq!(daemon.conversation_tasks.active_count().await, 0);
        drop(ineligible.dispatch);
    }

    #[tokio::test]
    async fn epoch_ten_pending_alias_reclaims_a_reserved_fork_without_an_owner() {
        let (daemon, source, _store, model) = daemon_with_completed_fork_source().await;
        let conversation = daemon
            .conversation
            .as_ref()
            .expect("conversation service")
            .clone();
        let orphaned = conversation
            .edit_and_branch(
                EditAndBranchConversationTurn {
                    source_turn_id: source.turn.id.to_string(),
                    expected_revision: source.turn.revision,
                    content: "Reserved child awaiting task ownership".into(),
                },
                "orphaned-edit-canonical-key",
                Box::pin(std::future::pending()),
            )
            .await
            .expect("reserve orphaned edit child");
        assert!(!orphaned.reconciled_pending_delivery);
        assert!(orphaned.dispatch.is_some());
        let child_id = orphaned.snapshot.child_thread.id.clone();
        let turn_id = orphaned
            .snapshot
            .started_turn
            .as_ref()
            .expect("orphaned child turn")
            .turn
            .id
            .clone();
        assert_eq!(daemon.conversation_tasks.active_count().await, 0);
        assert_eq!(model.stream_calls.load(Ordering::SeqCst), 0);

        let reclaimed = conversation_fork(
            daemon
                .handle(conversation_edit_and_branch(
                    source.turn.id.as_str(),
                    source.turn.revision,
                    "Reserved child awaiting task ownership",
                    "orphaned-edit-alias-key",
                ))
                .await
                .expect("reclaim reserved child through pending alias"),
        );
        assert_eq!(
            reclaimed
                .child_thread
                .as_ref()
                .map(|thread| thread.id.as_str()),
            Some(child_id.as_str())
        );
        assert_eq!(
            reclaimed
                .started_turn
                .as_ref()
                .map(|turn| turn.turn_id.as_str()),
            Some(turn_id.as_str())
        );
        wait_for_conversation_state_event(
            &daemon,
            turn_id.as_str(),
            0,
            v1::ConversationTurnState::ProviderStarted,
        )
        .await;
        assert_eq!(model.stream_calls.load(Ordering::SeqCst), 1);
        assert_eq!(daemon.conversation_tasks.active_count().await, 1);

        let active = conversation_fork(
            daemon
                .handle(conversation_edit_and_branch(
                    source.turn.id.as_str(),
                    source.turn.revision,
                    "Reserved child awaiting task ownership",
                    "orphaned-edit-alias-key",
                ))
                .await
                .expect("active reclaimed child replay"),
        );
        let active_turn = active.started_turn.expect("active reclaimed turn");
        assert_eq!(
            active_turn.state,
            v1::ConversationTurnState::ProviderStarted as i32
        );
        let terminal = conversation_turn(
            daemon
                .handle(conversation_cancel(
                    &active_turn.turn_id,
                    active_turn.revision,
                    "orphaned-edit-stop",
                ))
                .await
                .expect("cancel reclaimed child"),
        );
        assert_eq!(
            terminal.state,
            v1::ConversationTurnState::InterruptedNeedsReview as i32
        );
        wait_for_conversation_capacity(&daemon).await;
        drop(orphaned.dispatch);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn epoch_ten_edit_and_regenerate_dispatch_exact_child_turns_and_replay() {
        let (daemon, source, store, model) = daemon_with_completed_fork_source().await;
        let parent_before = store
            .list_messages(&source.turn.thread_id, None, 100)
            .await
            .expect("parent history before dispatching forks");
        let baseline_list_calls = model.list_calls.load(Ordering::SeqCst);

        let edited = conversation_fork(
            daemon
                .handle(conversation_edit_and_branch(
                    source.turn.id.as_str(),
                    source.turn.revision,
                    "Edited canonical prompt",
                    "edit-handler-command",
                ))
                .await
                .expect("Edit-and-branch response"),
        );
        let edited_child = edited.child_thread.as_ref().expect("edited child");
        let edited_turn = edited.started_turn.as_ref().expect("edited turn");
        assert_eq!(
            edited_turn.state,
            v1::ConversationTurnState::Reserved as i32
        );
        assert_eq!(edited_turn.model_id, source.turn.model_id);
        assert_eq!(
            edited_turn
                .user_message
                .as_ref()
                .map(|message| message.content.as_str()),
            Some("Edited canonical prompt")
        );
        assert_ne!(
            edited_turn
                .user_message
                .as_ref()
                .map(|message| message.id.as_str()),
            Some(source.user_message.id.as_str())
        );
        let edited_turn_lineage = edited_turn.lineage.as_ref().expect("edited turn lineage");
        assert_eq!(
            edited_turn_lineage.origin,
            v1::ConversationTurnOrigin::EditAndBranch as i32
        );
        assert_eq!(edited_turn_lineage.source_turn_id, source.turn.id.as_str());
        assert_eq!(edited_turn_lineage.retry_depth, 0);
        assert_forked_thread_origin(
            edited_child,
            &source,
            source.user_message.id.as_str(),
            v1::ConversationForkKind::EditAndBranch,
        );
        assert_eq!(daemon.conversation_tasks.active_count().await, 1);
        wait_for_conversation_state_event(
            &daemon,
            &edited_turn.turn_id,
            0,
            v1::ConversationTurnState::ProviderStarted,
        )
        .await;
        assert_eq!(model.stream_calls.load(Ordering::SeqCst), 1);
        {
            let requests = model.requests.lock().expect("recorded edit request");
            let [request] = requests.as_slice() else {
                panic!("one edited provider request");
            };
            assert_exact_fork_request(request, &source, "Edited canonical prompt");
        }

        let edited_replay = conversation_fork(
            daemon
                .handle(conversation_edit_and_branch(
                    source.turn.id.as_str(),
                    source.turn.revision,
                    "Edited canonical prompt",
                    "edit-handler-command",
                ))
                .await
                .expect("active Edit-and-branch replay"),
        );
        assert_eq!(edited_replay.child_thread, edited.child_thread);
        let edited_replay_turn = edited_replay.started_turn.expect("edited replay turn");
        assert_eq!(edited_replay_turn.turn_id, edited_turn.turn_id);
        assert_eq!(
            edited_replay_turn.state,
            v1::ConversationTurnState::ProviderStarted as i32
        );
        assert_eq!(model.stream_calls.load(Ordering::SeqCst), 1);

        let conflicting_edit = daemon
            .handle(conversation_edit_and_branch(
                source.turn.id.as_str(),
                source.turn.revision,
                "Conflicting edited prompt",
                "edit-handler-command",
            ))
            .await
            .expect("conflicting edit response");
        assert_error_code(conflicting_edit, v1::ErrorCode::Conflict);
        assert_eq!(model.stream_calls.load(Ordering::SeqCst), 1);

        let edited_terminal = conversation_turn(
            daemon
                .handle(conversation_cancel(
                    &edited_turn.turn_id,
                    edited_replay_turn.revision,
                    "edit-handler-stop",
                ))
                .await
                .expect("edited turn cancellation"),
        );
        assert_eq!(
            edited_terminal.state,
            v1::ConversationTurnState::InterruptedNeedsReview as i32
        );
        wait_for_conversation_capacity(&daemon).await;

        let regenerated = conversation_fork(
            daemon
                .handle(conversation_regenerate(
                    source.turn.id.as_str(),
                    source.turn.revision,
                    "regenerate-handler-command",
                ))
                .await
                .expect("Regenerate response"),
        );
        let regenerated_child = regenerated
            .child_thread
            .as_ref()
            .expect("regenerated child");
        let regenerated_turn = regenerated.started_turn.as_ref().expect("regenerated turn");
        assert_eq!(
            regenerated_turn.state,
            v1::ConversationTurnState::Reserved as i32
        );
        assert_eq!(regenerated_turn.model_id, source.turn.model_id);
        assert_eq!(
            regenerated_turn
                .user_message
                .as_ref()
                .map(|message| message.content.as_str()),
            Some(source.user_message.content.as_str())
        );
        assert_ne!(
            regenerated_turn
                .user_message
                .as_ref()
                .map(|message| message.id.as_str()),
            Some(source.user_message.id.as_str())
        );
        let regenerated_lineage = regenerated_turn
            .lineage
            .as_ref()
            .expect("regenerated turn lineage");
        assert_eq!(
            regenerated_lineage.origin,
            v1::ConversationTurnOrigin::Regenerate as i32
        );
        assert_eq!(regenerated_lineage.source_turn_id, source.turn.id.as_str());
        assert_eq!(regenerated_lineage.retry_depth, 0);
        assert_forked_thread_origin(
            regenerated_child,
            &source,
            source
                .assistant_message
                .as_ref()
                .expect("source assistant")
                .id
                .as_str(),
            v1::ConversationForkKind::Regenerate,
        );
        assert_eq!(daemon.conversation_tasks.active_count().await, 1);
        wait_for_conversation_state_event(
            &daemon,
            &regenerated_turn.turn_id,
            0,
            v1::ConversationTurnState::ProviderStarted,
        )
        .await;
        assert_eq!(model.stream_calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            model.list_calls.load(Ordering::SeqCst),
            baseline_list_calls + 2
        );
        {
            let requests = model.requests.lock().expect("recorded regenerate request");
            let [edited_request, regenerated_request] = requests.as_slice() else {
                panic!("edit and regenerate provider requests");
            };
            assert_exact_fork_request(edited_request, &source, "Edited canonical prompt");
            assert_exact_fork_request(
                regenerated_request,
                &source,
                source.user_message.content.as_str(),
            );
        }

        let regenerated_replay = conversation_fork(
            daemon
                .handle(conversation_regenerate(
                    source.turn.id.as_str(),
                    source.turn.revision,
                    "regenerate-handler-command",
                ))
                .await
                .expect("active Regenerate replay"),
        );
        assert_eq!(regenerated_replay.child_thread, regenerated.child_thread);
        let regenerated_replay_turn = regenerated_replay
            .started_turn
            .expect("regenerated replay turn");
        assert_eq!(regenerated_replay_turn.turn_id, regenerated_turn.turn_id);
        assert_eq!(
            regenerated_replay_turn.state,
            v1::ConversationTurnState::ProviderStarted as i32
        );
        assert_eq!(model.stream_calls.load(Ordering::SeqCst), 2);

        let conflicting_regenerate = daemon
            .handle(conversation_regenerate(
                source.turn.id.as_str(),
                source.turn.revision.saturating_sub(1),
                "regenerate-handler-command",
            ))
            .await
            .expect("conflicting regenerate response");
        assert_error_code(conflicting_regenerate, v1::ErrorCode::Conflict);
        assert_eq!(model.stream_calls.load(Ordering::SeqCst), 2);

        let regenerate_metadata = conversation_fork_metadata(
            daemon
                .handle(conversation_get_fork_metadata(&regenerated_child.id))
                .await
                .expect("Regenerate metadata response"),
        );
        assert_eq!(
            regenerate_metadata.lineage.as_ref(),
            regenerated_child.lineage.as_ref()
        );
        assert_eq!(regenerate_metadata.family_threads.len(), 3);
        assert!(regenerate_metadata.inherited_assistant_outcomes.is_empty());
        assert!(
            regenerate_metadata
                .family_threads
                .iter()
                .any(|thread| thread.id == edited_child.id)
        );

        let regenerated_terminal = conversation_turn(
            daemon
                .handle(conversation_cancel(
                    &regenerated_turn.turn_id,
                    regenerated_replay_turn.revision,
                    "regenerate-handler-stop",
                ))
                .await
                .expect("regenerated turn cancellation"),
        );
        assert_eq!(
            regenerated_terminal.state,
            v1::ConversationTurnState::InterruptedNeedsReview as i32
        );
        wait_for_conversation_capacity(&daemon).await;
        assert_eq!(
            store
                .list_messages(&source.turn.thread_id, None, 100)
                .await
                .expect("parent history after dispatching forks"),
            parent_before
        );
    }

    #[tokio::test]
    async fn ambiguous_provider_start_exit_fails_closed_to_review_before_finish() {
        let (daemon, [thread_id, _], stream_calls, store, entered, release, _) =
            daemon_with_blocked_provider_start(true, false).await;
        let started = conversation_turn(
            daemon
                .handle(conversation_start(
                    &thread_id,
                    "Ambiguous provider boundary",
                    "ambiguous-provider-start",
                ))
                .await
                .expect("initial start"),
        );
        let turn_id = ConversationTurnId::new(started.turn_id).expect("turn id");
        tokio::time::timeout(Duration::from_secs(2), entered.wait())
            .await
            .expect("provider-start commit barrier");
        release.wait().await;

        let terminal = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let snapshot = store
                    .load_turn(&turn_id)
                    .await
                    .expect("turn load")
                    .expect("turn");
                if snapshot.turn.state == ConversationTurnState::InterruptedNeedsReview
                    && daemon.conversation_tasks.active_count().await == 0
                {
                    break snapshot;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("ambiguous provider-start reconciliation");
        assert_eq!(terminal.turn.revision, 2);
        assert_eq!(terminal.run.state, RunState::InterruptedNeedsReview);
        assert_eq!(
            terminal.effect.expect("provider effect").state,
            EffectState::NeedsReview
        );
        assert_eq!(stream_calls.load(Ordering::SeqCst), 0);
        let events = store
            .list_turn_events_since(&turn_id, 0, 100)
            .await
            .expect("turn events");
        assert!(matches!(
            events.events.last().map(|event| &event.kind),
            Some(ConversationTurnEventKind::StateChanged {
                from: ConversationTurnState::ProviderStarted,
                to: ConversationTurnState::InterruptedNeedsReview,
            })
        ));
    }

    #[tokio::test]
    async fn persistent_reconciliation_failure_is_bounded_and_quarantines_capacity() {
        let (daemon, [thread_id, _], stream_calls, store, entered, release, calls) =
            daemon_with_blocked_provider_start(true, true).await;
        let started = conversation_turn(
            daemon
                .handle(conversation_start(
                    &thread_id,
                    "Persistent reconciliation failure",
                    "persistent-reconciliation-start",
                ))
                .await
                .expect("initial start"),
        );
        let turn_id = ConversationTurnId::new(started.turn_id).expect("turn id");
        tokio::time::timeout(Duration::from_secs(2), entered.wait())
            .await
            .expect("provider-start commit barrier");
        release.wait().await;

        tokio::time::timeout(Duration::from_secs(3), async {
            while daemon.conversation_tasks.active_count().await != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("bounded reconciliation exit");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            MAX_CONVERSATION_RECONCILIATION_ATTEMPTS
        );
        assert_eq!(
            daemon.conversation_tasks.available_slots(),
            MAX_CONVERSATION_TASKS - 1
        );
        assert!(daemon.conversation_tasks.is_quarantined(&turn_id).await);
        assert_eq!(
            store
                .load_turn(&turn_id)
                .await
                .expect("turn load")
                .expect("turn")
                .turn
                .state,
            ConversationTurnState::ProviderStarted
        );
        assert_eq!(stream_calls.load(Ordering::SeqCst), 0);

        let blocked_replay = daemon
            .handle(conversation_start(
                &thread_id,
                "Persistent reconciliation failure",
                "persistent-reconciliation-start",
            ))
            .await
            .expect("quarantined replay response");
        assert_error_code(blocked_replay, v1::ErrorCode::Unavailable);
        let cancelled = conversation_turn(
            daemon
                .handle(conversation_cancel(
                    turn_id.as_str(),
                    1,
                    "quarantined-explicit-cancel",
                ))
                .await
                .expect("quarantine cancellation"),
        );
        assert_eq!(
            cancelled.state,
            v1::ConversationTurnState::InterruptedNeedsReview as i32
        );
        assert!(
            daemon
                .conversation_tasks
                .ownership(&turn_id)
                .await
                .is_none()
        );
        assert_eq!(
            daemon.conversation_tasks.available_slots(),
            MAX_CONVERSATION_TASKS
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn terminal_observations_release_quarantined_capacity_without_restart() {
        let store = Arc::new(InMemoryExecutionStore::new());
        let (daemon, [thread_id, _], _) =
            daemon_with_pending_conversation_store(store.clone(), store.clone()).await;
        let started = conversation_turn(
            daemon
                .handle(conversation_start(
                    &thread_id,
                    "Durable terminal winner",
                    "terminal-observation-start",
                ))
                .await
                .expect("initial start"),
        );
        wait_for_conversation_state_event(
            &daemon,
            &started.turn_id,
            0,
            v1::ConversationTurnState::ProviderStarted,
        )
        .await;

        let turn_id = ConversationTurnId::new(started.turn_id.clone()).expect("turn id");
        let provider_started = store
            .load_turn(&turn_id)
            .await
            .expect("turn load")
            .expect("turn");
        let answer = "Durably completed answer";
        store
            .append_turn_text(&turn_id, provider_started.turn.revision, 0, answer.into())
            .await
            .expect("durable assistant text");
        let transition_at = provider_started.turn.updated_at + 1;
        let mut turn = provider_started.turn.clone();
        let mut run = provider_started.run.clone();
        let mut effect = provider_started.effect.clone().expect("provider effect");
        let assistant = Message::new(
            MessageId::new("terminal-observation-assistant").expect("assistant id"),
            turn.thread_id.clone(),
            MessageRole::Assistant,
            answer.into(),
            transition_at,
        )
        .expect("assistant message");
        turn.complete(
            assistant.id.clone(),
            Some("terminal-observation-response".into()),
            Vec::new(),
            ConversationUsage::default(),
            Some(true),
            transition_at,
        )
        .expect("complete turn");
        run.transition(RunState::Completed, transition_at)
            .expect("complete run");
        effect.finish(true, transition_at).expect("finish effect");
        let completed = store
            .commit_terminal(TerminalTurnCommit {
                turn,
                expected_turn_revision: provider_started.turn.revision,
                run,
                expected_run_revision: provider_started.run.revision,
                effect: Some(effect),
                expected_effect_revision: provider_started
                    .effect
                    .as_ref()
                    .map(|value| value.revision),
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
            })
            .await
            .expect("durable terminal commit");
        assert!(daemon.conversation_tasks.signal(&turn_id).await);
        wait_for_all_conversation_slots(&daemon).await;

        quarantine_turn_for_test(&daemon, &turn_id).await;
        let cancel_winner = conversation_turn(
            daemon
                .handle(conversation_cancel(
                    turn_id.as_str(),
                    completed
                        .turn
                        .revision
                        .checked_sub(1)
                        .expect("pre-terminal revision"),
                    "terminal-observation-cancel",
                ))
                .await
                .expect("terminal cancellation winner"),
        );
        assert_eq!(
            cancel_winner.state,
            v1::ConversationTurnState::Completed as i32
        );
        assert_eq!(
            daemon.conversation_tasks.available_slots(),
            MAX_CONVERSATION_TASKS
        );

        quarantine_turn_for_test(&daemon, &turn_id).await;
        let start_winner = conversation_turn(
            daemon
                .handle(conversation_start(
                    &thread_id,
                    "Durable terminal winner",
                    "terminal-observation-start",
                ))
                .await
                .expect("terminal start winner"),
        );
        assert_eq!(
            start_winner.state,
            v1::ConversationTurnState::Completed as i32
        );
        assert_eq!(
            daemon.conversation_tasks.available_slots(),
            MAX_CONVERSATION_TASKS
        );

        quarantine_turn_for_test(&daemon, &turn_id).await;
        let listed = daemon
            .handle(conversation_list(&thread_id))
            .await
            .expect("terminal list observation");
        let envelope::Payload::Response(response) = listed.payload.expect("payload") else {
            panic!("response")
        };
        let Some(response::Result::ConversationTurns(page)) = response.result else {
            panic!("conversation turns")
        };
        assert!(page.turns.iter().any(|candidate| {
            candidate.turn_id == turn_id.as_str()
                && candidate.state == v1::ConversationTurnState::Completed as i32
        }));
        assert_eq!(
            daemon.conversation_tasks.available_slots(),
            MAX_CONVERSATION_TASKS
        );
        assert!(
            daemon
                .conversation_tasks
                .ownership(&turn_id)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn run_event_long_poll_resumes_from_a_bounded_durable_cursor() {
        let (daemon, _) = daemon();
        let created = daemon
            .runs
            .create(
                CreateRun {
                    project_id: "project-1".into(),
                    thread_id: "thread-1".into(),
                },
                "seed-run",
            )
            .await
            .expect("create run");
        let id = created.id.to_string();
        daemon
            .runs
            .transition(&created.id, 0, RunState::Planning, "seed-transition")
            .await
            .expect("transition run");

        let first = daemon
            .handle(run_event_poll(&id, 0, 1, 0, 5_000))
            .await
            .expect("first event batch");
        let first = run_event_batch(first);
        assert_eq!(first.events.len(), 1);
        assert_eq!(first.events[0].sequence, 1);
        assert_eq!(first.events[0].run_id, id);
        assert_eq!(first.events[0].kind, v1::RunEventKind::Created as i32);
        assert_eq!(first.next_sequence, 1);
        assert!(first.has_more);

        let second = daemon
            .handle(run_event_poll(&id, first.next_sequence, 2, 0, 5_000))
            .await
            .expect("resumed event batch");
        let second = run_event_batch(second);
        assert_eq!(second.events.len(), 1);
        assert_eq!(second.events[0].sequence, 2);
        assert_eq!(second.events[0].kind, v1::RunEventKind::StateChanged as i32);
        assert_eq!(second.next_sequence, 2);
        assert!(!second.has_more);

        let empty = daemon
            .handle(run_event_poll(&id, second.next_sequence, 2, 5, 5_000))
            .await
            .expect("empty event batch");
        let empty = run_event_batch(empty);
        assert!(empty.events.is_empty());
        assert_eq!(empty.next_sequence, 2);
        assert!(!empty.has_more);
    }

    #[tokio::test]
    async fn run_event_long_poll_rejects_missing_runs_and_unbounded_requests() {
        let (daemon, _) = daemon();
        let missing = daemon
            .handle(run_event_poll("missing-run", 0, 1, 5, 5_000))
            .await
            .expect("missing run response");
        assert_error_code(missing, v1::ErrorCode::NotFound);

        let over_limit = daemon
            .handle(run_event_poll("missing-run", 0, 101, 0, 5_000))
            .await
            .expect("over-limit response");
        assert_error_code(over_limit, v1::ErrorCode::InvalidArgument);

        let over_wait = daemon
            .handle(run_event_poll("missing-run", 0, 1, 20_001, 30_000))
            .await
            .expect("over-wait response");
        assert_error_code(over_wait, v1::ErrorCode::InvalidArgument);

        let over_cursor = daemon
            .handle(run_event_poll(
                "missing-run",
                (i64::MAX as u64) + 1,
                1,
                0,
                5_000,
            ))
            .await
            .expect("over-cursor response");
        assert_error_code(over_cursor, v1::ErrorCode::InvalidArgument);

        let deadline_overlap = daemon
            .handle(run_event_poll("missing-run", 0, 1, 1_000, 1_010))
            .await
            .expect("deadline overlap response");
        assert_error_code(deadline_overlap, v1::ErrorCode::InvalidArgument);
    }

    fn project_id(envelope: v1::Envelope) -> String {
        let envelope::Payload::Response(response) = envelope.payload.expect("payload") else {
            panic!("response")
        };
        let Some(response::Result::Project(project)) = response.result else {
            panic!("project")
        };
        project.id
    }

    fn response_result(envelope: v1::Envelope) -> response::Result {
        let envelope::Payload::Response(response) = envelope.payload.expect("payload") else {
            panic!("response")
        };
        response.result.expect("result")
    }

    async fn seed_approval(daemon: &Daemon, suffix: &str) -> grok_domain::Approval {
        let created = daemon
            .runs
            .create(
                CreateRun {
                    project_id: format!("project-{suffix}"),
                    thread_id: format!("thread-{suffix}"),
                },
                &format!("seed-run-{suffix}"),
            )
            .await
            .expect("seed run");
        let planning = daemon
            .runs
            .transition(
                &created.id,
                created.revision,
                RunState::Planning,
                &format!("seed-planning-{suffix}"),
            )
            .await
            .expect("seed planning");
        daemon
            .approvals
            .request(
                RequestApproval {
                    run_id: planning.id,
                    expected_run_revision: planning.revision,
                    action: RequestedAction {
                        action: "publish_release".into(),
                        target: format!("Release {suffix}"),
                        data_summary: "Publishes reviewed release material.".into(),
                        risk: ApprovalRisk::High,
                    },
                    scope: ApprovalScope::Once,
                    expires_at: 90,
                },
                &format!("seed-approval-{suffix}"),
            )
            .await
            .expect("seed approval")
    }

    fn request_with_key(operation: request::Operation, key: &str) -> v1::Envelope {
        let mut envelope = request(operation);
        envelope.idempotency_key = key.into();
        envelope
    }

    async fn import_test_artifact(
        daemon: &Daemon,
        project_id: &str,
        display_name: &str,
        key: &str,
    ) -> v1::Artifact {
        let imported = daemon
            .handle(artifact_request_with_key(
                request::Operation::ImportArtifact(v1::ImportArtifactRequest {
                    project_id: project_id.into(),
                    thread_id: None,
                    display_name: display_name.into(),
                    media_type: "text/plain".into(),
                    source_path: test_artifact_source_path(display_name),
                }),
                key,
            ))
            .await
            .expect("test artifact import response");
        let response::Result::ArtifactOperation(imported) = response_result(imported) else {
            panic!("test artifact import operation")
        };
        let Some(v1::artifact_operation_result::Result::ImportedArtifact(imported)) =
            imported.result
        else {
            panic!("test imported artifact")
        };
        imported
    }

    fn removal_operation(artifact: &v1::Artifact) -> request::Operation {
        request::Operation::RemoveArtifact(v1::RemoveArtifactRequest {
            artifact_id: artifact.id.clone(),
            expected_revision: artifact.revision,
            expected_content_version: artifact.content_version.expect("artifact content version"),
        })
    }

    fn test_artifact_source_path(display_name: &str) -> String {
        if cfg!(windows) {
            format!(r"C:\private\native-picker\{display_name}")
        } else {
            format!("/private/native-picker/{display_name}")
        }
    }

    async fn wait_for_removal_recovery(store: &InMemoryExecutionStore) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !store
                .list_incomplete_removals(1)
                .await
                .expect("pending removals")
                .is_empty()
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("daemon-owned removal recovery completed");
    }

    fn artifact_request_with_key(operation: request::Operation, key: &str) -> v1::Envelope {
        let minimum = artifact_operation_minimum_budget(Some(&operation))
            .expect("artifact operation minimum budget");
        artifact_request_with_budget(operation, key, minimum + Duration::from_secs(1))
    }

    fn artifact_request_with_budget(
        operation: request::Operation,
        key: &str,
        remaining: Duration,
    ) -> v1::Envelope {
        let mut envelope = request_with_key(operation, key);
        envelope.deadline_unix_ms = 10_u64
            .checked_add(u64::try_from(remaining.as_millis()).expect("bounded artifact budget"))
            .expect("artifact deadline");
        envelope
    }

    fn approval(envelope: v1::Envelope) -> v1::Approval {
        let envelope::Payload::Response(response) = envelope.payload.expect("payload") else {
            panic!("response")
        };
        let Some(response::Result::Approval(approval)) = response.result else {
            panic!("approval")
        };
        approval
    }

    fn conversation_start(thread_id: &str, content: &str, key: &str) -> v1::Envelope {
        let mut envelope = request(request::Operation::StartConversationTurn(
            v1::StartConversationTurnRequest {
                thread_id: thread_id.into(),
                content: content.into(),
                model_id: None,
                search_enabled: false,
            },
        ));
        envelope.idempotency_key = key.into();
        envelope.deadline_unix_ms = 60_010;
        envelope
    }

    fn conversation_list(thread_id: &str) -> v1::Envelope {
        let mut envelope = request(request::Operation::ListConversationTurns(
            v1::ListConversationTurnsRequest {
                thread_id: thread_id.into(),
                cursor: String::new(),
                limit: 100,
            },
        ));
        envelope.idempotency_key.clear();
        envelope.deadline_unix_ms = 60_010;
        envelope
    }

    async fn quarantine_turn_for_test(daemon: &Daemon, turn_id: &ConversationTurnId) {
        let permit = daemon
            .conversation_tasks
            .try_acquire()
            .expect("quarantine task slot");
        let registration = daemon
            .conversation_tasks
            .register(turn_id.clone())
            .await
            .expect("quarantine ownership");
        daemon
            .conversation_tasks
            .quarantine(turn_id, registration.generation, permit)
            .await;
        drop(registration.cancel);
        assert!(daemon.conversation_tasks.is_quarantined(turn_id).await);
        assert_eq!(
            daemon.conversation_tasks.available_slots(),
            MAX_CONVERSATION_TASKS - 1
        );
    }

    async fn wait_for_all_conversation_slots(daemon: &Daemon) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while daemon.conversation_tasks.available_slots() != MAX_CONVERSATION_TASKS {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("conversation task capacity release");
    }

    fn test_dispatch_exit_idempotency_key(turn_id: &ConversationTurnId) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let digest = Sha256::digest(turn_id.as_str().as_bytes());
        let mut key = String::from("daemon-dispatch-exit-");
        for byte in digest {
            key.push(char::from(HEX[usize::from(byte >> 4)]));
            key.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        key
    }

    fn conversation_cancel(turn_id: &str, expected_revision: u64, key: &str) -> v1::Envelope {
        let mut envelope = request(request::Operation::CancelConversationTurn(
            v1::CancelConversationTurnRequest {
                turn_id: turn_id.into(),
                expected_revision,
            },
        ));
        envelope.idempotency_key = key.into();
        envelope.deadline_unix_ms = 60_010;
        envelope
    }

    fn conversation_retry(source_turn_id: &str, expected_revision: u64, key: &str) -> v1::Envelope {
        let mut envelope = request(request::Operation::RetryConversationTurn(
            v1::RetryConversationTurnRequest {
                source_turn_id: source_turn_id.into(),
                expected_revision,
            },
        ));
        envelope.idempotency_key = key.into();
        envelope.deadline_unix_ms = 60_010;
        envelope
    }

    fn conversation_branch(
        source_turn_id: &str,
        expected_revision: u64,
        key: &str,
    ) -> v1::Envelope {
        let mut envelope = request(request::Operation::BranchConversationThread(
            v1::BranchConversationThreadRequest {
                source_turn_id: source_turn_id.into(),
                expected_revision,
            },
        ));
        envelope.idempotency_key = key.into();
        envelope.deadline_unix_ms = 60_010;
        envelope
    }

    fn conversation_edit_and_branch(
        source_turn_id: &str,
        expected_revision: u64,
        content: &str,
        key: &str,
    ) -> v1::Envelope {
        let mut envelope = request(request::Operation::EditAndBranchConversationTurn(
            v1::EditAndBranchConversationTurnRequest {
                source_turn_id: source_turn_id.into(),
                expected_revision,
                content: content.into(),
            },
        ));
        envelope.idempotency_key = key.into();
        envelope.deadline_unix_ms = 60_010;
        envelope
    }

    fn conversation_regenerate(
        source_turn_id: &str,
        expected_revision: u64,
        key: &str,
    ) -> v1::Envelope {
        let mut envelope = request(request::Operation::RegenerateConversationTurn(
            v1::RegenerateConversationTurnRequest {
                source_turn_id: source_turn_id.into(),
                expected_revision,
            },
        ));
        envelope.idempotency_key = key.into();
        envelope.deadline_unix_ms = 60_010;
        envelope
    }

    fn conversation_get_fork_metadata(thread_id: &str) -> v1::Envelope {
        let mut envelope = request(request::Operation::GetConversationForkMetadata(
            v1::GetConversationForkMetadataRequest {
                thread_id: thread_id.into(),
            },
        ));
        envelope.idempotency_key.clear();
        envelope.deadline_unix_ms = 60_010;
        envelope
    }

    fn conversation_ack_fork_delivery(
        child_thread_id: &str,
        expected_revision: u64,
        key: &str,
    ) -> v1::Envelope {
        let mut envelope = request(request::Operation::AcknowledgeConversationForkDelivery(
            v1::AcknowledgeConversationForkDeliveryRequest {
                child_thread_id: child_thread_id.into(),
                expected_revision,
            },
        ));
        envelope.idempotency_key = key.into();
        envelope.deadline_unix_ms = 60_010;
        envelope
    }

    fn conversation_event_poll(
        turn_id: &str,
        after_sequence: u64,
        limit: u32,
        wait_timeout_ms: u32,
    ) -> v1::Envelope {
        let mut envelope = request(request::Operation::PollConversationTurnEvents(
            v1::PollConversationTurnEventsRequest {
                turn_id: turn_id.into(),
                after_sequence,
                limit,
                wait_timeout_ms,
            },
        ));
        envelope.idempotency_key.clear();
        envelope.deadline_unix_ms = 60_010;
        envelope
    }

    fn conversation_turn(envelope: v1::Envelope) -> v1::ConversationTurnResult {
        let envelope::Payload::Response(response) = envelope.payload.expect("payload") else {
            panic!("response")
        };
        let Some(response::Result::ConversationTurn(turn)) = response.result else {
            panic!("conversation turn")
        };
        turn
    }

    fn conversation_fork(envelope: v1::Envelope) -> v1::ConversationForkResult {
        let envelope::Payload::Response(response) = envelope.payload.expect("payload") else {
            panic!("response")
        };
        let Some(response::Result::ConversationFork(fork)) = response.result else {
            panic!("conversation fork")
        };
        fork
    }

    fn conversation_fork_metadata(envelope: v1::Envelope) -> v1::ConversationForkMetadata {
        let envelope::Payload::Response(response) = envelope.payload.expect("payload") else {
            panic!("response")
        };
        let Some(response::Result::ConversationForkMetadata(metadata)) = response.result else {
            panic!("conversation fork metadata")
        };
        metadata
    }

    fn conversation_fork_delivery(envelope: v1::Envelope) -> v1::ConversationForkDelivery {
        let envelope::Payload::Response(response) = envelope.payload.expect("payload") else {
            panic!("response")
        };
        let Some(response::Result::ConversationForkDelivery(delivery)) = response.result else {
            panic!("conversation fork delivery")
        };
        delivery
    }

    fn assert_forked_thread_origin(
        thread: &v1::Thread,
        source: &ConversationTurnSnapshot,
        source_message_id: &str,
        expected_kind: v1::ConversationForkKind,
    ) {
        assert_ne!(thread.id, source.turn.thread_id.as_str());
        assert_eq!(thread.project_id, source.turn.project_id.as_str());
        let lineage = thread.lineage.as_ref().expect("child thread lineage");
        assert_eq!(lineage.root_thread_id, source.turn.thread_id.as_str());
        assert_eq!(lineage.fork_depth, 1);
        let Some(v1::conversation_thread_lineage::Origin::Fork(origin)) = lineage.origin.as_ref()
        else {
            panic!("forked thread origin");
        };
        assert_eq!(origin.parent_thread_id, source.turn.thread_id.as_str());
        assert_eq!(origin.source_turn_id, source.turn.id.as_str());
        assert_eq!(origin.source_message_id, source_message_id);
        assert_eq!(origin.kind, expected_kind as i32);
    }

    fn assert_exact_fork_request(
        request: &ConversationRequest,
        source: &ConversationTurnSnapshot,
        expected_prompt: &str,
    ) {
        assert_eq!(request.model, source.turn.model_id);
        assert_eq!(request.continuation, None);
        assert!(request.tools.is_empty());
        assert!(!request.store);
        let [system, message] = request.messages.as_slice() else {
            panic!("product system context and one canonical fork prompt");
        };
        assert_eq!(system.role, ConversationRole::System);
        assert_eq!(
            system.content,
            vec![ContentPart::Text(PRODUCT_CHAT_SYSTEM_PROMPT_V2.into())]
        );
        assert_eq!(message.role, ConversationRole::User);
        assert_eq!(
            message.content,
            vec![ContentPart::Text(expected_prompt.into())]
        );
    }

    fn conversation_event_batch(envelope: v1::Envelope) -> v1::ConversationTurnEventBatch {
        let envelope::Payload::Response(response) = envelope.payload.expect("payload") else {
            panic!("response")
        };
        let Some(response::Result::ConversationTurnEventBatch(batch)) = response.result else {
            panic!("conversation event batch")
        };
        batch
    }

    async fn wait_for_conversation_state_event(
        daemon: &Daemon,
        turn_id: &str,
        after_sequence: u64,
        state: v1::ConversationTurnState,
    ) -> v1::ConversationTurnEventBatch {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let batch = conversation_event_batch(
                    daemon
                        .handle(conversation_event_poll(turn_id, after_sequence, 100, 0))
                        .await
                        .expect("conversation event poll"),
                );
                if batch.events.iter().any(|event| {
                    event.kind == v1::ConversationTurnEventKind::StateChanged as i32
                        && event.to_state == state as i32
                }) {
                    return batch;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("conversation state event")
    }

    async fn wait_for_conversation_capacity(daemon: &Daemon) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while daemon.conversation_tasks.available_slots() != MAX_CONVERSATION_TASKS {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("provider task capacity released");
    }

    fn run_event_poll(
        run_id: &str,
        after_sequence: u64,
        limit: u32,
        wait_timeout_ms: u32,
        deadline_unix_ms: u64,
    ) -> v1::Envelope {
        let mut envelope = request(request::Operation::PollRunEvents(
            v1::PollRunEventsRequest {
                run_id: run_id.into(),
                after_sequence,
                limit,
                wait_timeout_ms,
            },
        ));
        envelope.idempotency_key.clear();
        envelope.deadline_unix_ms = deadline_unix_ms;
        envelope
    }

    fn run_event_batch(envelope: v1::Envelope) -> v1::RunEventBatch {
        let envelope::Payload::Response(response) = envelope.payload.expect("payload") else {
            panic!("response")
        };
        let Some(response::Result::RunEventBatch(batch)) = response.result else {
            panic!("run event batch")
        };
        batch
    }

    fn assert_error_code(envelope: v1::Envelope, expected: v1::ErrorCode) {
        let envelope::Payload::Response(response) = envelope.payload.expect("payload") else {
            panic!("response")
        };
        assert!(matches!(
            response.result,
            Some(response::Result::Error(v1::ErrorResponse { code, .. }))
                if code == expected as i32
        ));
    }

    fn wire_message(envelope: v1::Envelope) -> v1::Message {
        let envelope::Payload::Response(response) = envelope.payload.expect("payload") else {
            panic!("response")
        };
        let Some(response::Result::Message(message)) = response.result else {
            panic!("message")
        };
        message
    }

    fn wire_messages(envelope: v1::Envelope) -> v1::MessageList {
        let envelope::Payload::Response(response) = envelope.payload.expect("payload") else {
            panic!("response")
        };
        let Some(response::Result::Messages(messages)) = response.result else {
            panic!("messages")
        };
        messages
    }

    fn account_state(envelope: v1::Envelope) -> v1::AccountState {
        let envelope::Payload::Response(response) = envelope.payload.expect("payload") else {
            panic!("response")
        };
        let Some(response::Result::AccountState(state)) = response.result else {
            panic!("account state")
        };
        state
    }

    fn desktop_preferences(envelope: v1::Envelope) -> v1::DesktopPreferences {
        let envelope::Payload::Response(response) = envelope.payload.expect("payload") else {
            panic!("response")
        };
        let Some(response::Result::DesktopPreferences(preferences)) = response.result else {
            panic!("desktop preferences")
        };
        preferences
    }

    fn capability_availability(envelope: &v1::Envelope, capability: v1::Capability) -> i32 {
        let Some(envelope::Payload::Response(response)) = envelope.payload.as_ref() else {
            panic!("response")
        };
        let Some(response::Result::Capabilities(capabilities)) = response.result.as_ref() else {
            panic!("capabilities")
        };
        capabilities
            .statuses
            .iter()
            .find(|status| status.capability == capability as i32)
            .expect("capability status")
            .availability
    }

    #[test]
    fn internal_diagnostics_never_cross_the_renderer_boundary() {
        for (input, expected) in [
            (
                ApplicationError::Storage("/home/user/private/state.db: malformed page".into()),
                "internal storage operation failed",
            ),
            (
                ApplicationError::Unavailable("Authorization: Bearer secret".into()),
                "required local dependency is unavailable",
            ),
        ] {
            let result = error_result(&input);
            let v1::response::Result::Error(error) = result else {
                panic!("error response")
            };
            assert!(!error.message.contains("/home/user"));
            assert!(!error.message.contains("Bearer"));
            assert_eq!(error.message, expected);
        }
    }

    #[tokio::test]
    async fn usage_summary_rejects_invalid_scope_and_window_before_store_access() {
        let (daemon, _) = daemon();
        for (scope_kind, scope_id, window) in [
            ("weekly", "", "last_7_days"),
            ("workspace", "extra-id", "last_7_days"),
            ("project", "", "last_7_days"),
            ("thread", "thread-1", "monthly"),
        ] {
            let response = daemon
                .handle(request(request::Operation::GetUsageSummary(
                    v1::GetUsageSummaryRequest {
                        scope_kind: scope_kind.into(),
                        scope_id: scope_id.into(),
                        window: window.into(),
                    },
                )))
                .await
                .expect("invalid usage summary is still a framed response");
            let envelope::Payload::Response(response) = response.payload.expect("payload") else {
                panic!("response")
            };
            assert!(
                matches!(
                    response.result,
                    Some(response::Result::Error(v1::ErrorResponse { code, .. }))
                        if code == v1::ErrorCode::InvalidArgument as i32
                ),
                "expected invalid argument for {scope_kind}/{window}"
            );
        }
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn usage_summary_returns_workspace_aggregate_over_completed_turns() {
        let store = Arc::new(InMemoryExecutionStore::new());
        let vault = Arc::new(InMemorySecretVault::new());
        seed_test_xai_credential(&vault);
        let clock = Arc::new(FixedClock::new(10));
        let ids: Arc<dyn IdGenerator> = Arc::new(SequentialIdGenerator::new());
        let runs = Arc::new(RunService::new(store.clone(), clock.clone(), ids.clone()));
        let approvals = Arc::new(ApprovalService::new(
            store.clone(),
            clock.clone(),
            ids.clone(),
        ));
        let workspace = Arc::new(WorkspaceService::new(
            store.clone(),
            clock.clone(),
            ids.clone(),
        ));
        let credentials = Arc::new(CredentialService::new(
            vault,
            store.clone(),
            Arc::new(AcceptXaiKey),
        ));
        let factory: Arc<dyn ConversationModelFactory> = Arc::new(PendingConversationFactory(
            Arc::new(PendingConversationModel {
                stream_calls: Arc::new(AtomicUsize::new(0)),
            }),
        ));
        let conversation = Arc::new(ConversationService::new(
            store.clone(),
            workspace.clone(),
            credentials.clone(),
            factory,
            clock.clone(),
            ids,
            store.clone(),
        ));
        let project = workspace
            .create_project(
                CreateProject {
                    name: "Usage summary".into(),
                    description: String::new(),
                },
                "usage-summary-project",
            )
            .await
            .expect("project");
        let thread = workspace
            .create_thread(
                CreateThread {
                    project_id: project.id.to_string(),
                    title: "Usage summary".into(),
                },
                "usage-summary-thread",
            )
            .await
            .expect("thread");
        seed_completed_usage_turn(
            store.as_ref(),
            project.id.clone(),
            thread.id.clone(),
            ConversationUsage {
                input_tokens: 11,
                output_tokens: 5,
                cost_in_usd_ticks: 17,
            },
        )
        .await;

        let daemon = Daemon::new(
            runs,
            approvals,
            credentials,
            clock,
            [7; 32],
            "instance-usage".into(),
        )
        .with_workspace(workspace)
        .with_conversation(conversation);

        let workspace_summary = daemon
            .handle(request(request::Operation::GetUsageSummary(
                v1::GetUsageSummaryRequest {
                    scope_kind: "workspace".into(),
                    scope_id: String::new(),
                    window: "all_time".into(),
                },
            )))
            .await
            .expect("workspace usage summary");
        let envelope::Payload::Response(response) = workspace_summary.payload.expect("payload")
        else {
            panic!("response")
        };
        let Some(response::Result::UsageSummary(summary)) = response.result else {
            panic!("usage summary result: {response:?}")
        };
        assert_eq!(summary.scope_kind, "workspace");
        assert!(summary.scope_id.is_empty());
        assert_eq!(summary.window, "all_time");
        assert_eq!(summary.input_tokens, 11);
        assert_eq!(summary.output_tokens, 5);
        assert_eq!(summary.cost_in_usd_ticks, 17);
        assert_eq!(summary.turn_count, 1);
        assert_eq!(summary.as_of_unix_ms, 10);

        let project_summary = daemon
            .handle(request(request::Operation::GetUsageSummary(
                v1::GetUsageSummaryRequest {
                    scope_kind: "project".into(),
                    scope_id: project.id.to_string(),
                    window: "last_30_days".into(),
                },
            )))
            .await
            .expect("project usage summary");
        let envelope::Payload::Response(response) = project_summary.payload.expect("payload")
        else {
            panic!("response")
        };
        let Some(response::Result::UsageSummary(summary)) = response.result else {
            panic!("project usage summary")
        };
        assert_eq!(summary.scope_kind, "project");
        assert_eq!(summary.scope_id, project.id.to_string());
        assert_eq!(summary.input_tokens, 11);
        assert_eq!(summary.turn_count, 1);

        let thread_summary = daemon
            .handle(request(request::Operation::GetUsageSummary(
                v1::GetUsageSummaryRequest {
                    scope_kind: "thread".into(),
                    scope_id: thread.id.to_string(),
                    window: "last_7_days".into(),
                },
            )))
            .await
            .expect("thread usage summary");
        let envelope::Payload::Response(response) = thread_summary.payload.expect("payload") else {
            panic!("response")
        };
        let Some(response::Result::UsageSummary(summary)) = response.result else {
            panic!("thread usage summary")
        };
        assert_eq!(summary.scope_kind, "thread");
        assert_eq!(summary.scope_id, thread.id.to_string());
        assert_eq!(summary.output_tokens, 5);
    }

    #[allow(clippy::too_many_lines)]
    async fn seed_completed_usage_turn(
        store: &InMemoryExecutionStore,
        project_id: ProjectId,
        thread_id: ThreadId,
        usage: ConversationUsage,
    ) {
        let now = 5_u64;
        let user = Message::new(
            MessageId::new("usage-user-1").expect("message id"),
            thread_id.clone(),
            MessageRole::User,
            "Prompt".into(),
            now,
        )
        .expect("user");
        let run = Run::queued(
            RunId::new("usage-run-1").expect("run id"),
            project_id.clone(),
            thread_id.clone(),
            now,
        );
        let turn = ConversationTurn::reserve(
            ConversationTurnId::new("usage-turn-1").expect("turn id"),
            "usage-command-1".into(),
            [9; 32],
            project_id,
            thread_id,
            user.id.clone(),
            run.id.clone(),
            "grok-4.3".into(),
            false,
            now,
        )
        .expect("turn");
        let reserved = store
            .reserve_turn(
                turn,
                ConversationTurnLineage::original("xai-binding-usage".into()).expect("lineage"),
                ConversationTurnReservationSource::CurrentThread,
                user,
                run,
                NewRunEvent {
                    occurred_at: now,
                    kind: RunEventKind::Created,
                },
                ConversationTurnEventKind::Created,
            )
            .await
            .expect("reserve");
        let mut started_turn = reserved.snapshot.turn.clone();
        let mut started_run = reserved.snapshot.run.clone();
        let mut effect = SideEffect::prepare(
            EffectId::new("usage-effect-1").expect("effect id"),
            started_run.id.clone(),
            EffectKind::ExternalMutation,
            "official xAI Responses API model grok-4.3".into(),
            Idempotency::NonIdempotent,
            now + 1,
        );
        effect.start(now + 1).expect("start effect");
        started_turn
            .start_provider(effect.id.clone(), [10; 32], now + 1)
            .expect("start provider");
        started_run
            .transition(RunState::Planning, now + 1)
            .expect("planning");
        started_run
            .transition(RunState::Running, now + 1)
            .expect("running");
        let started = store
            .commit_provider_start(ProviderStartCommit {
                turn: started_turn,
                expected_turn_revision: reserved.snapshot.turn.revision,
                run: started_run,
                expected_run_revision: reserved.snapshot.run.revision,
                effect: effect.clone(),
                events: vec![
                    NewRunEvent {
                        occurred_at: now + 1,
                        kind: RunEventKind::StateChanged {
                            from: RunState::Queued,
                            to: RunState::Planning,
                        },
                    },
                    NewRunEvent {
                        occurred_at: now + 1,
                        kind: RunEventKind::StateChanged {
                            from: RunState::Planning,
                            to: RunState::Running,
                        },
                    },
                    NewRunEvent {
                        occurred_at: now + 1,
                        kind: RunEventKind::EffectPrepared {
                            effect_id: effect.id.clone(),
                        },
                    },
                ],
                turn_event: ConversationTurnEventKind::StateChanged {
                    from: ConversationTurnState::Reserved,
                    to: ConversationTurnState::ProviderStarted,
                },
            })
            .await
            .expect("provider start");
        store
            .append_turn_text(&started.turn.id, started.turn.revision, 0, "Answer".into())
            .await
            .expect("append text");
        let mut turn = started.turn.clone();
        let mut run = started.run.clone();
        let mut effect = started.effect.clone().expect("effect");
        let assistant = Message::new(
            MessageId::new("usage-assistant-1").expect("assistant id"),
            turn.thread_id.clone(),
            MessageRole::Assistant,
            "Answer".into(),
            now + 2,
        )
        .expect("assistant");
        turn.complete(
            assistant.id.clone(),
            Some("response-1".into()),
            Vec::new(),
            usage,
            Some(true),
            now + 2,
        )
        .expect("complete");
        effect.finish(true, now + 2).expect("finish effect");
        run.transition(RunState::Completed, now + 2)
            .expect("complete run");
        store
            .commit_terminal(TerminalTurnCommit {
                turn,
                expected_turn_revision: started.turn.revision,
                run,
                expected_run_revision: started.run.revision,
                expected_effect_revision: started.effect.as_ref().map(|value| value.revision),
                effect: Some(effect),
                assistant_message: Some(assistant),
                events: vec![NewRunEvent {
                    occurred_at: now + 2,
                    kind: RunEventKind::StateChanged {
                        from: RunState::Running,
                        to: RunState::Completed,
                    },
                }],
                turn_event: ConversationTurnEventKind::StateChanged {
                    from: ConversationTurnState::ProviderStarted,
                    to: ConversationTurnState::Completed,
                },
            })
            .await
            .expect("terminal");
    }
}
