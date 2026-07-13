use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use grok_application::{
    ApprovalService, Clock, ExecutionStore, HostExecutionPolicyStore, HostFilesystemReader,
    HostFilesystemWriter, HostProcessErrorKind, HostProcessExecutor, HostProcessRequest,
    PrepareEffect, RequestApproval, SideEffectService, StoreError,
};
use grok_domain::{
    ApprovalRisk, ApprovalScope, ApprovalStatus, EffectId, EffectKind, EffectState, Idempotency,
    RequestedAction, RunId, RunState, WorkExecutionBackend,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::VerifiedHostToolsHelper;

const MAX_BRIDGE_MESSAGE_BYTES: usize = 1024 * 1024;
const BRIDGE_IO_TIMEOUT: Duration = Duration::from_secs(30);
const APPROVAL_WAIT: Duration = Duration::from_mins(15);
const APPROVAL_POLL: Duration = Duration::from_millis(100);
const DEFAULT_PROCESS_TIMEOUT: Duration = Duration::from_mins(1);

/// Daemon-owned policy, journal, approval, and OS adapter graph for one bridge.
pub struct HostToolServices {
    policies: Arc<dyn HostExecutionPolicyStore>,
    executions: Arc<dyn ExecutionStore>,
    filesystem_reader: Arc<dyn HostFilesystemReader>,
    filesystem_writer: Arc<dyn HostFilesystemWriter>,
    process_executor: Arc<dyn HostProcessExecutor>,
    approvals: Arc<ApprovalService>,
    effects: Arc<SideEffectService>,
    clock: Arc<dyn Clock>,
    process_slots: Semaphore,
}

impl HostToolServices {
    /// Composes the complete daemon-side Host Tools gate.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        policies: Arc<dyn HostExecutionPolicyStore>,
        executions: Arc<dyn ExecutionStore>,
        filesystem_reader: Arc<dyn HostFilesystemReader>,
        filesystem_writer: Arc<dyn HostFilesystemWriter>,
        process_executor: Arc<dyn HostProcessExecutor>,
        approvals: Arc<ApprovalService>,
        effects: Arc<SideEffectService>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            policies,
            executions,
            filesystem_reader,
            filesystem_writer,
            process_executor,
            approvals,
            effects,
            clock,
            process_slots: Semaphore::new(2),
        }
    }
}

/// Live per-run daemon endpoint consumed only by the packaged MCP helper.
pub struct HostToolBridge {
    endpoint: String,
    cancellation: CancellationToken,
    task: tokio::task::JoinHandle<()>,
    directory: Option<PathBuf>,
}

impl std::fmt::Debug for HostToolBridge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostToolBridge")
            .field("endpoint", &"[LOCAL ENDPOINT]")
            .finish_non_exhaustive()
    }
}

impl HostToolBridge {
    /// Starts a private local endpoint for one persisted Host-bound run.
    ///
    /// # Errors
    ///
    /// Returns a path-free unavailable error when the platform endpoint cannot
    /// be created securely.
    #[cfg(unix)]
    pub fn start(
        base: &Path,
        run_id: RunId,
        policy_revision: u64,
        helper: VerifiedHostToolsHelper,
        services: Arc<HostToolServices>,
        cancellation: CancellationToken,
    ) -> Result<Self, StoreError> {
        let directory = base.join(format!("host-tools-{}", uuid::Uuid::new_v4()));
        create_private_directory(&directory)?;
        let socket = directory.join("bridge.sock");
        let listener = tokio::net::UnixListener::bind(&socket)
            .map_err(|_| StoreError::Unavailable("Host Tools endpoint unavailable".into()))?;
        let endpoint = socket.to_string_lossy().into_owned();
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                let accepted = tokio::select! {
                    () = task_cancellation.cancelled() => break,
                    Some(_) = connections.join_next(), if !connections.is_empty() => continue,
                    accepted = listener.accept() => accepted,
                };
                let Ok((stream, _)) = accepted else { break };
                if !linux_peer_is_helper(&stream, &helper) {
                    continue;
                }
                let services = services.clone();
                let run_id = run_id.clone();
                let connection_cancellation = task_cancellation.child_token();
                connections.spawn(async move {
                    let _ = handle_connection(
                        stream,
                        &run_id,
                        policy_revision,
                        services.as_ref(),
                        connection_cancellation,
                    )
                    .await;
                });
            }
            task_cancellation.cancel();
            while connections.join_next().await.is_some() {}
        });
        Ok(Self {
            endpoint,
            cancellation,
            task,
            directory: Some(directory),
        })
    }

    /// Fails closed until the audited Windows named-pipe peer verifier is
    /// composed.
    #[cfg(not(unix))]
    pub fn start(
        _base: &Path,
        _run_id: RunId,
        _policy_revision: u64,
        _helper: VerifiedHostToolsHelper,
        _services: Arc<HostToolServices>,
        _cancellation: CancellationToken,
    ) -> Result<Self, StoreError> {
        Err(StoreError::Unavailable(
            "Host Tools platform endpoint unavailable".into(),
        ))
    }

    /// Returns the private endpoint locator passed to the packaged helper.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Closes the endpoint and waits for its accept loop to finish.
    pub async fn shutdown(mut self) {
        self.cancellation.cancel();
        let _ = (&mut self.task).await;
        if let Some(directory) = self.directory.take() {
            let _ = std::fs::remove_file(directory.join("bridge.sock"));
            let _ = std::fs::remove_dir(directory);
        }
    }
}

impl Drop for HostToolBridge {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

#[cfg(unix)]
fn create_private_directory(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700);
    builder
        .create(path)
        .map_err(|_| StoreError::Unavailable("Host Tools endpoint unavailable".into()))
}

#[cfg(target_os = "linux")]
fn linux_peer_is_helper(stream: &tokio::net::UnixStream, helper: &VerifiedHostToolsHelper) -> bool {
    let Ok(credentials) = stream.peer_cred() else {
        return false;
    };
    if credentials.uid() != rustix::process::getuid().as_raw() {
        return false;
    }
    let Some(pid) = credentials.pid().and_then(|pid| u32::try_from(pid).ok()) else {
        return false;
    };
    helper.verify_linux_peer(pid).is_ok()
}

#[cfg(all(unix, not(target_os = "linux")))]
fn linux_peer_is_helper(
    _stream: &tokio::net::UnixStream,
    _helper: &VerifiedHostToolsHelper,
) -> bool {
    false
}

async fn handle_connection<S>(
    mut stream: S,
    expected_run_id: &RunId,
    expected_policy_revision: u64,
    services: &HostToolServices,
    cancellation: CancellationToken,
) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let response = match tokio::time::timeout(BRIDGE_IO_TIMEOUT, read_line(&mut stream)).await {
        Ok(Ok(Some(bytes))) => {
            dispatch(
                &bytes,
                expected_run_id,
                expected_policy_revision,
                services,
                cancellation,
            )
            .await
        }
        _ => error_result("Host Tools request unavailable"),
    };
    let mut encoded = serde_json::to_vec(&response)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "encode"))?;
    if encoded.len() > MAX_BRIDGE_MESSAGE_BYTES {
        encoded = serde_json::to_vec(&error_result("Host Tools response exceeded its bound"))
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "encode"))?;
    }
    encoded.push(b'\n');
    tokio::time::timeout(BRIDGE_IO_TIMEOUT, stream.write_all(&encoded))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "write timeout"))??;
    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn dispatch(
    bytes: &[u8],
    expected_run_id: &RunId,
    expected_policy_revision: u64,
    services: &HostToolServices,
    cancellation: CancellationToken,
) -> Value {
    let Ok(request) = serde_json::from_slice::<Value>(bytes) else {
        return error_result("Invalid Host Tools request");
    };
    if request.get("version").and_then(Value::as_u64) != Some(1)
        || request.get("runId").and_then(Value::as_str) != Some(expected_run_id.as_str())
        || request.get("policyRevision").and_then(Value::as_u64) != Some(expected_policy_revision)
    {
        return error_result("Host Tools binding mismatch");
    }
    let Ok(policy) = services.policies.get_host_execution_policy().await else {
        return error_result("Host Tools policy unavailable");
    };
    if !policy.is_effectively_active() || policy.revision != expected_policy_revision {
        return error_result("Host Tools policy is no longer active");
    }
    let Ok(run) = services.executions.get_run(expected_run_id).await else {
        return error_result("Host Work run unavailable");
    };
    if !run.is_work_bound_to(WorkExecutionBackend::HostDirect)
        || !matches!(
            run.state,
            RunState::Planning | RunState::AwaitingApproval | RunState::Running
        )
    {
        return error_result("Host Work run is not executable");
    }
    let Some(call) = request.get("toolCall") else {
        return error_result("Host Tools call is missing");
    };
    let Some(name) = call.get("name").and_then(Value::as_str) else {
        return error_result("Host Tools call name is invalid");
    };
    let Some(call_id) = exact_call_id(call.get("callId")) else {
        return error_result("Host Tools call identity is invalid");
    };
    let arguments = call.get("arguments").unwrap_or(&Value::Null);
    match name {
        "host_filesystem_list" if policy.tool_classes.filesystem_read => {
            let Some(path) = exact_path(arguments) else {
                return error_result("Host Tools path is invalid");
            };
            match services.filesystem_reader.list(Path::new(path)).await {
                Ok(entries) => {
                    let text = serde_json::to_string(
                        &entries
                            .iter()
                            .map(|entry| {
                                json!({
                                    "name": entry.name,
                                    "type": if entry.is_directory { "directory" } else { "file" },
                                    "size": entry.size
                                })
                            })
                            .collect::<Vec<_>>(),
                    )
                    .unwrap_or_else(|_| "[]".into());
                    success_result(&text)
                }
                Err(error) => error_result(&error.message),
            }
        }
        "host_filesystem_read" if policy.tool_classes.filesystem_read => {
            let Some((path, max_bytes)) = exact_read(arguments) else {
                return error_result("Host Tools path is invalid");
            };
            match services
                .filesystem_reader
                .read_bytes(Path::new(path), max_bytes)
                .await
            {
                Ok(bytes) => match String::from_utf8(bytes) {
                    Ok(text) => success_result(&text),
                    Err(error) => success_result(
                        &serde_json::to_string(&json!({
                            "encoding": "base64",
                            "data": STANDARD.encode(error.into_bytes())
                        }))
                        .unwrap_or_else(|_| "{\"encoding\":\"unavailable\"}".into()),
                    ),
                },
                Err(error) => error_result(&error.message),
            }
        }
        "host_filesystem_write" if policy.tool_classes.filesystem_write => {
            dispatch_write(
                arguments,
                call_id,
                expected_run_id,
                expected_policy_revision,
                services,
                cancellation,
            )
            .await
        }
        "host_process_exec" if policy.tool_classes.process_execute => {
            dispatch_process(
                arguments,
                call_id,
                expected_run_id,
                expected_policy_revision,
                services,
                cancellation,
            )
            .await
        }
        _ => error_result("Host Tools operation is unavailable"),
    }
}

fn exact_call_id(value: Option<&Value>) -> Option<String> {
    let value = value?;
    let id = match value {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        _ => return None,
    };
    (!id.is_empty() && id.len() <= 128).then_some(id)
}

async fn dispatch_write(
    arguments: &Value,
    call_id: String,
    run_id: &RunId,
    policy_revision: u64,
    services: &HostToolServices,
    cancellation: CancellationToken,
) -> Value {
    let Some((path, content)) = exact_write(arguments) else {
        return error_result("Host Tools write arguments are invalid");
    };
    let digest = Sha256::digest(content.as_bytes());
    let action = RequestedAction {
        action: "host_filesystem_write".into(),
        target: path.into(),
        data_summary: format!(
            "Replace with {} UTF-8 bytes (SHA-256 {})",
            content.len(),
            hex::encode(digest)
        ),
        risk: ApprovalRisk::High,
    };
    if let Err(message) =
        await_exact_approval(run_id, &call_id, action, services, &cancellation).await
    {
        return error_result(&message);
    }
    if let Err(message) = revalidate_bound(run_id, policy_revision, services).await {
        return error_result(&message);
    }
    let effect = match prepare_effect(
        run_id,
        &call_id,
        EffectKind::FileWrite,
        path,
        Idempotency::Idempotent,
        services,
    )
    .await
    {
        Ok(effect) => effect,
        Err(message) => return error_result(&message),
    };
    let write = services
        .filesystem_writer
        .write_text(Path::new(path), content.into());
    tokio::pin!(write);
    let result = tokio::select! {
        result = &mut write => Some(result),
        () = cancellation.cancelled() => None,
    };
    match result {
        Some(Ok(())) => match services
            .effects
            .finish(&effect.id, effect.revision, true)
            .await
        {
            Ok(_) => success_result("File written"),
            Err(_) => interrupt_after_uncertain(&effect.id, effect.revision, services).await,
        },
        Some(Err(error)) => {
            if services
                .effects
                .finish(&effect.id, effect.revision, false)
                .await
                .is_err()
            {
                return interrupt_after_uncertain(&effect.id, effect.revision, services).await;
            }
            error_result(&error.message)
        }
        None => interrupt_after_uncertain(&effect.id, effect.revision, services).await,
    }
}

async fn dispatch_process(
    arguments: &Value,
    call_id: String,
    run_id: &RunId,
    policy_revision: u64,
    services: &HostToolServices,
    cancellation: CancellationToken,
) -> Value {
    let Ok(_process_slot) = services.process_slots.try_acquire() else {
        return error_result("Host process concurrency limit reached");
    };
    let Some(request) = exact_process(arguments) else {
        return error_result("Host process arguments are invalid");
    };
    let request = match services.process_executor.validate(request).await {
        Ok(request) => request,
        Err(error) => return error_result(&error.message),
    };
    let target = serde_json::to_string(&request.argv).unwrap_or_else(|_| "[invalid argv]".into());
    let action = RequestedAction {
        action: "host_process_exec".into(),
        target: target.clone(),
        data_summary: format!(
            "Run in {} with a {} ms limit; approved programs have the user's network access",
            request.cwd,
            request.timeout.as_millis()
        ),
        risk: ApprovalRisk::High,
    };
    if let Err(message) =
        await_exact_approval(run_id, &call_id, action, services, &cancellation).await
    {
        return error_result(&message);
    }
    if let Err(message) = revalidate_bound(run_id, policy_revision, services).await {
        return error_result(&message);
    }
    let effect = match prepare_effect(
        run_id,
        &call_id,
        EffectKind::ProcessExecution,
        &target,
        Idempotency::NonIdempotent,
        services,
    )
    .await
    {
        Ok(effect) => effect,
        Err(message) => return error_result(&message),
    };
    match services
        .process_executor
        .execute(request, cancellation)
        .await
    {
        Ok(output) => {
            let succeeded = output.exit_code == Some(0);
            if services
                .effects
                .finish(&effect.id, effect.revision, succeeded)
                .await
                .is_err()
            {
                return interrupt_after_uncertain(&effect.id, effect.revision, services).await;
            }
            success_result(
                &serde_json::to_string(&json!({
                    "exitCode": output.exit_code,
                    "stdout": output.stdout,
                    "stderr": output.stderr,
                    "truncated": output.truncated
                }))
                .unwrap_or_else(|_| "{\"error\":\"output unavailable\"}".into()),
            )
        }
        Err(error)
            if matches!(
                error.kind,
                HostProcessErrorKind::Interrupted | HostProcessErrorKind::TimedOut
            ) =>
        {
            interrupt_after_uncertain(&effect.id, effect.revision, services).await
        }
        Err(error) => {
            if services
                .effects
                .finish(&effect.id, effect.revision, false)
                .await
                .is_err()
            {
                return interrupt_after_uncertain(&effect.id, effect.revision, services).await;
            }
            error_result(&error.message)
        }
    }
}

async fn await_exact_approval(
    run_id: &RunId,
    call_id: &str,
    action: RequestedAction,
    services: &HostToolServices,
    cancellation: &CancellationToken,
) -> Result<(), String> {
    let run = services
        .executions
        .get_run(run_id)
        .await
        .map_err(|_| "Host Work run unavailable".to_string())?;
    if run.state != RunState::Running {
        return Err("Host Work run is not ready for approval".into());
    }
    let expires_at = services
        .clock
        .now()
        .saturating_add(u64::try_from(APPROVAL_WAIT.as_millis()).unwrap_or(u64::MAX));
    let key = bridge_key(run_id, call_id, "approval");
    let approval = services
        .approvals
        .request(
            RequestApproval {
                run_id: run_id.clone(),
                expected_run_revision: run.revision,
                action,
                scope: ApprovalScope::Once,
                expires_at,
            },
            &key,
        )
        .await
        .map_err(|error| error.to_string())?;
    let deadline = tokio::time::Instant::now() + APPROVAL_WAIT;
    loop {
        let current = services
            .executions
            .get_approval(&approval.id)
            .await
            .map_err(|_| "Host Tools approval unavailable".to_string())?;
        match current.status {
            ApprovalStatus::Granted => return Ok(()),
            ApprovalStatus::Denied => return Err("Host Tools action was denied".into()),
            ApprovalStatus::Expired => return Err("Host Tools approval expired".into()),
            ApprovalStatus::Cancelled => return Err("Host Tools approval was cancelled".into()),
            ApprovalStatus::Pending => {}
        }
        tokio::select! {
            () = cancellation.cancelled() => {
                close_pending_approval(&approval.id, current.revision, services, &key).await;
                return Err("Host Tools action was cancelled".into());
            },
            () = tokio::time::sleep_until(deadline) => {
                close_pending_approval(&approval.id, current.revision, services, &key).await;
                return Err("Host Tools approval expired".into());
            },
            () = tokio::time::sleep(APPROVAL_POLL) => {}
        }
    }
}

async fn revalidate_bound(
    run_id: &RunId,
    policy_revision: u64,
    services: &HostToolServices,
) -> Result<(), String> {
    let policy = services
        .policies
        .get_host_execution_policy()
        .await
        .map_err(|_| "Host Tools policy unavailable".to_string())?;
    if !policy.is_effectively_active() || policy.revision != policy_revision {
        return Err("Host Tools policy is no longer active".into());
    }
    let run = services
        .executions
        .get_run(run_id)
        .await
        .map_err(|_| "Host Work run unavailable".to_string())?;
    if !run.is_work_bound_to(WorkExecutionBackend::HostDirect) || run.state != RunState::Running {
        return Err("Host Work run is not executable".into());
    }
    Ok(())
}

async fn close_pending_approval(
    approval_id: &grok_domain::ApprovalId,
    revision: u64,
    services: &HostToolServices,
    base_key: &str,
) {
    let _ = services
        .approvals
        .decide(
            approval_id,
            revision,
            grok_domain::ApprovalDecision::Deny,
            &format!("{base_key}-closed"),
        )
        .await;
}

async fn prepare_effect(
    run_id: &RunId,
    call_id: &str,
    kind: EffectKind,
    target: &str,
    idempotency: Idempotency,
    services: &HostToolServices,
) -> Result<grok_domain::SideEffect, String> {
    let id =
        EffectId::new(bridge_key(run_id, call_id, "effect")).map_err(|error| error.to_string())?;
    let input = PrepareEffect {
        run_id: run_id.clone(),
        kind,
        target: target.into(),
        idempotency,
    };
    match services
        .effects
        .prepare_with_id(input.clone(), id.clone())
        .await
    {
        Ok(effect) => services
            .effects
            .start(&effect.id, effect.revision)
            .await
            .map_err(|error| error.to_string()),
        Err(grok_application::ApplicationError::Conflict) => {
            let existing = services
                .executions
                .get_effect(&id)
                .await
                .map_err(|_| "Host Tools effect replay is unavailable".to_string())?;
            if existing.run_id != input.run_id
                || existing.kind != input.kind
                || existing.target != input.target
                || existing.idempotency != input.idempotency
            {
                return Err("Host Tools effect identity conflict".into());
            }
            match existing.state {
                EffectState::Prepared => services
                    .effects
                    .start(&existing.id, existing.revision)
                    .await
                    .map_err(|error| error.to_string()),
                EffectState::Executing => {
                    let _ = services
                        .effects
                        .interrupt(&existing.id, existing.revision)
                        .await;
                    Err("Host Tools effect was interrupted and needs review".into())
                }
                EffectState::Succeeded | EffectState::Failed => {
                    Err("Host Tools effect was already dispatched".into())
                }
                EffectState::NeedsReview => {
                    Err("Host Tools effect needs review before continuing".into())
                }
            }
        }
        Err(error) => Err(error.to_string()),
    }
}

async fn interrupt_after_uncertain(
    effect_id: &EffectId,
    expected_revision: u64,
    services: &HostToolServices,
) -> Value {
    let revision = match services.executions.get_effect(effect_id).await {
        Ok(effect) if effect.state == EffectState::Executing => effect.revision,
        _ => expected_revision,
    };
    let _ = services.effects.interrupt(effect_id, revision).await;
    error_result("Host Tools effect was interrupted and needs review")
}

fn bridge_key(run_id: &RunId, call_id: &str, stage: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"grok-host-tool-call-v1\0");
    digest.update(run_id.as_str().as_bytes());
    digest.update([0]);
    digest.update(call_id.as_bytes());
    digest.update([0]);
    digest.update(stage.as_bytes());
    format!("host-tool-{}", hex::encode(digest.finalize()))
}

fn exact_write(arguments: &Value) -> Option<(&str, &str)> {
    let object = arguments.as_object()?;
    if object.len() != 2 {
        return None;
    }
    let path = object
        .get("path")?
        .as_str()
        .filter(|path| !path.is_empty() && path.len() <= 4096)?;
    let content = object
        .get("content")?
        .as_str()
        .filter(|content| content.len() <= grok_host_tools::MAX_WRITE_BYTES)?;
    Some((path, content))
}

fn exact_process(arguments: &Value) -> Option<HostProcessRequest> {
    let object = arguments.as_object()?;
    if !(2..=3).contains(&object.len())
        || object
            .keys()
            .any(|key| !matches!(key.as_str(), "argv" | "cwd" | "timeoutMs"))
    {
        return None;
    }
    let argv = object
        .get("argv")?
        .as_array()?
        .iter()
        .map(|value| value.as_str().map(str::to_owned))
        .collect::<Option<Vec<_>>>()?;
    let cwd = object.get("cwd")?.as_str()?.to_owned();
    let timeout = match object.get("timeoutMs") {
        Some(value) => Duration::from_millis(value.as_u64()?),
        None => DEFAULT_PROCESS_TIMEOUT,
    };
    Some(HostProcessRequest { argv, cwd, timeout })
}

fn exact_path(arguments: &Value) -> Option<&str> {
    let object = arguments.as_object()?;
    if object.len() != 1 {
        return None;
    }
    object
        .get("path")?
        .as_str()
        .filter(|path| !path.is_empty() && path.len() <= 4096)
}

fn exact_read(arguments: &Value) -> Option<(&str, u64)> {
    let object = arguments.as_object()?;
    if !(1..=2).contains(&object.len())
        || object
            .keys()
            .any(|key| !matches!(key.as_str(), "path" | "maxBytes"))
    {
        return None;
    }
    let path = object
        .get("path")?
        .as_str()
        .filter(|path| !path.is_empty() && path.len() <= 4096)?;
    let max_bytes = object
        .get("maxBytes")
        .map_or(Some(grok_host_tools::MAX_READ_BYTES), Value::as_u64)?;
    (max_bytes > 0 && max_bytes <= grok_host_tools::MAX_READ_HARD_BYTES)
        .then_some((path, max_bytes))
}

fn success_result(text: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": false })
}

fn error_result(message: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": message }], "isError": true })
}

async fn read_line<R>(reader: &mut R) -> std::io::Result<Option<Vec<u8>>>
where
    R: AsyncRead + Unpin,
{
    let mut result = Vec::with_capacity(4096);
    let mut byte = [0_u8; 1];
    loop {
        let read = reader.read(&mut byte).await?;
        if read == 0 {
            return if result.is_empty() {
                Ok(None)
            } else {
                Ok(Some(result))
            };
        }
        if byte[0] == b'\n' {
            return Ok(Some(result));
        }
        if result.len() == MAX_BRIDGE_MESSAGE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "oversize request",
            ));
        }
        result.push(byte[0]);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use grok_application::{
        CreateRun, HostExecutionPolicyStore, HostFilesystemWriter, IdGenerator, MutationCommand,
        RunService,
    };
    use grok_domain::{
        ApprovalDecision, HOST_ACKNOWLEDGMENT_VERSION, HostExecutionPolicy, HostToolClasses,
        RunEventKind, RunState, WorkExecutionBackend,
    };
    use grok_host_tools::CapabilityHostFilesystem;
    use grok_memory::{FixedClock, InMemoryExecutionStore, SequentialIdGenerator};

    use super::*;

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn bridge_revalidates_run_and_policy_before_each_read() {
        let root = tempfile::tempdir().expect("root");
        let endpoints = tempfile::tempdir().expect("endpoints");
        std::fs::write(root.path().join("note.txt"), "hello from host").expect("note");
        let policy = HostExecutionPolicy {
            revision: 1,
            active: true,
            acknowledgment_version: HOST_ACKNOWLEDGMENT_VERSION,
            acknowledged_at: 1,
            tool_classes: HostToolClasses {
                filesystem_read: true,
                filesystem_write: true,
                process_execute: true,
            },
            canonical_roots: vec![root.path().to_string_lossy().into_owned()],
            broad_scope_acknowledged: false,
            updated_at: 1,
        };
        let store = Arc::new(InMemoryExecutionStore::new());
        let command = MutationCommand {
            scope: "enroll_host_execution_v1".into(),
            key: "bridge-policy".into(),
            fingerprint: [7; 32],
        };
        store
            .replace_host_execution_policy(policy.clone(), 0, &command)
            .await
            .expect("policy");
        let clock = Arc::new(FixedClock::new(1));
        let ids: Arc<dyn IdGenerator> = Arc::new(SequentialIdGenerator::new());
        let execution: Arc<dyn ExecutionStore> = store.clone();
        let runs = RunService::new(execution.clone(), clock.clone(), ids.clone());
        let run = runs
            .create_work(
                CreateRun {
                    project_id: "project".into(),
                    thread_id: "thread".into(),
                },
                WorkExecutionBackend::HostDirect,
                "bridge-run",
            )
            .await
            .expect("run");
        let run = runs
            .transition(&run.id, 0, RunState::Planning, "bridge-planning")
            .await
            .expect("planning");
        let run = runs
            .transition(&run.id, run.revision, RunState::Running, "bridge-running")
            .await
            .expect("running");
        let filesystem =
            Arc::new(CapabilityHostFilesystem::open(&policy.canonical_roots).expect("filesystem"));
        let reader: Arc<dyn HostFilesystemReader> = filesystem.clone();
        let writer: Arc<dyn HostFilesystemWriter> = filesystem.clone();
        let process: Arc<dyn HostProcessExecutor> = filesystem;
        let helper =
            VerifiedHostToolsHelper::verify(std::env::current_exe().expect("test executable"))
                .expect("helper identity");
        let approvals = Arc::new(ApprovalService::new(
            execution.clone(),
            clock.clone(),
            ids.clone(),
        ));
        let bridge = HostToolBridge::start(
            endpoints.path(),
            run.id.clone(),
            policy.revision,
            helper,
            Arc::new(HostToolServices::new(
                store.clone(),
                execution.clone(),
                reader,
                writer,
                process,
                approvals.clone(),
                Arc::new(SideEffectService::new(execution, clock.clone(), ids)),
                clock,
            )),
            CancellationToken::new(),
        )
        .expect("bridge");

        let first = call(
            bridge.endpoint().to_owned(),
            json!({
                "version": 1,
                "runId": run.id.as_str(),
                "policyRevision": 1,
                "toolCall": {
                    "callId": "read-1",
                    "name": "host_filesystem_read",
                    "arguments": { "path": root.path().join("note.txt").to_string_lossy() }
                }
            }),
        )
        .await;
        assert_eq!(first["isError"], false);
        assert_eq!(first["content"][0]["text"], "hello from host");

        let endpoint = bridge.endpoint().to_owned();
        let write_path = root.path().join("written.txt");
        let write_call = tokio::spawn(call(
            endpoint,
            json!({
                "version": 1,
                "runId": run.id.as_str(),
                "policyRevision": 1,
                "toolCall": {
                    "callId": "write-1",
                    "name": "host_filesystem_write",
                    "arguments": {
                        "path": write_path.to_string_lossy(),
                        "content": "approved content"
                    }
                }
            }),
        ));
        grant_next_approval(store.as_ref(), approvals.as_ref(), &run.id).await;
        let write = write_call.await.expect("write task");
        assert_eq!(write["isError"], false, "{write}");
        assert_eq!(
            std::fs::read_to_string(&write_path).expect("written file"),
            "approved content"
        );

        let endpoint = bridge.endpoint().to_owned();
        let process_call = tokio::spawn(call(
            endpoint,
            json!({
                "version": 1,
                "runId": run.id.as_str(),
                "policyRevision": 1,
                "toolCall": {
                    "callId": "process-1",
                    "name": "host_process_exec",
                    "arguments": {
                        "argv": ["printf", "approved process"],
                        "cwd": root.path().to_string_lossy(),
                        "timeoutMs": 5000
                    }
                }
            }),
        ));
        grant_next_approval(store.as_ref(), approvals.as_ref(), &run.id).await;
        let process = process_call.await.expect("process task");
        assert_eq!(process["isError"], false, "{process}");
        assert!(
            process["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("approved process")),
            "{process}"
        );

        let revoked = HostExecutionPolicy {
            revision: 2,
            active: false,
            updated_at: 2,
            ..policy
        };
        store
            .replace_host_execution_policy(
                revoked,
                1,
                &MutationCommand {
                    scope: "revoke_host_execution_v1".into(),
                    key: "bridge-revoke".into(),
                    fingerprint: [8; 32],
                },
            )
            .await
            .expect("revoke");
        let denied = call(
            bridge.endpoint().to_owned(),
            json!({
                "version": 1,
                "runId": run.id.as_str(),
                "policyRevision": 1,
                "toolCall": {
                    "callId": "read-2",
                    "name": "host_filesystem_read",
                    "arguments": { "path": root.path().join("note.txt").to_string_lossy() }
                }
            }),
        )
        .await;
        assert_eq!(denied["isError"], true);
        bridge.shutdown().await;
    }

    async fn grant_next_approval(
        store: &InMemoryExecutionStore,
        approvals: &ApprovalService,
        run_id: &RunId,
    ) {
        let approval = loop {
            let events = store.events_since(run_id, 0, 100).await.expect("events");
            let pending = events.iter().rev().find_map(|event| match &event.kind {
                RunEventKind::ApprovalRequested { approval_id } => Some(approval_id.clone()),
                _ => None,
            });
            if let Some(id) = pending {
                let approval = store.get_approval(&id).await.expect("approval");
                if approval.status == ApprovalStatus::Pending {
                    break approval;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        approvals
            .decide(
                &approval.id,
                approval.revision,
                ApprovalDecision::Grant,
                &format!("grant-{}", approval.id),
            )
            .await
            .expect("grant");
    }

    async fn call(endpoint: String, request: Value) -> Value {
        let mut stream = tokio::net::UnixStream::connect(endpoint)
            .await
            .expect("connect");
        let mut bytes = serde_json::to_vec(&request).expect("encode");
        bytes.push(b'\n');
        stream.write_all(&bytes).await.expect("write");
        let response = read_line(&mut stream)
            .await
            .expect("read")
            .expect("response");
        serde_json::from_slice(&response).expect("JSON response")
    }
}
