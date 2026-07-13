//! Grok Desktop trusted per-user daemon entry point.

#[cfg(target_os = "linux")]
mod linux_guest_transport;
#[cfg(target_os = "linux")]
mod linux_isolation_probe;

use std::{
    collections::HashSet,
    error::Error,
    ffi::OsStr,
    fs::File,
    io::{ErrorKind, Read},
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[cfg(all(debug_assertions, feature = "debug-acp-descriptor"))]
use grok_acp::ExternalGrokComponent;
use grok_acp::{
    GrokHomeSpec, MAX_SIGNED_CATALOG_ENVELOPE_BYTES, OfficialGrokCatalogVerifier,
    TrustedCatalogKey, VerifiedGrokComponent,
};
use grok_application::{
    AgentRuntimeErrorKind, ApplicationError, ApprovalService, ArtifactContentRetention,
    ArtifactContentStore, ArtifactOpener, ArtifactService, ArtifactStore,
    AutomationSchedulerService, AutomationSchedulerStore, CapabilityFacts,
    ChatModelPreferenceStore, ChatModelService, ChatRailSelection, Clock, ConversationModelFactory,
    ConversationService, ConversationTurnStore, CredentialEnrollmentService,
    CredentialMutationStore, CredentialService, DesktopPreferencesService, DesktopPreferencesStore,
    ExecutionStore, HostExecutionPolicyStore, IdGenerator, IsolationProbe, IsolationProbeError,
    IsolationRuntime, MAX_ARTIFACT_RECOVERY_BATCH, MAX_AUTOMATION_SCHEDULER_RECOVERY_BATCH,
    MAX_CONVERSATION_RECOVERY_BATCH, MAX_PRIVILEGED_RECOVERY_BATCH,
    ManagedIntegrationLifecycleStore, PrivilegedGatewayError, PrivilegedGuestControlTransport,
    PrivilegedOperationService, PrivilegedOperationStore, RunService, ScheduledGuestDispatcher,
    SecretName, SecretValue, SecretVault, SideEffectService, SuperGrokEnrollmentService,
    VaultError, WorkspaceService, WorkspaceStore,
};
#[cfg(target_os = "linux")]
use grok_artifact_storage::LinuxArtifactContent;
use grok_artifact_storage::UnavailableArtifactContent;
use grok_credential_enrollment::NativeCredentialEnrollment;
use grok_daemon::{
    AgentRuntimeUnavailableReason, AutomationSchedulerLifecycle, Daemon, GrokAcpRoleFactory,
    HostWorkRuntime, HostWorkService, VerifiedHostToolsHelper, serve_connection,
};
use grok_domain::{AutomationSchedulerOwnerId, ChatRail, EffectState, RunState};
use grok_memory::{
    EphemeralKeyProvider, InMemoryExecutionStore, InMemoryManagedIntegrationLifecycleStore,
    InMemorySecretVault, SystemClock, UuidGenerator,
};
use grok_sqlcipher::{DatabaseLock, SqlCipherStore};
use grok_vault::OsVault;
use grok_vm_service_client::VmServiceIsolationProbe;
use grok_xai::oauth::XaiOAuthClient;
use grok_xai::{
    OfficialXaiApiKeyValidator, OfficialXaiConversationModelFactory,
    OfficialXaiOAuthConversationModelFactory,
};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;
use zeroize::Zeroize;

type DynError = Box<dyn Error + Send + Sync>;

const ACP_BUILD_KEYS: Option<&str> = option_env!("GROK_ACP_CATALOG_TRUSTED_KEYS");
const ACP_BUILD_TRUST_BINDING: Option<&str> = option_env!("GROK_ACP_CATALOG_TRUST_BINDING");
const ACP_BUILD_TRUST_BINDING_PREFIX: &str = "grok-acp-catalog-trust-v1:";
const WISP_BUILD_KEYS: Option<&str> = option_env!("GROK_WISP_CATALOG_TRUSTED_KEYS");
const WISP_BUILD_TRUST_BINDING: Option<&str> = option_env!("GROK_WISP_CATALOG_TRUST_BINDING");
const WISP_BUILD_TRUST_BINDING_PREFIX: &str = "grok-wisp-catalog-trust-v1:";
const ACP_COMPONENT_DIRECTORY: &str = "components";
const ACP_COMPONENT_NAME: &str = "grok-acp";
const ACP_CATALOG_FILE: &str = "catalog.json";
const ACP_CATALOG_WATERMARK_NAME: &str = "grok-acp.catalog-sequence.v1";
const ACP_CATALOG_WATERMARK_MAGIC: &[u8; 8] = b"GRKACP01";
const MAX_HOST_WORK_RECOVERY_BATCH: usize = 100;
const MAX_BUILD_KEY_INPUT_BYTES: usize = 4096;
const MAX_BUILD_KEYS: usize = 16;
const MAX_CONCURRENT_IPC_CONNECTIONS: usize = 64;
const AUTOMATION_SCHEDULER_LOOP_INTERVAL: Duration = Duration::from_secs(5);
const AUTOMATION_SCHEDULER_SHUTDOWN_GRACE: Duration = Duration::from_secs(65);
const LEGACY_STARTUP_NONCE_VARIABLE: &str = "GROK_DAEMON_STARTUP_NONCE_HEX";
const STARTUP_NONCE_STDIN_MARKER: &str = "GROK_DAEMON_STARTUP_NONCE_STDIN";
const LEGACY_ACP_VARIABLES: [&str; 4] = [
    "GROK_ACP_EXECUTABLE",
    "GROK_ACP_VERSION",
    "GROK_ACP_SHA256",
    "GROK_ACP_WORKSPACE_ROOTS",
];

/// Ties this daemon's lifetime to the supervising desktop process.
///
/// The supervisor stops the daemon on every graceful shutdown path, but a
/// crashed or force-killed desktop process cannot. An orphaned daemon keeps
/// the execution-store lock and turns every subsequent launch into an
/// immediate `DatabaseInUse` failure, so ask the kernel for SIGTERM on
/// parent death instead. Windows ties the lifetime through service/job
/// supervision and does not need this.
#[cfg(target_os = "linux")]
fn bind_lifetime_to_parent() {
    use rustix::process::{Signal, getppid, set_parent_process_death_signal};

    let _ = set_parent_process_death_signal(Some(Signal::TERM));
    // The parent may have exited before the registration took effect; a
    // reparented daemon reports init as its parent.
    if getppid().is_none_or(|parent| parent.as_raw_nonzero().get() == 1) {
        std::process::exit(0);
    }
}

/// Removes same-user process inspection and core-dump access before secrets,
/// environment configuration, the async runtime, or worker threads exist.
#[cfg(target_os = "linux")]
fn harden_process_inspection() -> Result<(), DynError> {
    use rustix::process::{DumpableBehavior, dumpable_behavior, set_dumpable_behavior};

    set_dumpable_behavior(DumpableBehavior::NotDumpable)?;
    if dumpable_behavior()? != DumpableBehavior::NotDumpable {
        return Err("the daemon process remained dumpable".into());
    }
    Ok(())
}

fn main() -> Result<(), DynError> {
    #[cfg(target_os = "linux")]
    harden_process_inspection()?;
    #[cfg(target_os = "linux")]
    bind_lifetime_to_parent();

    let startup_nonce = startup_nonce()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run(startup_nonce))
}

fn initialize_tracing() -> Result<(), DynError> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .try_init()?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn run(startup_nonce: StartupNonce) -> Result<(), DynError> {
    initialize_tracing()?;
    if startup_nonce.standalone {
        warn!("startup nonce was not supplied by a parent process; using a standalone nonce");
    }

    let stores = stores().await?;
    let runtime_vault = stores.vault.clone();
    let clock = Arc::new(SystemClock);
    let ids: Arc<dyn IdGenerator> = Arc::new(UuidGenerator);
    let (instance_id, automation_scheduler_owner) = new_daemon_instance_identity()?;
    let automation_scheduler = Arc::new(AutomationSchedulerService::new(
        stores.scheduler.clone(),
        clock.clone(),
        ids.clone(),
    ));
    let mut automation_scheduler_lifecycle =
        recover_automation_scheduler(automation_scheduler.as_ref(), &automation_scheduler_owner)
            .await;
    recover_privileged_operations(&stores, clock.clone(), ids.clone()).await?;
    let runs = Arc::new(RunService::new(
        stores.execution.clone(),
        clock.clone(),
        ids.clone(),
    ));
    let approvals = Arc::new(ApprovalService::new(
        stores.execution.clone(),
        clock.clone(),
        ids.clone(),
    ));
    let effects = Arc::new(SideEffectService::new(
        stores.execution.clone(),
        clock.clone(),
        ids.clone(),
    ));
    recover_host_work(stores.execution.as_ref(), effects.as_ref(), runs.as_ref()).await?;
    let workspace = Arc::new(WorkspaceService::new(
        stores.workspace.clone(),
        clock.clone(),
        ids.clone(),
    ));
    let (artifacts, artifact_content_available, artifact_open_available) =
        configured_artifact_service(&stores, clock.clone(), ids.clone()).await?;
    let desktop_preferences = Arc::new(DesktopPreferencesService::new(
        stores.desktop_preferences.clone(),
        clock.clone(),
    ));
    let credential_mutations = stores.credential_mutations.clone();
    let credentials = Arc::new(CredentialService::new(
        stores.vault.clone(),
        credential_mutations.clone(),
        Arc::new(OfficialXaiApiKeyValidator::new()),
    ));
    let isolation_probe = configured_isolation_probe();
    let (xai_probe, isolation_probe_result) = tokio::join!(
        credentials.refresh_xai_capabilities(),
        isolation_probe.probe()
    );
    if let Err(error) = xai_probe {
        warn!(%error, "xAI capabilities remain unresolved after startup probe");
    }
    let isolation_broker_qualified = match isolation_probe_result {
        Ok(_) => {
            info!("packaged isolation broker passed static qualification");
            true
        }
        Err(error) => {
            warn!(
                reason_code = isolation_probe_reason(error),
                "isolation broker remains statically unqualified"
            );
            false
        }
    };
    let supergrok_oauth = Arc::new(XaiOAuthClient::new()?);
    let supergrok_enrollment = Arc::new(SuperGrokEnrollmentService::new(
        supergrok_oauth,
        stores.vault.clone(),
    )?);
    let default_chat_rail = if supergrok_enrollment.connection_status()?.is_some() {
        ChatRail::SuperGrokApi
    } else {
        ChatRail::XaiApiKey
    };
    let chat_rail = Arc::new(ChatRailSelection::new(default_chat_rail));
    let (chat_models, conversation) = configured_chat_services(
        &stores,
        workspace.clone(),
        credentials.clone(),
        supergrok_enrollment.clone(),
        chat_rail.clone(),
        clock.clone(),
        ids.clone(),
    );
    recover_conversation_turns(&conversation).await?;
    let mut runtime_capability_facts = provider_network_policy(isolation_broker_qualified);
    runtime_capability_facts.artifact_content_ready =
        artifact_content_available && artifact_open_available;
    let isolation_runtime = configured_isolation_runtime(
        isolation_probe,
        stores.privileged_operations.clone(),
        clock.clone(),
        ids.clone(),
    );
    let scheduler_runtime = if let Some(dispatcher) = configured_scheduled_guest_dispatcher() {
        if automation_scheduler_lifecycle
            == AutomationSchedulerLifecycle::KernelInitializedExecutionDisabled
        {
            let facts = isolation_runtime
                .refresh("automation-isolation-startup")
                .await
                .unwrap_or_default();
            if scheduler_runtime_eligible(automation_scheduler_lifecycle, true, facts) {
                automation_scheduler_lifecycle =
                    AutomationSchedulerLifecycle::KernelInitializedExecutionEnabled;
                Some(start_automation_scheduler(
                    automation_scheduler.clone(),
                    automation_scheduler_owner,
                    isolation_runtime.clone(),
                    dispatcher,
                ))
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };
    let mut daemon = Daemon::new(
        runs.clone(),
        approvals.clone(),
        credentials.clone(),
        clock.clone(),
        startup_nonce.value,
        instance_id,
    )
    .with_workspace(workspace.clone())
    .with_automation_scheduler(automation_scheduler, automation_scheduler_lifecycle)
    .with_artifacts(
        artifacts,
        artifact_content_available,
        artifact_open_available,
    )
    .with_desktop_preferences(desktop_preferences)
    .with_host_execution_policy(stores.host_execution_policy.clone())
    .with_chat_models(chat_models)
    .with_conversation(conversation)
    .with_runtime_capability_facts(runtime_capability_facts);
    daemon = daemon.with_supergrok_enrollment(supergrok_enrollment, chat_rail);
    daemon = daemon.with_credential_enrollment(configured_credential_enrollment(
        credentials,
        credential_mutations,
    ));
    // Journaled guest health gateway: strong isolation is ready only after a
    // successful runner.health through PrivilegedGateway (never host-exec).
    daemon = daemon.with_isolation_runtime(isolation_runtime);
    if let Some(managed) = configured_managed_integrations(&stores).await? {
        daemon = daemon.with_managed_integrations(managed);
    }
    let daemon = Arc::new(match configured_agent_runtime(runtime_vault).await {
        AgentRuntimeConfiguration::NotConfigured => daemon,
        AgentRuntimeConfiguration::Available(runtime) => {
            let service = Arc::new(HostWorkService::new(
                runtime.clone(),
                stores.host_execution_policy.clone(),
                stores.execution.clone(),
                runs,
                workspace,
                approvals,
                effects,
                clock,
                host_tools_endpoint_base()?,
                host_tools_denied_filesystem_roots()?,
            ));
            daemon
                .with_host_work_runtime(runtime)
                .with_host_work_service(service)
        }
        AgentRuntimeConfiguration::Unavailable(reason) => {
            daemon.with_unavailable_agent_runtime(reason)
        }
    });

    let serving = async {
        if let Ok(address) = std::env::var("GROK_DAEMON_DEV_TCP_ADDR") {
            serve_tcp(&address, daemon).await
        } else {
            serve_platform(daemon).await
        }
    };
    let result = tokio::select! {
        result = serving => result,
        result = shutdown_signal() => result,
    };
    if let Some(runtime) = scheduler_runtime
        && let Err(error) = runtime.shutdown().await
    {
        return Err(format!("automation scheduler task failed to join: {error}").into());
    }
    result
}

async fn shutdown_signal() -> Result<(), DynError> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result?,
            _ = terminate.recv() => {}
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await?;
        Ok(())
    }
}

fn configured_isolation_probe() -> Arc<dyn IsolationProbe> {
    #[cfg(target_os = "linux")]
    {
        if let Some(probe) = linux_isolation_probe::LinuxVmServiceIsolationProbe::production() {
            return Arc::new(probe);
        }
        if let Some(probe) = linux_isolation_probe::LinuxVmServiceIsolationProbe::from_env() {
            return Arc::new(probe);
        }
    }
    Arc::new(VmServiceIsolationProbe::new())
}

fn new_daemon_instance_identity()
-> Result<(String, AutomationSchedulerOwnerId), grok_domain::IdError> {
    let instance_id = format!("daemon-{}", uuid::Uuid::new_v4());
    let owner_id = AutomationSchedulerOwnerId::new(instance_id.clone())?;
    Ok((instance_id, owner_id))
}

/// Guest-control transport that never fakes a guest response.
///
/// Lab/production dialers replace this when a live linux-vm-service grant exists.
struct FailClosedGuestHealthTransport;

#[async_trait::async_trait]
impl PrivilegedGuestControlTransport for FailClosedGuestHealthTransport {
    async fn runner_health(&self, _vm_id: &str) -> Result<Vec<u8>, PrivilegedGatewayError> {
        Err(PrivilegedGatewayError::Unavailable(
            "guest runner.health dial is not configured".into(),
        ))
    }
}

/// Optional lab transport: `GROK_ISOLATION_FAKE_GUEST_HEALTH=ok` returns a body
/// only for automated isolation-path tests. Production leaves the env unset.
struct EnvGuestHealthTransport;

#[async_trait::async_trait]
impl PrivilegedGuestControlTransport for EnvGuestHealthTransport {
    async fn runner_health(&self, vm_id: &str) -> Result<Vec<u8>, PrivilegedGatewayError> {
        match std::env::var("GROK_ISOLATION_FAKE_GUEST_HEALTH").as_deref() {
            Ok("ok") => {
                Ok(format!(r#"{{"status":"ok","vm":"{vm_id}","source":"lab"}}"#).into_bytes())
            }
            _ => Err(PrivilegedGatewayError::Unavailable(
                "guest runner.health dial is not configured".into(),
            )),
        }
    }
}

/// Configures the signed Wisp lifecycle in debug builds when
/// `GROK_WISP_BUNDLE_ROOT` points at a catalog release root. Trust keys come
/// from an independent bound build input or debug-only trust configuration,
/// never from that release root.
async fn configured_managed_integrations(
    stores: &Stores,
) -> Result<Option<Arc<grok_daemon::ManagedIntegrationService>>, DynError> {
    use grok_daemon::ManagedIntegrationService;

    let Some(bundle) = std::env::var_os("GROK_WISP_BUNDLE_ROOT").map(PathBuf::from) else {
        return Ok(None);
    };
    if !cfg!(debug_assertions) {
        return Err("GROK_WISP_BUNDLE_ROOT is a debug-only release-root override".into());
    }
    if !grok_daemon::managed_integration_publication_qualified() {
        return Err("managed-integration publication is not qualified on this platform".into());
    }
    let state = std::env::var_os("GROK_MANAGED_INTEGRATION_STATE").map_or_else(
        || {
            stores
                .artifact_content_base
                .clone()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("managed-integrations.state-anchor")
        },
        PathBuf::from,
    );
    let mut service =
        ManagedIntegrationService::with_lifecycle_store(state, stores.managed_integrations.clone());
    let trusted_keys = configured_wisp_catalog_keys()
        .map_err(|()| "Wisp catalog trust configuration is missing or invalid")?;
    for (key_id, public_key) in trusted_keys {
        service.trust_key("grok-insider", key_id, &public_key)?;
    }
    let verified = service.verify_catalog_bound_bundle(&bundle)?;
    service.register_bundle(&verified)?;
    let service = Arc::new(service);
    let recovered = service
        .recover_pending_publications(SystemClock.now())
        .await?;
    if recovered > 0 {
        info!(recovered, "recovered managed-integration publications");
    }
    Ok(Some(service))
}

fn configured_wisp_catalog_keys() -> Result<Vec<(String, [u8; 32])>, ()> {
    match (WISP_BUILD_KEYS, WISP_BUILD_TRUST_BINDING) {
        (Some(value), Some(binding)) if wisp_catalog_trust_binding(value) == binding => {
            parse_wisp_catalog_keys(value)
        }
        (None, None) if cfg!(debug_assertions) => {
            let value = std::env::var("GROK_WISP_CATALOG_TRUSTED_KEYS").map_err(|_| ())?;
            parse_wisp_catalog_keys(&value)
        }
        _ => Err(()),
    }
}

fn wisp_catalog_trust_binding(value: &str) -> String {
    format!(
        "{WISP_BUILD_TRUST_BINDING_PREFIX}{}",
        hex::encode(Sha256::digest(value.as_bytes()))
    )
}

fn parse_wisp_catalog_keys(value: &str) -> Result<Vec<(String, [u8; 32])>, ()> {
    if value.is_empty() || value.len() > MAX_BUILD_KEY_INPUT_BYTES {
        return Err(());
    }
    let mut keys = Vec::new();
    let mut previous = None;
    for record in value.split(';') {
        let (key_id, encoded) = record.split_once('=').ok_or(())?;
        if record.matches('=').count() != 1
            || key_id.is_empty()
            || key_id.len() > 128
            || !key_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
            || encoded.len() != 64
            || !encoded
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            || previous.is_some_and(|prior| key_id <= prior)
        {
            return Err(());
        }
        let public_key: [u8; 32] = hex::decode(encoded)
            .map_err(|_| ())?
            .try_into()
            .map_err(|_| ())?;
        ed25519_dalek::VerifyingKey::from_bytes(&public_key).map_err(|_| ())?;
        keys.push((key_id.to_owned(), public_key));
        if keys.len() > MAX_BUILD_KEYS {
            return Err(());
        }
        previous = Some(key_id);
    }
    if keys.is_empty() { Err(()) } else { Ok(keys) }
}

fn configured_isolation_runtime(
    probe: Arc<dyn IsolationProbe>,
    privileged_operations: Arc<dyn PrivilegedOperationStore>,
    clock: Arc<dyn grok_application::Clock>,
    ids: Arc<dyn IdGenerator>,
) -> Arc<IsolationRuntime> {
    let transport: Arc<dyn PrivilegedGuestControlTransport> = {
        #[cfg(target_os = "linux")]
        {
            if let Some(socket_transport) =
                linux_guest_transport::LinuxVmServiceGuestTransport::from_env()
            {
                Arc::new(socket_transport)
            } else if std::env::var("GROK_ISOLATION_FAKE_GUEST_HEALTH").is_ok() {
                Arc::new(EnvGuestHealthTransport)
            } else {
                Arc::new(FailClosedGuestHealthTransport)
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            if std::env::var("GROK_ISOLATION_FAKE_GUEST_HEALTH").is_ok() {
                Arc::new(EnvGuestHealthTransport)
            } else {
                Arc::new(FailClosedGuestHealthTransport)
            }
        }
    };
    Arc::new(IsolationRuntime::new(
        probe,
        privileged_operations,
        clock,
        ids,
        transport,
        "work-vm",
        "authority-grant-isolation-default",
    ))
}

/// Joined daemon-lifetime scheduler task.
struct AutomationSchedulerRuntime {
    cancellation: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl AutomationSchedulerRuntime {
    async fn shutdown(self) -> Result<(), tokio::task::JoinError> {
        self.cancellation.cancel();
        let mut task = self.task;
        if let Ok(result) =
            tokio::time::timeout(AUTOMATION_SCHEDULER_SHUTDOWN_GRACE, &mut task).await
        {
            result
        } else {
            task.abort();
            match task.await {
                Err(error) if error.is_cancelled() => Ok(()),
                result => result,
            }
        }
    }
}

fn start_automation_scheduler(
    scheduler: Arc<AutomationSchedulerService>,
    owner_id: AutomationSchedulerOwnerId,
    isolation: Arc<IsolationRuntime>,
    dispatcher: Arc<dyn ScheduledGuestDispatcher>,
) -> AutomationSchedulerRuntime {
    let cancellation = CancellationToken::new();
    let task_cancellation = cancellation.clone();
    let task = tokio::spawn(async move {
        let mut cycle = 0_u64;
        loop {
            if task_cancellation.is_cancelled() {
                return;
            }
            cycle = cycle.wrapping_add(1);
            let refresh_key = format!("automation-isolation-{cycle:016x}");
            let isolation_ready = isolation
                .refresh(&refresh_key)
                .await
                .is_ok_and(|facts| facts.broker_qualified && facts.strong_isolation_ready);
            if isolation_ready {
                if let Err(error) = scheduler
                    .dispatch_resumable(
                        dispatcher.as_ref(),
                        &task_cancellation,
                        grok_application::MAX_AUTOMATION_SCHEDULER_TICK_DEFINITIONS,
                    )
                    .await
                {
                    warn!(%error, "automation scheduler resumable dispatch failed closed");
                }
                if !task_cancellation.is_cancelled() {
                    if let Err(error) = scheduler
                        .execute_due(
                            &owner_id,
                            grok_application::MAX_AUTOMATION_SCHEDULER_TICK_DEFINITIONS,
                        )
                        .await
                    {
                        warn!(%error, "automation scheduler evaluation failed closed");
                    }
                    if let Err(error) = scheduler
                        .dispatch_resumable(
                            dispatcher.as_ref(),
                            &task_cancellation,
                            grok_application::MAX_AUTOMATION_SCHEDULER_TICK_DEFINITIONS,
                        )
                        .await
                    {
                        warn!(%error, "automation scheduler dispatch failed closed");
                    }
                }
            }
            tokio::select! {
                () = task_cancellation.cancelled() => return,
                () = tokio::time::sleep(AUTOMATION_SCHEDULER_LOOP_INTERVAL) => {}
            }
        }
    });
    AutomationSchedulerRuntime { cancellation, task }
}

/// Production remains fail-closed until a qualified platform adapter exposes
/// the dedicated scheduled-work guest contract. Health-only guest control is
/// intentionally not widened into execution authority.
fn configured_scheduled_guest_dispatcher() -> Option<Arc<dyn ScheduledGuestDispatcher>> {
    #[cfg(target_os = "linux")]
    {
        linux_guest_transport::scheduled::LinuxScheduledGuestDispatcher::from_env()
            .map(|dispatcher| Arc::new(dispatcher) as Arc<dyn ScheduledGuestDispatcher>)
    }
    #[cfg(not(target_os = "linux"))]
    None
}

const fn scheduler_runtime_eligible(
    lifecycle: AutomationSchedulerLifecycle,
    dispatcher_configured: bool,
    isolation: grok_application::IsolationRuntimeFacts,
) -> bool {
    matches!(
        lifecycle,
        AutomationSchedulerLifecycle::KernelInitializedExecutionDisabled
    ) && dispatcher_configured
        && isolation.broker_qualified
        && isolation.strong_isolation_ready
}

/// Performs exactly one bounded, journal-only recovery pass before IPC starts.
///
/// Recovery cannot dispatch work: the scheduler service has no Run, provider,
/// tool, or timer dependency. Live prior ownership defers recovery, while a
/// persistence or integrity failure degrades only the automation kernel; none
/// of these outcomes blocks chat/app startup.
async fn recover_automation_scheduler(
    scheduler: &AutomationSchedulerService,
    owner_id: &AutomationSchedulerOwnerId,
) -> AutomationSchedulerLifecycle {
    match scheduler
        .recover_expired_claims(owner_id, MAX_AUTOMATION_SCHEDULER_RECOVERY_BATCH)
        .await
    {
        Ok(summary) if summary.truncated => {
            AutomationSchedulerLifecycle::RecoveryPendingExecutionDisabled
        }
        Ok(_) => AutomationSchedulerLifecycle::KernelInitializedExecutionDisabled,
        Err(error @ ApplicationError::Unavailable(_)) => {
            warn!(
                reason_code = automation_scheduler_recovery_reason(&error),
                "automation scheduler recovery remains pending"
            );
            AutomationSchedulerLifecycle::RecoveryPendingExecutionDisabled
        }
        Err(error) => {
            warn!(
                reason_code = automation_scheduler_recovery_reason(&error),
                "automation scheduler recovery failed closed"
            );
            AutomationSchedulerLifecycle::DegradedExecutionDisabled
        }
    }
}

const fn automation_scheduler_recovery_reason(error: &ApplicationError) -> &'static str {
    match error {
        ApplicationError::InvalidInput(_) => "invalid_input",
        ApplicationError::NotFound => "not_found",
        ApplicationError::Conflict => "conflict",
        ApplicationError::InvalidState(_) => "invalid_state",
        ApplicationError::Unavailable(_) => "unavailable",
        ApplicationError::Integrity(_) => "integrity_failure",
        ApplicationError::Unauthorized(_) => "unauthorized",
        ApplicationError::Storage(_) => "storage_failure",
        ApplicationError::DeadlineExceeded => "deadline_exceeded",
        ApplicationError::Cancelled => "cancelled",
    }
}

async fn configured_artifact_service(
    stores: &Stores,
    clock: Arc<SystemClock>,
    ids: Arc<dyn IdGenerator>,
) -> Result<(Arc<ArtifactService>, bool, bool), grok_application::ApplicationError> {
    let (content, retention, opener, content_available, open_available) =
        configured_artifact_content(stores).await;
    let artifacts = Arc::new(
        ArtifactService::new(
            stores.artifacts.clone(),
            content,
            opener,
            stores.workspace.clone(),
            clock,
            ids,
        )
        .with_content_retention(retention),
    );
    let content_available = recover_artifact_operations(&artifacts, content_available).await?;
    Ok((
        artifacts,
        content_available,
        open_available && content_available,
    ))
}

async fn recover_artifact_operations(
    artifacts: &ArtifactService,
    content_ready: bool,
) -> Result<bool, grok_application::ApplicationError> {
    let mut content_ready = content_ready;
    if content_ready {
        match artifacts
            .recover_incomplete_imports(MAX_ARTIFACT_RECOVERY_BATCH)
            .await
        {
            Ok(imports) => {
                if imports.committed > 0 || imports.failed > 0 {
                    warn!(
                        committed = imports.committed,
                        failed = imports.failed,
                        "recovered incomplete artifact imports without reusing a selected path"
                    );
                }
                if imports.truncated {
                    return Err(grok_application::ApplicationError::Unavailable(
                        "artifact import recovery backlog exceeds the bounded startup pass".into(),
                    ));
                }
            }
            Err(
                grok_application::ApplicationError::Unavailable(_)
                | grok_application::ApplicationError::DeadlineExceeded
                | grok_application::ApplicationError::Integrity(_),
            ) => {
                content_ready = false;
                warn!(
                    "artifact import recovery deferred; daemon is starting with Files unavailable"
                );
            }
            Err(error) => return Err(error),
        }
    } else {
        warn!("artifact import recovery deferred because private content is unavailable");
    }

    let opens = artifacts
        .recover_incomplete_opens(MAX_ARTIFACT_RECOVERY_BATCH)
        .await?;
    if opens.failed_before_dispatch > 0 || opens.interrupted_needs_review > 0 {
        warn!(
            failed_before_dispatch = opens.failed_before_dispatch,
            interrupted_needs_review = opens.interrupted_needs_review,
            "recovered incomplete artifact opens without replaying a platform side effect"
        );
    }
    if opens.truncated {
        return Err(grok_application::ApplicationError::Unavailable(
            "artifact open recovery backlog exceeds the bounded startup pass".into(),
        ));
    }

    if content_ready {
        match artifacts
            .recover_incomplete_removals(MAX_ARTIFACT_RECOVERY_BATCH)
            .await
        {
            Ok(removals) => {
                if removals.committed > 0 {
                    warn!(
                        committed = removals.committed,
                        "recovered incomplete artifact removals after exact private-namespace purge"
                    );
                }
                if removals.truncated {
                    return Err(grok_application::ApplicationError::Unavailable(
                        "artifact removal recovery backlog exceeds the bounded startup pass".into(),
                    ));
                }
            }
            Err(
                grok_application::ApplicationError::Unavailable(_)
                | grok_application::ApplicationError::DeadlineExceeded
                | grok_application::ApplicationError::Integrity(_),
            ) => {
                content_ready = false;
                warn!(
                    "artifact removal recovery deferred; daemon is starting with Files unavailable"
                );
            }
            Err(error) => return Err(error),
        }
    } else {
        warn!("artifact removal recovery deferred because private content is unavailable");
    }
    Ok(content_ready)
}

async fn configured_artifact_content(
    stores: &Stores,
) -> (
    Arc<dyn ArtifactContentStore>,
    Arc<dyn ArtifactContentRetention>,
    Arc<dyn ArtifactOpener>,
    bool,
    bool,
) {
    #[cfg(target_os = "linux")]
    if let Some(base) = stores.artifact_content_base.as_deref() {
        match LinuxArtifactContent::open(base) {
            Ok(mut adapter) => {
                let open_available = match adapter.qualify_open_portal().await {
                    Ok(()) => true,
                    Err(error) => {
                        warn!(%error, "artifact portal qualification remains fail-closed");
                        false
                    }
                };
                let adapter = Arc::new(adapter);
                return (
                    adapter.clone(),
                    adapter.clone(),
                    adapter,
                    true,
                    open_available,
                );
            }
            Err(error) => {
                warn!(%error, "artifact content storage remains fail-closed");
            }
        }
    }

    let unavailable = Arc::new(UnavailableArtifactContent);
    (
        unavailable.clone(),
        unavailable.clone(),
        unavailable,
        false,
        false,
    )
}

async fn recover_conversation_turns(
    conversation: &ConversationService,
) -> Result<(), grok_application::ApplicationError> {
    let recovery = conversation
        .recover_incomplete(MAX_CONVERSATION_RECOVERY_BATCH)
        .await?;
    if recovery.recovered() > 0 {
        warn!(
            cancelled_reserved = recovery.cancelled_reserved,
            interrupted_needs_review = recovery.interrupted_needs_review,
            "recovered incomplete conversation turns without replay"
        );
    }
    if recovery.truncated {
        return Err(grok_application::ApplicationError::Unavailable(
            "conversation recovery backlog exceeds the bounded startup pass".into(),
        ));
    }
    Ok(())
}

async fn recover_privileged_operations(
    stores: &Stores,
    clock: Arc<dyn grok_application::Clock>,
    ids: Arc<dyn IdGenerator>,
) -> Result<(), grok_application::ApplicationError> {
    let service = PrivilegedOperationService::new(stores.privileged_operations.clone(), clock, ids);
    let recovery = service
        .recover_interrupted(MAX_PRIVILEGED_RECOVERY_BATCH)
        .await?;
    if recovery.recovered() > 0 {
        warn!(
            retry_pending = recovery.retry_pending,
            interrupted_needs_review = recovery.interrupted_needs_review,
            "recovered interrupted privileged-operation journal entries without replay"
        );
    }
    if recovery.truncated {
        return Err(grok_application::ApplicationError::Unavailable(
            "privileged-operation recovery backlog exceeds the bounded startup pass".into(),
        ));
    }
    Ok(())
}

async fn recover_host_work(
    store: &dyn ExecutionStore,
    effects: &SideEffectService,
    runs: &RunService,
) -> Result<(), ApplicationError> {
    let incomplete = store
        .list_recoverable_host_effects(MAX_HOST_WORK_RECOVERY_BATCH + 1)
        .await?;
    if incomplete.len() > MAX_HOST_WORK_RECOVERY_BATCH {
        return Err(ApplicationError::Unavailable(
            "Host Work recovery backlog exceeds the bounded startup pass".into(),
        ));
    }
    let mut needs_review = 0_u64;
    let mut never_dispatched = 0_u64;
    for effect in incomplete {
        match effect.state {
            EffectState::Executing => {
                effects.interrupt(&effect.id, effect.revision).await?;
                needs_review = needs_review.saturating_add(1);
            }
            EffectState::Prepared => {
                let executing = effects.start(&effect.id, effect.revision).await?;
                effects
                    .finish(&executing.id, executing.revision, false)
                    .await?;
                never_dispatched = never_dispatched.saturating_add(1);
            }
            EffectState::Succeeded | EffectState::Failed | EffectState::NeedsReview => {}
        }
    }

    let recoverable = store
        .list_recoverable_host_runs(MAX_HOST_WORK_RECOVERY_BATCH + 1)
        .await?;
    if recoverable.len() > MAX_HOST_WORK_RECOVERY_BATCH {
        return Err(ApplicationError::Unavailable(
            "Host Work run recovery backlog exceeds the bounded startup pass".into(),
        ));
    }
    let mut closed_runs = 0_u64;
    for stale in recoverable {
        let current = store.get_run(&stale.id).await?;
        if current.state == RunState::InterruptedNeedsReview || current.state.is_terminal() {
            continue;
        }
        let next = if current.state == RunState::Queued {
            RunState::Cancelled
        } else {
            RunState::Failed
        };
        runs.transition(
            &current.id,
            current.revision,
            next,
            &format!("host-work-startup-recovery-{}", current.id),
        )
        .await?;
        closed_runs = closed_runs.saturating_add(1);
    }
    if needs_review > 0 || never_dispatched > 0 || closed_runs > 0 {
        warn!(
            needs_review,
            never_dispatched, closed_runs, "recovered incomplete Host Work without replay"
        );
    }
    Ok(())
}

fn configured_credential_enrollment(
    credentials: Arc<CredentialService>,
    mutations: Arc<dyn CredentialMutationStore>,
) -> Arc<CredentialEnrollmentService> {
    // Windows prompts through the audited Win32 boundary; unix daemons prompt
    // through pinentry. Both keep the entered key inside this process.
    Arc::new(CredentialEnrollmentService::new(
        credentials,
        mutations,
        Arc::new(NativeCredentialEnrollment::new()),
    ))
}

fn provider_network_policy(isolation_broker_qualified: bool) -> CapabilityFacts {
    CapabilityFacts {
        // Connectivity is optimistic. Official provider calls remain authoritative for liveness.
        online: true,
        isolation_broker_qualified,
        ..CapabilityFacts::default()
    }
}

fn configured_chat_services(
    stores: &Stores,
    workspace: Arc<WorkspaceService>,
    credentials: Arc<CredentialService>,
    supergrok: Arc<SuperGrokEnrollmentService>,
    default_rail: Arc<ChatRailSelection>,
    clock: Arc<dyn grok_application::Clock>,
    ids: Arc<dyn IdGenerator>,
) -> (Arc<ChatModelService>, Arc<ConversationService>) {
    let model_factory: Arc<dyn ConversationModelFactory> =
        Arc::new(OfficialXaiConversationModelFactory);
    let oauth_factory: Arc<dyn ConversationModelFactory> =
        Arc::new(OfficialXaiOAuthConversationModelFactory);
    let chat_models = Arc::new(ChatModelService::new_with_supergrok(
        stores.chat_model_preferences.clone(),
        credentials.clone(),
        model_factory.clone(),
        supergrok.clone(),
        oauth_factory.clone(),
        default_rail.clone(),
        clock.clone(),
    ));
    let conversation = Arc::new(ConversationService::new_with_supergrok(
        stores.conversation.clone(),
        workspace,
        credentials,
        model_factory,
        supergrok,
        oauth_factory,
        default_rail,
        clock,
        ids,
        stores.chat_model_preferences.clone(),
    ));
    (chat_models, conversation)
}

const fn isolation_probe_reason(error: IsolationProbeError) -> &'static str {
    match error {
        IsolationProbeError::Unavailable => "unavailable",
        IsolationProbeError::Unqualified => "unqualified",
        IsolationProbeError::Incompatible => "incompatible_contract",
        IsolationProbeError::Protocol => "protocol_failure",
    }
}

enum AgentRuntimeConfiguration {
    NotConfigured,
    Available(Arc<HostWorkRuntime>),
    Unavailable(AgentRuntimeUnavailableReason),
}

async fn configured_agent_runtime(vault: Arc<dyn SecretVault>) -> AgentRuntimeConfiguration {
    if legacy_acp_override_requested() {
        #[cfg(all(debug_assertions, feature = "debug-acp-descriptor"))]
        {
            return configured_legacy_agent_runtime().await;
        }
        #[cfg(not(all(debug_assertions, feature = "debug-acp-descriptor")))]
        {
            return invalid_agent_runtime_configuration();
        }
    }

    let keys = match configured_catalog_keys() {
        Ok(Some(keys)) => keys,
        Ok(None) => return AgentRuntimeConfiguration::NotConfigured,
        Err(()) => return invalid_agent_runtime_configuration(),
    };
    let Ok(grok_home) = default_grok_home_spec() else {
        return invalid_agent_runtime_configuration();
    };
    let Ok((component_root, catalog_path)) = product_component_layout() else {
        return unavailable_component_runtime();
    };
    let Ok(component) =
        load_managed_component(&component_root, &catalog_path, keys, vault.as_ref())
    else {
        return unavailable_component_runtime();
    };
    start_agent_runtime(component, grok_home).await
}

async fn start_agent_runtime(
    component: VerifiedGrokComponent,
    grok_home: GrokHomeSpec,
) -> AgentRuntimeConfiguration {
    let factory = Arc::new(GrokAcpRoleFactory::new(component, grok_home));
    let helper = configured_host_tools_helper();
    match HostWorkRuntime::start(factory, helper).await {
        Ok(runtime) => {
            info!("official Grok ACP runtime initialized");
            AgentRuntimeConfiguration::Available(Arc::new(runtime))
        }
        Err(error) => {
            let reason = AgentRuntimeUnavailableReason::Runtime(error.kind);
            warn!(
                reason_code = reason.code(),
                "official Grok agent runtime unavailable"
            );
            AgentRuntimeConfiguration::Unavailable(reason)
        }
    }
}

#[cfg(all(debug_assertions, feature = "debug-acp-descriptor"))]
async fn configured_legacy_agent_runtime() -> AgentRuntimeConfiguration {
    let Some(executable) = std::env::var_os("GROK_ACP_EXECUTABLE") else {
        return invalid_agent_runtime_configuration();
    };
    let Ok(version) = std::env::var("GROK_ACP_VERSION") else {
        return invalid_agent_runtime_configuration();
    };
    let Ok(sha256) = std::env::var("GROK_ACP_SHA256") else {
        return invalid_agent_runtime_configuration();
    };
    if std::env::var_os("GROK_ACP_WORKSPACE_ROOTS").is_some() {
        return invalid_agent_runtime_configuration();
    }
    let Ok(grok_home) = default_grok_home_spec() else {
        return invalid_agent_runtime_configuration();
    };
    let descriptor = ExternalGrokComponent {
        executable: PathBuf::from(executable),
        version,
        sha256,
        publisher: "xAI".into(),
    };
    let Ok(component) = VerifiedGrokComponent::verify(&descriptor) else {
        return unavailable_component_runtime();
    };
    start_agent_runtime(component, grok_home).await
}

fn legacy_acp_override_requested() -> bool {
    LEGACY_ACP_VARIABLES
        .iter()
        .any(|variable| std::env::var_os(variable).is_some())
}

fn configured_catalog_keys() -> Result<Option<Vec<TrustedCatalogKey>>, ()> {
    catalog_keys_configuration(
        ACP_BUILD_KEYS,
        ACP_BUILD_TRUST_BINDING,
        cfg!(debug_assertions),
    )
}

fn catalog_keys_configuration(
    configured: Option<&str>,
    binding: Option<&str>,
    debug_build: bool,
) -> Result<Option<Vec<TrustedCatalogKey>>, ()> {
    match (configured, binding) {
        (Some(value), Some(binding)) if catalog_trust_binding(value) == binding => {
            parse_catalog_keys(value).map(Some)
        }
        (None, None) if debug_build => Ok(None),
        _ => Err(()),
    }
}

fn catalog_trust_binding(value: &str) -> String {
    format!(
        "{ACP_BUILD_TRUST_BINDING_PREFIX}{}",
        hex::encode(Sha256::digest(value.as_bytes()))
    )
}

fn parse_catalog_keys(value: &str) -> Result<Vec<TrustedCatalogKey>, ()> {
    if value.is_empty() || value.len() > MAX_BUILD_KEY_INPUT_BYTES {
        return Err(());
    }
    let mut seen = HashSet::new();
    let mut keys = Vec::new();
    let mut previous_key_id = None;
    for record in value.split(';') {
        let (key_id, encoded) = record.split_once('=').ok_or(())?;
        if record.matches('=').count() != 1
            || encoded.len() != 64
            || !encoded
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            || !seen.insert(key_id)
            || previous_key_id.is_some_and(|previous| key_id <= previous)
        {
            return Err(());
        }
        let decoded = hex::decode(encoded).map_err(|_| ())?;
        let public_key: [u8; 32] = decoded.try_into().map_err(|_| ())?;
        keys.push(TrustedCatalogKey::new(key_id, public_key).map_err(|_| ())?);
        if keys.len() > MAX_BUILD_KEYS {
            return Err(());
        }
        previous_key_id = Some(key_id);
    }
    if keys.is_empty() {
        return Err(());
    }
    Ok(keys)
}

fn product_component_layout() -> Result<(PathBuf, PathBuf), ManagedComponentError> {
    let executable = std::env::current_exe().map_err(ManagedComponentError::Io)?;
    product_component_layout_from_executable(&executable)
}

fn product_component_layout_from_executable(
    daemon_executable: &Path,
) -> Result<(PathBuf, PathBuf), ManagedComponentError> {
    if !daemon_executable.is_absolute() {
        return Err(ManagedComponentError::Configuration);
    }
    let parent = daemon_executable
        .parent()
        .ok_or(ManagedComponentError::Configuration)?;
    let root = parent
        .join(ACP_COMPONENT_DIRECTORY)
        .join(ACP_COMPONENT_NAME);
    let catalog = root.join(ACP_CATALOG_FILE);
    Ok((root, catalog))
}

fn load_managed_component(
    component_root: &Path,
    catalog_path: &Path,
    keys: Vec<TrustedCatalogKey>,
    vault: &dyn SecretVault,
) -> Result<VerifiedGrokComponent, ManagedComponentError> {
    let watermark = load_catalog_watermark(vault)?;
    let catalog = read_bounded_catalog(catalog_path)?;
    let verifier = OfficialGrokCatalogVerifier::new(component_root, keys)
        .map_err(|_| ManagedComponentError::Verification)?;
    let catalog_component = verifier
        .verify(&catalog, watermark)
        .map_err(|_| ManagedComponentError::Verification)?;
    persist_catalog_watermark(vault, watermark, catalog_component.sequence())?;
    Ok(catalog_component.into_component())
}

fn read_bounded_catalog(path: &Path) -> Result<Vec<u8>, ManagedComponentError> {
    let metadata = std::fs::symlink_metadata(path).map_err(ManagedComponentError::Io)?;
    let maximum = u64::try_from(MAX_SIGNED_CATALOG_ENVELOPE_BYTES)
        .map_err(|_| ManagedComponentError::Verification)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > maximum
    {
        return Err(ManagedComponentError::Verification);
    }
    let file = File::open(path).map_err(ManagedComponentError::Io)?;
    let expected_len =
        usize::try_from(metadata.len()).map_err(|_| ManagedComponentError::Verification)?;
    let mut bytes = Vec::with_capacity(expected_len);
    file.take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(ManagedComponentError::Io)?;
    if bytes.len() != expected_len || bytes.len() > MAX_SIGNED_CATALOG_ENVELOPE_BYTES {
        return Err(ManagedComponentError::Verification);
    }
    Ok(bytes)
}

fn load_catalog_watermark(vault: &dyn SecretVault) -> Result<u64, ManagedComponentError> {
    let name = catalog_watermark_name()?;
    let value = match vault.get(&name) {
        Ok(value) => value,
        Err(VaultError::NotFound) => return Ok(0),
        Err(_) => return Err(ManagedComponentError::Vault),
    };
    let bytes = value.expose_secret();
    if bytes.len() != 16 || &bytes[..8] != ACP_CATALOG_WATERMARK_MAGIC {
        return Err(ManagedComponentError::Vault);
    }
    let sequence = u64::from_be_bytes(
        bytes[8..]
            .try_into()
            .map_err(|_| ManagedComponentError::Vault)?,
    );
    if sequence == 0 {
        return Err(ManagedComponentError::Vault);
    }
    Ok(sequence)
}

fn persist_catalog_watermark(
    vault: &dyn SecretVault,
    previous: u64,
    accepted: u64,
) -> Result<(), ManagedComponentError> {
    if accepted < previous || accepted == 0 {
        return Err(ManagedComponentError::Verification);
    }
    if accepted == previous {
        return Ok(());
    }
    let mut record = Vec::with_capacity(16);
    record.extend_from_slice(ACP_CATALOG_WATERMARK_MAGIC);
    record.extend_from_slice(&accepted.to_be_bytes());
    let value = SecretValue::new(record).map_err(|_| ManagedComponentError::Vault)?;
    vault
        .set(&catalog_watermark_name()?, &value)
        .map_err(|_| ManagedComponentError::Vault)
}

fn catalog_watermark_name() -> Result<SecretName, ManagedComponentError> {
    SecretName::new(ACP_CATALOG_WATERMARK_NAME).map_err(|_| ManagedComponentError::Configuration)
}

#[derive(Debug, thiserror::Error)]
enum ManagedComponentError {
    #[error("managed ACP component configuration is invalid")]
    Configuration,
    #[error("managed ACP component verification failed")]
    Verification,
    #[error("managed ACP component rollback store is unavailable")]
    Vault,
    #[error("managed ACP component filesystem operation failed")]
    Io(#[source] std::io::Error),
}

fn unavailable_component_runtime() -> AgentRuntimeConfiguration {
    let reason =
        AgentRuntimeUnavailableReason::Runtime(AgentRuntimeErrorKind::ComponentVerification);
    warn!(
        reason_code = reason.code(),
        "official Grok agent runtime unavailable"
    );
    AgentRuntimeConfiguration::Unavailable(reason)
}

fn default_grok_home_spec() -> Result<GrokHomeSpec, DynError> {
    let directories = directories::ProjectDirs::from("net", "Grok Insider", "Grok Desktop")
        .ok_or("operating system did not provide a local application data directory")?;
    let installation_id =
        std::env::var("GROK_INSTALLATION_ID").unwrap_or_else(|_| "default".into());
    Ok(GrokHomeSpec::new(
        directories.data_local_dir().to_path_buf(),
        installation_id,
    )?)
}

fn host_tools_endpoint_base() -> Result<PathBuf, DynError> {
    let directories = directories::ProjectDirs::from("net", "Grok Insider", "Grok Desktop")
        .ok_or("operating system did not provide a local application data directory")?;
    let path = directories.data_local_dir().join("host-tools-runtime");
    std::fs::create_dir_all(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(path)
}

fn host_tools_denied_filesystem_roots() -> Result<Vec<String>, DynError> {
    let directories = directories::ProjectDirs::from("net", "Grok Insider", "Grok Desktop")
        .ok_or("operating system did not provide a local application data directory")?;
    let root = directories.data_local_dir().canonicalize()?;
    Ok(vec![root.to_string_lossy().into_owned()])
}

fn configured_host_tools_helper() -> Option<VerifiedHostToolsHelper> {
    let candidate = if cfg!(debug_assertions) {
        std::env::var_os("GROK_HOST_TOOLS_MCP_EXECUTABLE")
            .map(PathBuf::from)
            .or_else(packaged_host_tools_helper)
    } else {
        packaged_host_tools_helper()
    };
    let Some(path) = candidate else {
        warn!("Host Tools helper is not packaged; Host Work remains unavailable");
        return None;
    };
    if let Ok(helper) = VerifiedHostToolsHelper::verify(path) {
        Some(helper)
    } else {
        warn!("Host Tools helper failed identity verification; Host Work remains unavailable");
        None
    }
}

fn packaged_host_tools_helper() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "grok-host-tools-mcp.exe"
    } else {
        "grok-host-tools-mcp"
    };
    std::env::current_exe()
        .ok()?
        .parent()
        .map(|parent| parent.join(name))
        .filter(|path| path.is_file())
}

fn invalid_agent_runtime_configuration() -> AgentRuntimeConfiguration {
    let reason = AgentRuntimeUnavailableReason::InvalidConfiguration;
    warn!(
        reason_code = reason.code(),
        "official Grok agent runtime unavailable"
    );
    AgentRuntimeConfiguration::Unavailable(reason)
}

struct Stores {
    execution: Arc<dyn ExecutionStore>,
    workspace: Arc<dyn WorkspaceStore>,
    scheduler: Arc<dyn AutomationSchedulerStore>,
    artifacts: Arc<dyn ArtifactStore>,
    conversation: Arc<dyn ConversationTurnStore>,
    credential_mutations: Arc<dyn CredentialMutationStore>,
    desktop_preferences: Arc<dyn DesktopPreferencesStore>,
    host_execution_policy: Arc<dyn HostExecutionPolicyStore>,
    chat_model_preferences: Arc<dyn ChatModelPreferenceStore>,
    privileged_operations: Arc<dyn PrivilegedOperationStore>,
    managed_integrations: Arc<dyn ManagedIntegrationLifecycleStore>,
    vault: Arc<dyn SecretVault>,
    artifact_content_base: Option<PathBuf>,
}

impl Stores {
    fn from_store<T>(
        store: Arc<T>,
        managed_integrations: Arc<dyn ManagedIntegrationLifecycleStore>,
        vault: Arc<dyn SecretVault>,
        artifact_content_base: Option<PathBuf>,
    ) -> Self
    where
        T: ArtifactStore
            + AutomationSchedulerStore
            + ConversationTurnStore
            + ChatModelPreferenceStore
            + CredentialMutationStore
            + DesktopPreferencesStore
            + ExecutionStore
            + HostExecutionPolicyStore
            + PrivilegedOperationStore
            + WorkspaceStore
            + 'static,
    {
        Self {
            execution: store.clone(),
            workspace: store.clone(),
            scheduler: store.clone(),
            artifacts: store.clone(),
            conversation: store.clone(),
            desktop_preferences: store.clone(),
            host_execution_policy: store.clone(),
            chat_model_preferences: store.clone(),
            privileged_operations: store.clone(),
            credential_mutations: store,
            managed_integrations,
            vault,
            artifact_content_base,
        }
    }
}

async fn stores() -> Result<Stores, DynError> {
    if std::env::var_os("GROK_DAEMON_EPHEMERAL").is_some() {
        if !cfg!(debug_assertions) {
            return Err("ephemeral execution storage is disabled in release builds".into());
        }
        info!("using explicitly requested non-persistent execution store");
        return Ok(Stores::from_store(
            Arc::new(InMemoryExecutionStore::new()),
            Arc::new(InMemoryManagedIntegrationLifecycleStore::new()),
            Arc::new(InMemorySecretVault::new()),
            None,
        ));
    }

    let configured_path = std::env::var_os("GROK_DATABASE_PATH");
    let key = std::env::var("GROK_DATABASE_KEY_HEX");
    match (configured_path, key) {
        (Some(path), Ok(mut encoded_key)) if cfg!(debug_assertions) => {
            let decoded_result = hex::decode(&encoded_key);
            encoded_key.zeroize();
            let mut decoded_key = decoded_result?;
            let parsed_key = decoded_key.as_slice().try_into();
            decoded_key.zeroize();
            let mut key: [u8; 32] = parsed_key
                .map_err(|_| "GROK_DATABASE_KEY_HEX must encode exactly 32 bytes")?;
            let provider = Arc::new(EphemeralKeyProvider::new(key));
            key.zeroize();
            let path = std::path::PathBuf::from(path);
            let artifact_content_base = absolute_parent(&path)?;
            let store = SqlCipherStore::open(path, provider).await?;
            let vault = os_vault()?;
            info!("using debug-configured persistent SQLCipher execution store");
            Ok(Stores::from_store(
                Arc::new(store.clone()),
                Arc::new(store),
                vault,
                Some(artifact_content_base),
            ))
        }
        (Some(_), Ok(_)) => Err(
            "release persistence requires a platform SecureKeyProvider; environment keys are debug-only"
                .into(),
        ),
        (None, Ok(mut encoded_key)) => {
            encoded_key.zeroize();
            Err("GROK_DATABASE_KEY_HEX requires an explicit debug database path".into())
        }
        (path, Err(std::env::VarError::NotPresent)) => {
            let path = match path {
                Some(path) if cfg!(debug_assertions) => std::path::PathBuf::from(path),
                Some(_) => return Err("database path overrides are debug-only".into()),
                None => default_database_path()?,
            };
            let installation_id = std::env::var("GROK_INSTALLATION_ID")
                .unwrap_or_else(|_| "default".into());
            let lock = DatabaseLock::acquire(&path)?;
            let artifact_content_base = absolute_parent(&path)?;
            let vault = Arc::new(OsVault::new(&installation_id)?);
            vault.ensure_database_key()?;
            let store = SqlCipherStore::open_locked(path, vault.clone(), lock).await?;
            info!("using platform-vault-backed persistent SQLCipher execution store");
            Ok(Stores::from_store(
                Arc::new(store.clone()),
                Arc::new(store),
                vault,
                Some(artifact_content_base),
            ))
        }
        (_, Err(error)) => Err(error.into()),
    }
}

fn absolute_parent(path: &Path) -> Result<PathBuf, DynError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    absolute
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "database path has no parent directory".into())
}

fn os_vault() -> Result<Arc<dyn SecretVault>, DynError> {
    let installation_id =
        std::env::var("GROK_INSTALLATION_ID").unwrap_or_else(|_| "default".into());
    Ok(Arc::new(OsVault::new(&installation_id)?))
}

fn default_database_path() -> Result<std::path::PathBuf, DynError> {
    let directories = directories::ProjectDirs::from("net", "Grok Insider", "Grok Desktop")
        .ok_or("operating system did not provide a local application data directory")?;
    let data_directory = directories.data_local_dir().join("data");
    ensure_private_database_directory(&data_directory)?;
    Ok(data_directory.join("grok.db"))
}

fn ensure_private_database_directory(path: &Path) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(path)?;
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StartupNonce {
    value: [u8; 32],
    standalone: bool,
}

fn startup_nonce() -> Result<StartupNonce, DynError> {
    let legacy_present = std::env::var_os(LEGACY_STARTUP_NONCE_VARIABLE).is_some();
    let marker = std::env::var_os(STARTUP_NONCE_STDIN_MARKER);
    let stdin = std::io::stdin();
    startup_nonce_from_reader(legacy_present, marker.as_deref(), &mut stdin.lock())
}

fn startup_nonce_from_reader(
    legacy_present: bool,
    marker: Option<&OsStr>,
    reader: &mut impl Read,
) -> Result<StartupNonce, DynError> {
    if legacy_present {
        return Err(
            "GROK_DAEMON_STARTUP_NONCE_HEX is no longer accepted; use the bounded stdin handoff"
                .into(),
        );
    }
    if let Some(marker) = marker {
        if marker != OsStr::new("1") {
            return Err("GROK_DAEMON_STARTUP_NONCE_STDIN must be exactly 1".into());
        }
        let mut nonce = [0; 32];
        reader.read_exact(&mut nonce).map_err(|_| {
            std::io::Error::new(
                ErrorKind::InvalidData,
                "startup nonce stdin must contain exactly 32 bytes",
            )
        })?;
        let mut extra = [0; 1];
        let extra_bytes = loop {
            match reader.read(&mut extra) {
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                result => break result?,
            }
        };
        if extra_bytes != 0 {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "startup nonce stdin must contain exactly 32 bytes",
            )
            .into());
        }
        return Ok(StartupNonce {
            value: nonce,
            standalone: false,
        });
    }

    let mut nonce = [0; 32];
    nonce[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    nonce[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    Ok(StartupNonce {
        value: nonce,
        standalone: true,
    })
}

async fn serve_tcp(address: &str, daemon: Arc<Daemon>) -> Result<(), DynError> {
    let address: SocketAddr = address.parse()?;
    if !address.ip().is_loopback() {
        return Err("development TCP transport must bind to loopback".into());
    }
    let listener = tokio::net::TcpListener::bind(address).await?;
    let connections = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_IPC_CONNECTIONS));
    info!(address = %listener.local_addr()?, "daemon development transport ready");
    loop {
        let permit = connections.clone().acquire_owned().await?;
        let (stream, _) = listener.accept().await?;
        spawn_connection(stream, daemon.clone(), permit);
    }
}

fn spawn_connection<S>(stream: S, daemon: Arc<Daemon>, permit: tokio::sync::OwnedSemaphorePermit)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let _permit = permit;
        if let Err(error) = serve_connection(stream, daemon).await {
            error!(%error, "local IPC connection failed");
        }
    });
}

#[cfg(unix)]
async fn serve_platform(daemon: Arc<Daemon>) -> Result<(), DynError> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    let path = if let Ok(path) = std::env::var("GROK_DAEMON_SOCKET") {
        std::path::PathBuf::from(path)
    } else {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map_or_else(std::env::temp_dir, std::path::PathBuf::from);
        runtime.join("grok-desktop").join("daemon.sock")
    };
    let parent = path.parent().ok_or("daemon socket has no parent")?;
    std::fs::create_dir_all(parent)?;
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_socket() => std::fs::remove_file(&path)?,
        Ok(_) => return Err("refusing to replace a non-socket daemon path".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let listener = tokio::net::UnixListener::bind(&path)?;
    let connections = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_IPC_CONNECTIONS));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    info!(path = %path.display(), "daemon local transport ready");
    loop {
        let permit = connections.clone().acquire_owned().await?;
        let (stream, _) = listener.accept().await?;
        spawn_connection(stream, daemon.clone(), permit);
    }
}

#[cfg(windows)]
async fn serve_platform(daemon: Arc<Daemon>) -> Result<(), DynError> {
    use tokio::net::windows::named_pipe::ServerOptions;

    let pipe_name = std::env::var("GROK_DAEMON_PIPE")
        .unwrap_or_else(|_| r"\\.\pipe\grok-desktop-daemon".into());
    let connections = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_IPC_CONNECTIONS));
    let mut first = true;
    loop {
        let permit = connections.clone().acquire_owned().await?;
        let mut options = ServerOptions::new();
        if first {
            options.first_pipe_instance(true);
            first = false;
        }
        let server = options.create(&pipe_name)?;
        server.connect().await?;
        spawn_connection(server, daemon.clone(), permit);
    }
}

#[cfg(not(any(unix, windows)))]
async fn serve_platform(_daemon: Arc<Daemon>) -> Result<(), DynError> {
    Err("this platform has no production local transport".into())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use ed25519_dalek::{Signer, SigningKey};
    use grok_application::{
        AutomationOccurrenceDispatch, AutomationOccurrenceDispatchResult,
        AutomationOccurrenceRunCompletion, AutomationScheduleCandidate,
        AutomationScheduleEvaluationCommit, AutomationScheduleEvaluationResult,
        AutomationSchedulerJournalStatus, AutomationSchedulerLeaseAcquisition,
        AutomationSchedulerRecoverySummary, ClaimAutomationOccurrence, StoreError,
    };
    use grok_domain::{
        AutomationId, AutomationOccurrence, AutomationOccurrenceId, AutomationSchedulerLease,
        AutomationSchedulerLeaseToken, RunId, UnixMillis,
    };
    use serde_json::json;
    use sha2::{Digest, Sha256};

    use super::*;

    const CATALOG_KEY_ID: &str = "xai-release-2026";
    const CATALOG_SIGNATURE_DOMAIN: &[u8] = b"grok.desktop.official-component-catalog.v1\0";
    const COMPONENT_BYTES: &[u8] = b"official Grok Build test component";
    const FUTURE_EXPIRY: u64 = 4_102_444_800;

    #[derive(Debug, Clone, Copy)]
    enum StartupRecoveryOutcome {
        Summary(AutomationSchedulerRecoverySummary),
        LeaseBusy,
        StorageFailure,
    }

    #[derive(Debug)]
    struct StartupRecoveryStore {
        outcome: StartupRecoveryOutcome,
        acquire_calls: AtomicUsize,
        recovery_calls: AtomicUsize,
    }

    impl StartupRecoveryStore {
        const fn new(outcome: StartupRecoveryOutcome) -> Self {
            Self {
                outcome,
                acquire_calls: AtomicUsize::new(0),
                recovery_calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl AutomationSchedulerStore for StartupRecoveryStore {
        async fn acquire_automation_scheduler_lease(
            &self,
            owner_id: &AutomationSchedulerOwnerId,
            now: UnixMillis,
            ttl_ms: u64,
        ) -> Result<AutomationSchedulerLeaseAcquisition, StoreError> {
            self.acquire_calls.fetch_add(1, Ordering::SeqCst);
            if matches!(self.outcome, StartupRecoveryOutcome::LeaseBusy) {
                let lease = AutomationSchedulerLease::acquire(
                    AutomationSchedulerOwnerId::new("daemon-prior").expect("prior owner"),
                    1,
                    now,
                    ttl_ms,
                )
                .map_err(|_| StoreError::Conflict)?;
                return Ok(AutomationSchedulerLeaseAcquisition::Busy { lease });
            }
            let lease = AutomationSchedulerLease::acquire(owner_id.clone(), 1, now, ttl_ms)
                .map_err(|_| StoreError::Conflict)?;
            Ok(AutomationSchedulerLeaseAcquisition::Acquired {
                lease,
                continuous: false,
                continuity_started_at: now,
            })
        }

        async fn list_automation_schedule_candidates(
            &self,
            _after: Option<&AutomationId>,
            _limit: usize,
        ) -> Result<Vec<AutomationScheduleCandidate>, StoreError> {
            panic!("startup recovery must not evaluate schedules")
        }

        async fn commit_automation_schedule_evaluation(
            &self,
            _evaluation: AutomationScheduleEvaluationCommit,
        ) -> Result<AutomationScheduleEvaluationResult, StoreError> {
            panic!("startup recovery must not materialize occurrences")
        }

        async fn get_automation_occurrence(
            &self,
            _id: &AutomationOccurrenceId,
        ) -> Result<AutomationOccurrence, StoreError> {
            panic!("startup recovery must not load individual occurrences")
        }

        async fn list_automation_occurrences(
            &self,
            _automation_id: &AutomationId,
            _after: Option<&AutomationOccurrenceId>,
            _limit: usize,
        ) -> Result<Vec<AutomationOccurrence>, StoreError> {
            panic!("startup recovery must not expose occurrence pages")
        }

        async fn claim_automation_occurrence(
            &self,
            _claim: ClaimAutomationOccurrence,
        ) -> Result<AutomationOccurrence, StoreError> {
            panic!("startup recovery must not claim work")
        }

        async fn claim_and_bind_automation_occurrence(
            &self,
            _dispatch: AutomationOccurrenceDispatch,
        ) -> Result<AutomationOccurrenceDispatchResult, StoreError> {
            panic!("startup recovery must not bind work")
        }

        async fn list_resumable_automation_dispatches(
            &self,
            _after: Option<&AutomationOccurrenceId>,
            _limit: usize,
        ) -> Result<Vec<AutomationOccurrenceDispatchResult>, StoreError> {
            panic!("startup recovery must not resume work")
        }

        async fn begin_automation_occurrence_run(
            &self,
            _occurrence_id: &AutomationOccurrenceId,
            _expected_occurrence_revision: u64,
            _run_id: &RunId,
            _expected_run_revision: u64,
            _now: UnixMillis,
        ) -> Result<AutomationOccurrenceDispatchResult, StoreError> {
            panic!("startup recovery must not begin work")
        }

        async fn complete_automation_occurrence_run(
            &self,
            _occurrence_id: &AutomationOccurrenceId,
            _expected_revision: u64,
            _run_id: &RunId,
            _completion: AutomationOccurrenceRunCompletion,
            _now: UnixMillis,
        ) -> Result<AutomationOccurrence, StoreError> {
            panic!("startup recovery must not complete work")
        }

        async fn recover_automation_occurrence_claims(
            &self,
            _lease: &AutomationSchedulerLeaseToken,
            _now: UnixMillis,
            limit: usize,
        ) -> Result<AutomationSchedulerRecoverySummary, StoreError> {
            self.recovery_calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(limit, MAX_AUTOMATION_SCHEDULER_RECOVERY_BATCH);
            match self.outcome {
                StartupRecoveryOutcome::Summary(summary) => Ok(summary),
                StartupRecoveryOutcome::StorageFailure => {
                    Err(StoreError::Internal("sensitive backend detail".into()))
                }
                StartupRecoveryOutcome::LeaseBusy => {
                    panic!("busy lease must stop before recovery")
                }
            }
        }

        async fn automation_scheduler_journal_status(
            &self,
        ) -> Result<AutomationSchedulerJournalStatus, StoreError> {
            panic!("startup recovery must not need a second journal query")
        }

        async fn link_automation_occurrence_run(
            &self,
            _lease: &grok_domain::AutomationSchedulerLeaseToken,
            _occurrence_id: &AutomationOccurrenceId,
            _expected_revision: u64,
            _run_id: grok_domain::RunId,
            _now: UnixMillis,
        ) -> Result<grok_domain::AutomationOccurrence, StoreError> {
            panic!("startup recovery must not link runs")
        }
    }

    #[derive(Debug)]
    struct IntegrityArtifactContent;

    #[derive(Debug)]
    struct SuccessfulArtifactRetention;

    #[async_trait::async_trait]
    impl ArtifactContentRetention for SuccessfulArtifactRetention {
        async fn purge_content(
            &self,
            _content: &grok_domain::ArtifactVersion,
            _deadline_unix_ms: u64,
        ) -> Result<
            grok_application::ArtifactContentPurge,
            grok_application::ArtifactRetentionFailureCode,
        > {
            Ok(grok_application::ArtifactContentPurge::Purged)
        }
    }

    #[async_trait::async_trait]
    impl ArtifactContentStore for IntegrityArtifactContent {
        async fn prepare_import_content(
            &self,
            _source: &grok_application::SelectedSourcePath,
            _artifact_id: &grok_domain::ArtifactId,
            _content_version: u32,
            _media_type: &str,
            _max_bytes: u64,
            _deadline_unix_ms: u64,
        ) -> Result<
            grok_application::PreparedArtifactContent,
            grok_application::ArtifactImportFailureCode,
        > {
            Err(grok_application::ArtifactImportFailureCode::IntegrityFailure)
        }

        async fn publish_content(
            &self,
            _content: &grok_domain::ArtifactVersion,
            _deadline_unix_ms: u64,
        ) -> Result<
            grok_application::ArtifactContentPublication,
            grok_application::ArtifactImportFailureCode,
        > {
            Err(grok_application::ArtifactImportFailureCode::IntegrityFailure)
        }

        async fn content_status(
            &self,
            _content: &grok_domain::ArtifactVersion,
            _deadline_unix_ms: u64,
        ) -> Result<
            grok_application::ArtifactContentStatus,
            grok_application::ArtifactImportFailureCode,
        > {
            Err(grok_application::ArtifactImportFailureCode::IntegrityFailure)
        }

        async fn discard_prepared_content(
            &self,
            _content: &grok_domain::ArtifactVersion,
        ) -> Result<(), grok_application::ArtifactImportFailureCode> {
            Ok(())
        }

        async fn discard_reserved_content(
            &self,
            _artifact_id: &grok_domain::ArtifactId,
            _content_version: u32,
        ) -> Result<(), grok_application::ArtifactImportFailureCode> {
            Ok(())
        }
    }

    fn test_artifact_content(id: grok_domain::ArtifactId) -> grok_domain::ArtifactVersion {
        grok_domain::ArtifactVersion::new(id, 1, [23; 32], "text/plain".into(), 4, 101)
            .expect("content version")
    }

    #[test]
    fn provider_policy_records_only_static_broker_qualification() {
        assert_eq!(
            provider_network_policy(true),
            CapabilityFacts {
                online: true,
                isolation_broker_qualified: true,
                ..CapabilityFacts::default()
            }
        );
        let unavailable = provider_network_policy(false);
        assert!(!unavailable.isolation_broker_qualified);
        assert!(!unavailable.strong_isolation_ready);
    }

    #[test]
    fn scheduler_owner_is_the_exact_daemon_instance_identity() {
        let (instance_id, owner_id) =
            new_daemon_instance_identity().expect("valid process identity");
        assert_eq!(owner_id.as_str(), instance_id);
    }

    #[tokio::test]
    async fn scheduler_runtime_shutdown_cancels_and_joins() {
        let cancellation = CancellationToken::new();
        let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let task_cancellation = cancellation.clone();
        let task_observed = observed.clone();
        let task = tokio::spawn(async move {
            task_cancellation.cancelled().await;
            task_observed.store(true, Ordering::SeqCst);
        });
        AutomationSchedulerRuntime { cancellation, task }
            .shutdown()
            .await
            .expect("joined scheduler task");
        assert!(observed.load(Ordering::SeqCst));
    }

    #[test]
    fn production_scheduler_dispatcher_is_fail_closed() {
        assert!(configured_scheduled_guest_dispatcher().is_none());
    }

    #[test]
    fn scheduler_runtime_requires_journal_dispatcher_and_live_isolation() {
        let ready = grok_application::IsolationRuntimeFacts {
            broker_qualified: true,
            strong_isolation_ready: true,
        };
        assert!(scheduler_runtime_eligible(
            AutomationSchedulerLifecycle::KernelInitializedExecutionDisabled,
            true,
            ready,
        ));
        assert!(!scheduler_runtime_eligible(
            AutomationSchedulerLifecycle::RecoveryPendingExecutionDisabled,
            true,
            ready,
        ));
        assert!(!scheduler_runtime_eligible(
            AutomationSchedulerLifecycle::KernelInitializedExecutionDisabled,
            false,
            ready,
        ));
        for isolation in [
            grok_application::IsolationRuntimeFacts {
                broker_qualified: false,
                strong_isolation_ready: true,
            },
            grok_application::IsolationRuntimeFacts {
                broker_qualified: true,
                strong_isolation_ready: false,
            },
        ] {
            assert!(!scheduler_runtime_eligible(
                AutomationSchedulerLifecycle::KernelInitializedExecutionDisabled,
                true,
                isolation,
            ));
        }
    }

    #[tokio::test]
    async fn scheduler_startup_recovery_is_single_bounded_and_non_executing() {
        let store = Arc::new(StartupRecoveryStore::new(StartupRecoveryOutcome::Summary(
            AutomationSchedulerRecoverySummary {
                released_unlinked: 1,
                interrupted_linked: 1,
                attempts_exhausted: 1,
                resumable_bound_queued: 0,
                truncated: false,
            },
        )));
        let scheduler = AutomationSchedulerService::new(
            store.clone(),
            Arc::new(grok_memory::FixedClock::new(100)),
            Arc::new(grok_memory::SequentialIdGenerator::new()),
        );
        let owner = AutomationSchedulerOwnerId::new("daemon-startup").expect("owner");

        assert_eq!(
            recover_automation_scheduler(&scheduler, &owner).await,
            AutomationSchedulerLifecycle::KernelInitializedExecutionDisabled
        );
        assert_eq!(store.acquire_calls.load(Ordering::SeqCst), 1);
        assert_eq!(store.recovery_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn truncated_scheduler_startup_recovery_remains_pending() {
        let store = Arc::new(StartupRecoveryStore::new(StartupRecoveryOutcome::Summary(
            AutomationSchedulerRecoverySummary {
                released_unlinked: MAX_AUTOMATION_SCHEDULER_RECOVERY_BATCH,
                truncated: true,
                ..AutomationSchedulerRecoverySummary::default()
            },
        )));
        let scheduler = AutomationSchedulerService::new(
            store,
            Arc::new(grok_memory::FixedClock::new(100)),
            Arc::new(grok_memory::SequentialIdGenerator::new()),
        );
        let owner = AutomationSchedulerOwnerId::new("daemon-truncated").expect("owner");

        assert_eq!(
            recover_automation_scheduler(&scheduler, &owner).await,
            AutomationSchedulerLifecycle::RecoveryPendingExecutionDisabled
        );
    }

    #[tokio::test]
    async fn scheduler_startup_failure_degrades_without_failing_startup() {
        let store = Arc::new(StartupRecoveryStore::new(
            StartupRecoveryOutcome::StorageFailure,
        ));
        let scheduler = AutomationSchedulerService::new(
            store.clone(),
            Arc::new(grok_memory::FixedClock::new(100)),
            Arc::new(grok_memory::SequentialIdGenerator::new()),
        );
        let owner = AutomationSchedulerOwnerId::new("daemon-degraded").expect("owner");

        assert_eq!(
            recover_automation_scheduler(&scheduler, &owner).await,
            AutomationSchedulerLifecycle::DegradedExecutionDisabled
        );
        assert_eq!(store.acquire_calls.load(Ordering::SeqCst), 1);
        assert_eq!(store.recovery_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            automation_scheduler_recovery_reason(&ApplicationError::Storage(
                "must not be logged".into()
            )),
            "storage_failure"
        );
    }

    #[tokio::test]
    async fn live_prior_scheduler_lease_keeps_startup_recovery_pending() {
        let store = Arc::new(StartupRecoveryStore::new(StartupRecoveryOutcome::LeaseBusy));
        let scheduler = AutomationSchedulerService::new(
            store.clone(),
            Arc::new(grok_memory::FixedClock::new(100)),
            Arc::new(grok_memory::SequentialIdGenerator::new()),
        );
        let owner = AutomationSchedulerOwnerId::new("daemon-new").expect("new owner");

        assert_eq!(
            recover_automation_scheduler(&scheduler, &owner).await,
            AutomationSchedulerLifecycle::RecoveryPendingExecutionDisabled
        );
        assert_eq!(store.acquire_calls.load(Ordering::SeqCst), 1);
        assert_eq!(store.recovery_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            automation_scheduler_recovery_reason(&ApplicationError::Unavailable(
                "owner detail must not be logged".into()
            )),
            "unavailable"
        );
    }

    #[test]
    fn isolation_probe_logs_only_stable_failure_classes() {
        assert_eq!(
            isolation_probe_reason(IsolationProbeError::Unavailable),
            "unavailable"
        );
        assert_eq!(
            isolation_probe_reason(IsolationProbeError::Unqualified),
            "unqualified"
        );
        assert_eq!(
            isolation_probe_reason(IsolationProbeError::Incompatible),
            "incompatible_contract"
        );
        assert_eq!(
            isolation_probe_reason(IsolationProbeError::Protocol),
            "protocol_failure"
        );
    }

    #[tokio::test]
    async fn transient_artifact_recovery_starts_files_limited_and_keeps_the_journal() {
        use grok_application::{
            ArtifactImportReservation, CreateProject, MutationCommand, WorkspaceService,
        };
        use grok_domain::{Artifact, ArtifactId};
        use grok_memory::{FixedClock, SequentialIdGenerator};

        let store = Arc::new(InMemoryExecutionStore::new());
        let clock = Arc::new(FixedClock::new(100));
        let ids = Arc::new(SequentialIdGenerator::new());
        let workspace = WorkspaceService::new(store.clone(), clock.clone(), ids.clone());
        let project = workspace
            .create_project(
                CreateProject {
                    name: "Artifact recovery".into(),
                    description: String::new(),
                },
                "artifact-recovery-project",
            )
            .await
            .expect("project");
        let artifact = Artifact::new_unavailable(
            ArtifactId::new("artifact-recovery-pending").expect("artifact ID"),
            project.id,
            None,
            "pending.txt".into(),
            100,
        )
        .expect("artifact");
        let command = MutationCommand {
            scope: "import_artifact".into(),
            key: "artifact-recovery-pending".into(),
            fingerprint: [17; 32],
        };
        assert!(matches!(
            store
                .reserve_import(artifact, &command)
                .await
                .expect("reserve import"),
            ArtifactImportReservation::NewlyPrepared(_)
        ));
        let unavailable = Arc::new(UnavailableArtifactContent);
        let artifacts = ArtifactService::new(
            store.clone(),
            unavailable.clone(),
            unavailable,
            store.clone(),
            clock.clone(),
            ids.clone(),
        );

        assert!(
            !recover_artifact_operations(&artifacts, true)
                .await
                .expect("start limited")
        );
        let replay = store
            .resolve_import(&command)
            .await
            .expect("resolve pending")
            .expect("pending journal");
        assert_eq!(
            replay.state,
            grok_application::ArtifactImportState::Prepared
        );

        let content = test_artifact_content(replay.artifact.id.clone());
        let ready = store
            .mark_content_ready(&replay.artifact.id, replay.revision, content, 101)
            .await
            .expect("content ready");
        assert!(matches!(
            ready,
            grok_application::ArtifactContentReadyResult::ContentReady(_)
        ));
        let integrity = Arc::new(IntegrityArtifactContent);
        let artifacts = ArtifactService::new(
            store.clone(),
            integrity,
            Arc::new(UnavailableArtifactContent),
            store.clone(),
            clock,
            ids,
        );
        assert!(
            !recover_artifact_operations(&artifacts, true)
                .await
                .expect("start limited after integrity failure")
        );
        let replay = store
            .resolve_import(&command)
            .await
            .expect("resolve retained integrity journal")
            .expect("retained integrity journal");
        assert_eq!(
            replay.state,
            grok_application::ArtifactImportState::ContentReady
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn startup_recovery_finishes_pending_removal_without_portal_or_open_dispatch() {
        use grok_application::{
            ArtifactContentReadyResult, ArtifactImportReservation, ArtifactRemovalReservation,
            ArtifactRemovalState, CreateProject, MutationCommand, WorkspaceService,
        };
        use grok_domain::{Artifact, ArtifactId, ArtifactState};
        use grok_memory::{FixedClock, SequentialIdGenerator};

        let store = Arc::new(InMemoryExecutionStore::new());
        let clock = Arc::new(FixedClock::new(100));
        let ids = Arc::new(SequentialIdGenerator::new());
        let workspace = WorkspaceService::new(store.clone(), clock.clone(), ids.clone());
        let project = workspace
            .create_project(
                CreateProject {
                    name: "Removal recovery".into(),
                    description: String::new(),
                },
                "removal-recovery-project",
            )
            .await
            .expect("project");
        let artifact = Artifact::new_unavailable(
            ArtifactId::new("artifact-removal-pending").expect("artifact ID"),
            project.id.clone(),
            None,
            "remove.txt".into(),
            100,
        )
        .expect("artifact");
        let import_command = MutationCommand {
            scope: "import_artifact".into(),
            key: "removal-recovery-import".into(),
            fingerprint: [41; 32],
        };
        let prepared = match store
            .reserve_import(artifact, &import_command)
            .await
            .expect("reserve import")
        {
            ArtifactImportReservation::NewlyPrepared(plan) => plan,
            ArtifactImportReservation::ExactReplay(_) => panic!("new import"),
        };
        let content = test_artifact_content(prepared.artifact.id.clone());
        let ready = match store
            .mark_content_ready(&prepared.artifact.id, 0, content.clone(), 101)
            .await
            .expect("content ready")
        {
            ArtifactContentReadyResult::ContentReady(plan) => plan,
            ArtifactContentReadyResult::QuotaExceeded { .. } => panic!("quota"),
        };
        let mut available = ready.artifact.clone();
        available
            .record_content(content.summary(), 102)
            .expect("available");
        let committed = store
            .commit_import(available, 0, ready.revision, content, 102)
            .await
            .expect("commit import");
        let removal_command = MutationCommand {
            scope: "remove_artifact".into(),
            key: "removal-recovery".into(),
            fingerprint: [42; 32],
        };
        let pending = match store
            .reserve_removal(
                &committed.artifact.id,
                committed.artifact.revision,
                1,
                &removal_command,
                103,
            )
            .await
            .expect("reserve removal")
        {
            ArtifactRemovalReservation::NewlyPending(plan) => plan,
            ArtifactRemovalReservation::ExactReplay(_) => panic!("new removal"),
        };
        assert_eq!(pending.state, ArtifactRemovalState::Pending);
        assert_eq!(pending.artifact.state, ArtifactState::Deleted);

        let unavailable = Arc::new(UnavailableArtifactContent);
        let artifacts = ArtifactService::new(
            store.clone(),
            unavailable.clone(),
            unavailable,
            store.clone(),
            clock,
            ids,
        )
        .with_content_retention(Arc::new(SuccessfulArtifactRetention));
        assert!(
            recover_artifact_operations(&artifacts, true)
                .await
                .expect("removal recovery remains qualified")
        );
        let terminal = store
            .resolve_removal(&removal_command)
            .await
            .expect("resolve removal")
            .expect("removal journal");
        assert_eq!(terminal.state, ArtifactRemovalState::Committed);
        assert_eq!(
            store
                .quota_usage(&project.id)
                .await
                .expect("released quota")
                .project_bytes,
            0
        );
    }

    #[test]
    fn startup_nonce_reader_accepts_only_the_exact_marked_payload() {
        let expected = [0xa5; 32];
        let mut exact = std::io::Cursor::new(expected);
        assert_eq!(
            startup_nonce_from_reader(false, Some(OsStr::new("1")), &mut exact)
                .expect("exact stdin nonce"),
            StartupNonce {
                value: expected,
                standalone: false,
            }
        );

        let mut ignored = std::io::Cursor::new([0xff; 33]);
        let standalone = startup_nonce_from_reader(false, None, &mut ignored)
            .expect("standalone nonce without marker");
        assert!(standalone.standalone);
        assert_eq!(ignored.position(), 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn database_directory_is_private_even_when_it_already_exists() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary application data directory");
        let data_directory = directory.path().join("data");
        std::fs::create_dir(&data_directory).expect("existing data directory");
        std::fs::set_permissions(&data_directory, std::fs::Permissions::from_mode(0o777))
            .expect("make fixture permissive");

        ensure_private_database_directory(&data_directory).expect("secure data directory");

        let mode = std::fs::metadata(data_directory)
            .expect("data directory metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }

    struct CatalogFixture {
        _directory: tempfile::TempDir,
        component_root: PathBuf,
        catalog_path: PathBuf,
        signing_key: SigningKey,
    }

    impl CatalogFixture {
        fn new(sequence: u64) -> Self {
            let directory = tempfile::tempdir().expect("temporary product directory");
            let component_root = directory.path().join("components").join("grok-acp");
            let bin = component_root.join("bin");
            std::fs::create_dir_all(&bin).expect("component directory");
            write_executable(&bin.join(component_executable_name()), COMPONENT_BYTES);
            let catalog_path = component_root.join(ACP_CATALOG_FILE);
            let signing_key = SigningKey::from_bytes(&[7; 32]);
            let fixture = Self {
                _directory: directory,
                component_root,
                catalog_path,
                signing_key,
            };
            fixture.write_catalog(sequence);
            fixture
        }

        fn trusted_keys(&self) -> Vec<TrustedCatalogKey> {
            vec![
                TrustedCatalogKey::new(CATALOG_KEY_ID, self.signing_key.verifying_key().to_bytes())
                    .expect("trusted catalog key"),
            ]
        }

        fn write_catalog(&self, sequence: u64) {
            let payload = serde_json::to_vec(&json!({
                "schema": "grok.official-component-catalog/v1",
                "sequence": sequence,
                "expiresAtUnixSeconds": FUTURE_EXPIRY,
                "components": [{
                    "name": "grok-build",
                    "publisher": "xAI",
                    "version": "1.2.3",
                    "os": component_operating_system(),
                    "architecture": component_architecture(),
                    "executable": format!("bin/{}", component_executable_name()),
                    "sha256": hex::encode(Sha256::digest(COMPONENT_BYTES)),
                    "size": COMPONENT_BYTES.len(),
                }],
            }))
            .expect("catalog payload");
            let signature = self
                .signing_key
                .sign(&catalog_signature_message(CATALOG_KEY_ID, &payload));
            let envelope = serde_json::to_vec(&json!({
                "schema": "grok.official-component-catalog-envelope/v1",
                "keyId": CATALOG_KEY_ID,
                "payload": STANDARD.encode(payload),
                "signature": STANDARD.encode(signature.to_bytes()),
            }))
            .expect("catalog envelope");
            std::fs::write(&self.catalog_path, envelope).expect("signed catalog");
        }
    }

    #[derive(Debug, Default)]
    struct FailingSetVault {
        inner: InMemorySecretVault,
    }

    impl SecretVault for FailingSetVault {
        fn get(&self, name: &SecretName) -> Result<SecretValue, VaultError> {
            self.inner.get(name)
        }

        fn set(&self, _name: &SecretName, _value: &SecretValue) -> Result<(), VaultError> {
            Err(VaultError::Unavailable)
        }

        fn delete(&self, name: &SecretName) -> Result<(), VaultError> {
            self.inner.delete(name)
        }
    }

    #[test]
    fn release_catalog_keys_fail_closed_when_not_pinned() {
        assert!(catalog_keys_configuration(None, None, false).is_err());
        assert!(matches!(
            catalog_keys_configuration(None, None, true),
            Ok(None)
        ));
        let key = SigningKey::from_bytes(&[7; 32]);
        let configured = format!("release-a={}", hex::encode(key.verifying_key().to_bytes()));
        let binding = catalog_trust_binding(&configured);
        assert!(catalog_keys_configuration(Some(&configured), None, false).is_err());
        assert!(catalog_keys_configuration(Some(&configured), Some("invalid"), false).is_err());
        assert!(
            catalog_keys_configuration(Some(&configured), Some(&binding), false)
                .expect("bound keys")
                .is_some()
        );
    }

    #[test]
    fn wisp_catalog_trust_requires_independent_bound_release_keys() {
        let key = SigningKey::from_bytes(&[11; 32]);
        let configured = format!(
            "wisp-release={}",
            hex::encode(key.verifying_key().to_bytes())
        );
        let binding = wisp_catalog_trust_binding(&configured);
        assert_ne!(binding, catalog_trust_binding(&configured));
        assert_eq!(
            parse_wisp_catalog_keys(&configured)
                .expect("valid independent Wisp trust")
                .len(),
            1
        );
        assert!(parse_wisp_catalog_keys("wisp-release=not-a-key").is_err());
        assert!(parse_wisp_catalog_keys(&format!("{configured};{configured}")).is_err());
    }

    #[test]
    fn parses_only_bounded_strict_build_key_records() {
        let first = SigningKey::from_bytes(&[7; 32]);
        let second = SigningKey::from_bytes(&[9; 32]);
        let first_hex = hex::encode(first.verifying_key().to_bytes());
        let second_hex = hex::encode(second.verifying_key().to_bytes());
        let configured = format!("release-a={first_hex};release-b={second_hex}");
        let keys = parse_catalog_keys(&configured).expect("valid build keys");
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].key_id(), "release-a");
        assert_eq!(keys[1].key_id(), "release-b");

        let malformed = [
            String::new(),
            "missing-separator".into(),
            format!("release-a={first_hex};release-a={second_hex}"),
            format!("release-a={}", first_hex.to_uppercase()),
            format!("release-a={first_hex}="),
            format!("invalid key={first_hex}"),
            format!("release-a={first_hex};"),
            format!("release-b={second_hex};release-a={first_hex}"),
        ];
        for value in malformed {
            assert!(parse_catalog_keys(&value).is_err(), "accepted {value:?}");
        }

        let too_many = (0..=MAX_BUILD_KEYS)
            .map(|index| format!("release-{index}={first_hex}"))
            .collect::<Vec<_>>()
            .join(";");
        assert!(parse_catalog_keys(&too_many).is_err());
        assert!(parse_catalog_keys(&"x".repeat(MAX_BUILD_KEY_INPUT_BYTES + 1)).is_err());
    }

    #[test]
    fn product_component_layout_is_fixed_beside_the_daemon() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let executable = directory.path().join("app").join("grok-daemon.exe");
        let (root, catalog) =
            product_component_layout_from_executable(&executable).expect("product layout");
        assert_eq!(
            root,
            executable
                .parent()
                .expect("daemon parent")
                .join(ACP_COMPONENT_DIRECTORY)
                .join(ACP_COMPONENT_NAME)
        );
        assert_eq!(catalog, root.join(ACP_CATALOG_FILE));
        assert!(matches!(
            product_component_layout_from_executable(Path::new("grok-daemon.exe")),
            Err(ManagedComponentError::Configuration)
        ));
    }

    #[test]
    fn verified_catalog_persists_sequence_and_reverifies_component() {
        let fixture = CatalogFixture::new(7);
        let vault = InMemorySecretVault::new();
        let component = load_managed_component(
            &fixture.component_root,
            &fixture.catalog_path,
            fixture.trusted_keys(),
            &vault,
        )
        .expect("managed component");

        component.reverify().expect("spawn-time reverification");
        assert_eq!(load_catalog_watermark(&vault).expect("watermark"), 7);
    }

    #[test]
    fn catalog_rollback_is_rejected_without_lowering_watermark() {
        let fixture = CatalogFixture::new(4);
        let vault = InMemorySecretVault::new();
        persist_catalog_watermark(&vault, 0, 5).expect("initial watermark");

        assert!(matches!(
            load_managed_component(
                &fixture.component_root,
                &fixture.catalog_path,
                fixture.trusted_keys(),
                &vault,
            ),
            Err(ManagedComponentError::Verification)
        ));
        assert_eq!(load_catalog_watermark(&vault).expect("watermark"), 5);
    }

    #[test]
    fn component_is_not_returned_when_watermark_persistence_fails() {
        let fixture = CatalogFixture::new(6);
        let vault = FailingSetVault::default();

        assert!(matches!(
            load_managed_component(
                &fixture.component_root,
                &fixture.catalog_path,
                fixture.trusted_keys(),
                &vault,
            ),
            Err(ManagedComponentError::Vault)
        ));
    }

    #[test]
    fn malformed_stored_watermark_fails_closed() {
        let fixture = CatalogFixture::new(6);
        let vault = InMemorySecretVault::new();
        vault
            .set(
                &catalog_watermark_name().expect("watermark name"),
                &SecretValue::new(b"invalid".to_vec()).expect("malformed record"),
            )
            .expect("seed malformed watermark");

        assert!(matches!(
            load_managed_component(
                &fixture.component_root,
                &fixture.catalog_path,
                fixture.trusted_keys(),
                &vault,
            ),
            Err(ManagedComponentError::Vault)
        ));
    }

    #[test]
    fn catalog_file_boundary_rejects_oversized_files() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let catalog = directory.path().join(ACP_CATALOG_FILE);
        let file = File::create(&catalog).expect("catalog file");
        file.set_len(
            u64::try_from(MAX_SIGNED_CATALOG_ENVELOPE_BYTES)
                .expect("catalog bound")
                .saturating_add(1),
        )
        .expect("oversized catalog");
        assert!(matches!(
            read_bounded_catalog(&catalog),
            Err(ManagedComponentError::Verification)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn catalog_file_boundary_rejects_symbolic_links() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temporary directory");
        let real_catalog = directory.path().join("real-catalog.json");
        std::fs::write(&real_catalog, b"{}").expect("real catalog");
        let catalog = directory.path().join(ACP_CATALOG_FILE);
        symlink(real_catalog, &catalog).expect("catalog symbolic link");
        assert!(matches!(
            read_bounded_catalog(&catalog),
            Err(ManagedComponentError::Verification)
        ));
    }

    #[tokio::test]
    async fn host_work_startup_recovery_never_replays_an_executing_effect() {
        use grok_application::{CreateRun, PrepareEffect};
        use grok_domain::{EffectKind, Idempotency, WorkExecutionBackend};

        let store = Arc::new(InMemoryExecutionStore::new());
        let clock = Arc::new(grok_memory::FixedClock::new(10));
        let ids: Arc<dyn IdGenerator> = Arc::new(grok_memory::SequentialIdGenerator::new());
        let execution: Arc<dyn ExecutionStore> = store.clone();
        let runs = RunService::new(execution.clone(), clock.clone(), ids.clone());
        let effects = SideEffectService::new(execution.clone(), clock, ids);
        let run = runs
            .create_work(
                CreateRun {
                    project_id: "project".into(),
                    thread_id: "thread".into(),
                },
                WorkExecutionBackend::HostDirect,
                "recover-host-run",
            )
            .await
            .expect("run");
        let run = runs
            .transition(
                &run.id,
                run.revision,
                RunState::Planning,
                "recover-planning",
            )
            .await
            .expect("planning");
        let run = runs
            .transition(&run.id, run.revision, RunState::Running, "recover-running")
            .await
            .expect("running");
        let effect = effects
            .prepare(PrepareEffect {
                run_id: run.id.clone(),
                kind: EffectKind::ProcessExecution,
                target: "approved command".into(),
                idempotency: Idempotency::NonIdempotent,
            })
            .await
            .expect("prepare");
        let effect = effects
            .start(&effect.id, effect.revision)
            .await
            .expect("start");

        recover_host_work(execution.as_ref(), &effects, &runs)
            .await
            .expect("recover");
        assert_eq!(
            execution
                .get_effect(&effect.id)
                .await
                .expect("effect")
                .state,
            EffectState::NeedsReview
        );
        assert_eq!(
            execution.get_run(&run.id).await.expect("run").state,
            RunState::InterruptedNeedsReview
        );
    }

    fn catalog_signature_message(key_id: &str, payload: &[u8]) -> Vec<u8> {
        let key_id_length = u16::try_from(key_id.len()).expect("bounded key ID");
        let mut message = Vec::with_capacity(
            CATALOG_SIGNATURE_DOMAIN.len() + size_of::<u16>() + key_id.len() + payload.len(),
        );
        message.extend_from_slice(CATALOG_SIGNATURE_DOMAIN);
        message.extend_from_slice(&key_id_length.to_be_bytes());
        message.extend_from_slice(key_id.as_bytes());
        message.extend_from_slice(payload);
        message
    }

    fn write_executable(path: &Path, contents: &[u8]) {
        std::fs::write(path, contents).expect("component executable");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                .expect("executable permissions");
        }
    }

    const fn component_executable_name() -> &'static str {
        if cfg!(target_os = "windows") {
            "grok.exe"
        } else {
            "grok"
        }
    }

    const fn component_operating_system() -> &'static str {
        if cfg!(target_os = "windows") {
            "windows"
        } else {
            "linux"
        }
    }

    const fn component_architecture() -> &'static str {
        if cfg!(target_arch = "aarch64") {
            "aarch64"
        } else {
            "x86_64"
        }
    }
}
