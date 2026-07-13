use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use grok_application::{
    ExecutionStore, HostExecutionPolicyStore, HostFilesystemReader, StoreError,
};
use grok_domain::{RunId, RunState, WorkExecutionBackend};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::VerifiedHostToolsHelper;

const MAX_BRIDGE_MESSAGE_BYTES: usize = 1024 * 1024;
const BRIDGE_IO_TIMEOUT: Duration = Duration::from_secs(30);

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
        policies: Arc<dyn HostExecutionPolicyStore>,
        executions: Arc<dyn ExecutionStore>,
        filesystem: Arc<dyn HostFilesystemReader>,
    ) -> Result<Self, StoreError> {
        let directory = base.join(format!("host-tools-{}", uuid::Uuid::new_v4()));
        create_private_directory(&directory)?;
        let socket = directory.join("bridge.sock");
        let listener = tokio::net::UnixListener::bind(&socket)
            .map_err(|_| StoreError::Unavailable("Host Tools endpoint unavailable".into()))?;
        let endpoint = socket.to_string_lossy().into_owned();
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    () = task_cancellation.cancelled() => break,
                    accepted = listener.accept() => accepted,
                };
                let Ok((stream, _)) = accepted else { break };
                if !linux_peer_is_helper(&stream, &helper) {
                    continue;
                }
                let policies = policies.clone();
                let executions = executions.clone();
                let filesystem = filesystem.clone();
                let run_id = run_id.clone();
                tokio::spawn(async move {
                    let _ = handle_connection(
                        stream,
                        &run_id,
                        policy_revision,
                        policies.as_ref(),
                        executions.as_ref(),
                        filesystem.as_ref(),
                    )
                    .await;
                });
            }
        });
        Ok(Self {
            endpoint,
            cancellation,
            task,
            directory: Some(directory),
        })
    }

    /// Returns the private endpoint locator passed to the packaged helper.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Closes the endpoint and waits for its accept loop to finish.
    pub async fn shutdown(self) {
        self.cancellation.cancel();
        let _ = self.task.await;
        if let Some(directory) = self.directory {
            let _ = std::fs::remove_file(directory.join("bridge.sock"));
            let _ = std::fs::remove_dir(directory);
        }
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
    policies: &dyn HostExecutionPolicyStore,
    executions: &dyn ExecutionStore,
    filesystem: &dyn HostFilesystemReader,
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
                policies,
                executions,
                filesystem,
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

async fn dispatch(
    bytes: &[u8],
    expected_run_id: &RunId,
    expected_policy_revision: u64,
    policies: &dyn HostExecutionPolicyStore,
    executions: &dyn ExecutionStore,
    filesystem: &dyn HostFilesystemReader,
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
    let Ok(policy) = policies.get_host_execution_policy().await else {
        return error_result("Host Tools policy unavailable");
    };
    if !policy.is_effectively_active() || policy.revision != expected_policy_revision {
        return error_result("Host Tools policy is no longer active");
    }
    let Ok(run) = executions.get_run(expected_run_id).await else {
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
    let arguments = call.get("arguments").unwrap_or(&Value::Null);
    match name {
        "host_filesystem_list" if policy.tool_classes.filesystem_read => {
            let Some(path) = exact_path(arguments) else {
                return error_result("Host Tools path is invalid");
            };
            match filesystem.list(Path::new(path)).await {
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
            let Some(path) = exact_path(arguments) else {
                return error_result("Host Tools path is invalid");
            };
            match filesystem.read_text(Path::new(path)).await {
                Ok(text) => success_result(&text),
                Err(error) => error_result(&error.message),
            }
        }
        "host_filesystem_write" | "host_process_exec" => {
            error_result("Host Tools operation requires an approval implementation")
        }
        _ => error_result("Host Tools operation is unavailable"),
    }
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
        CreateRun, HostExecutionPolicyStore, IdGenerator, MutationCommand, RunService,
    };
    use grok_domain::{
        HOST_ACKNOWLEDGMENT_VERSION, HostExecutionPolicy, HostToolClasses, RunState,
        WorkExecutionBackend,
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
                filesystem_write: false,
                process_execute: false,
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
        let runs = RunService::new(execution.clone(), clock.clone(), ids);
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
        let filesystem: Arc<dyn HostFilesystemReader> =
            Arc::new(CapabilityHostFilesystem::open(&policy.canonical_roots).expect("filesystem"));
        let helper =
            VerifiedHostToolsHelper::verify(std::env::current_exe().expect("test executable"))
                .expect("helper identity");
        let bridge = HostToolBridge::start(
            endpoints.path(),
            run.id.clone(),
            policy.revision,
            helper,
            store.clone(),
            execution,
            filesystem,
        )
        .expect("bridge");

        let first = call(
            bridge.endpoint(),
            json!({
                "version": 1,
                "runId": run.id.as_str(),
                "policyRevision": 1,
                "toolCall": {
                    "name": "host_filesystem_read",
                    "arguments": { "path": root.path().join("note.txt").to_string_lossy() }
                }
            }),
        )
        .await;
        assert_eq!(first["isError"], false);
        assert_eq!(first["content"][0]["text"], "hello from host");

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
            bridge.endpoint(),
            json!({
                "version": 1,
                "runId": run.id.as_str(),
                "policyRevision": 1,
                "toolCall": {
                    "name": "host_filesystem_read",
                    "arguments": { "path": root.path().join("note.txt").to_string_lossy() }
                }
            }),
        )
        .await;
        assert_eq!(denied["isError"], true);
        bridge.shutdown().await;
    }

    async fn call(endpoint: &str, request: Value) -> Value {
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
