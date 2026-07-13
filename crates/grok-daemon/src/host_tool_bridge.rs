use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
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
use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::VerifiedHostToolsHelper;

const MAX_BRIDGE_MESSAGE_BYTES: usize = 1024 * 1024;
const BRIDGE_IO_TIMEOUT: Duration = Duration::from_secs(30);
const APPROVAL_WAIT: Duration = Duration::from_mins(15);
const APPROVAL_POLL: Duration = Duration::from_millis(100);
const DEFAULT_PROCESS_TIMEOUT: Duration = Duration::from_mins(1);
const MAX_HTTP_HEADER_BYTES: usize = 16 * 1024;
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

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
    authorization: Option<String>,
    initialized: Arc<AtomicBool>,
    initialized_notify: Arc<Notify>,
    cancellation: CancellationToken,
    task: tokio::task::JoinHandle<()>,
    socket_path: Option<PathBuf>,
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
    /// Starts an authenticated, per-run Streamable HTTP MCP endpoint bound
    /// exclusively to IPv4 loopback.
    ///
    /// # Errors
    ///
    /// Returns a sanitized unavailable error when the loopback listener cannot
    /// be created.
    pub async fn start_http(
        run_id: RunId,
        policy_revision: u64,
        services: Arc<HostToolServices>,
        cancellation: CancellationToken,
    ) -> Result<Self, StoreError> {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| StoreError::Unavailable("Host Tools endpoint unavailable".into()))?;
        let address = listener
            .local_addr()
            .map_err(|_| StoreError::Unavailable("Host Tools endpoint unavailable".into()))?;
        let authority = format!("127.0.0.1:{}", address.port());
        let endpoint = format!("http://{authority}/mcp");
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let authorization = format!("Bearer {token}");
        let task_cancellation = cancellation.clone();
        let initialized = Arc::new(AtomicBool::new(false));
        let initialized_notify = Arc::new(Notify::new());
        let task_initialized = initialized.clone();
        let task_notify = initialized_notify.clone();
        let task_authorization = authorization.clone();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                let accepted = tokio::select! {
                    () = task_cancellation.cancelled() => break,
                    Some(_) = connections.join_next(), if !connections.is_empty() => continue,
                    accepted = listener.accept() => accepted,
                };
                let Ok((stream, peer)) = accepted else { break };
                if !peer.ip().is_loopback() {
                    continue;
                }
                let services = services.clone();
                let run_id = run_id.clone();
                let authorization = task_authorization.clone();
                let authority = authority.clone();
                let initialized = task_initialized.clone();
                let notify = task_notify.clone();
                let connection_cancellation = task_cancellation.child_token();
                connections.spawn(async move {
                    let _ = handle_http_connection(
                        stream,
                        &authority,
                        &authorization,
                        &run_id,
                        policy_revision,
                        services.as_ref(),
                        initialized,
                        notify,
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
            authorization: Some(authorization),
            initialized,
            initialized_notify,
            cancellation,
            task,
            socket_path: None,
        })
    }

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
        use std::os::unix::fs::PermissionsExt;

        let socket = next_unix_socket_path(base)?;
        let listener = tokio::net::UnixListener::bind(&socket).map_err(|_| {
            let _ = std::fs::remove_file(&socket);
            StoreError::Unavailable("Host Tools endpoint unavailable".into())
        })?;
        if std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).is_err() {
            let _ = std::fs::remove_file(&socket);
            return Err(StoreError::Unavailable(
                "Host Tools endpoint unavailable".into(),
            ));
        }
        let endpoint = socket.to_string_lossy().into_owned();
        let task_cancellation = cancellation.clone();
        let initialized = Arc::new(AtomicBool::new(false));
        let initialized_notify = Arc::new(Notify::new());
        let task_initialized = initialized.clone();
        let task_notify = initialized_notify.clone();
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
                let initialized = task_initialized.clone();
                let initialized_notify = task_notify.clone();
                let connection_cancellation = task_cancellation.child_token();
                connections.spawn(async move {
                    let _ = handle_connection(
                        stream,
                        &run_id,
                        policy_revision,
                        services.as_ref(),
                        initialized,
                        initialized_notify,
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
            authorization: None,
            initialized,
            initialized_notify,
            cancellation,
            task,
            socket_path: Some(socket),
        })
    }

    /// Starts an owner-only Windows named pipe and authenticates every client
    /// against the retained packaged helper identity.
    #[cfg(windows)]
    pub fn start(
        _base: &Path,
        run_id: RunId,
        policy_revision: u64,
        helper: VerifiedHostToolsHelper,
        services: Arc<HostToolServices>,
        cancellation: CancellationToken,
    ) -> Result<Self, StoreError> {
        let endpoint = format!(r"\\.\pipe\grok-desktop-host-tools-{}", uuid::Uuid::new_v4());
        let listener = grok_windows_acl::create_private_named_pipe_server(&endpoint, true)
            .map_err(|_| StoreError::Unavailable("Host Tools endpoint unavailable".into()))?;
        let task_cancellation = cancellation.clone();
        let endpoint_for_task = endpoint.clone();
        let initialized = Arc::new(AtomicBool::new(false));
        let initialized_notify = Arc::new(Notify::new());
        let task_initialized = initialized.clone();
        let task_notify = initialized_notify.clone();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            let mut listener = listener;
            loop {
                let connected = tokio::select! {
                    () = task_cancellation.cancelled() => break,
                    Some(_) = connections.join_next(), if !connections.is_empty() => continue,
                    connected = listener.connect() => connected,
                };
                if connected.is_err() {
                    break;
                }
                let next = match grok_windows_acl::create_private_named_pipe_server(
                    &endpoint_for_task,
                    false,
                ) {
                    Ok(next) => next,
                    Err(_) => break,
                };
                if helper.reverify().is_err() {
                    let _ = listener.disconnect();
                    listener = next;
                    continue;
                }
                let Ok(peer) =
                    grok_windows_acl::verify_named_pipe_client_executable(&listener, helper.path())
                else {
                    let _ = listener.disconnect();
                    listener = next;
                    continue;
                };
                let services = services.clone();
                let run_id = run_id.clone();
                let initialized = task_initialized.clone();
                let initialized_notify = task_notify.clone();
                let connection_cancellation = task_cancellation.child_token();
                connections.spawn(async move {
                    let _peer = peer;
                    let _ = handle_connection(
                        listener,
                        &run_id,
                        policy_revision,
                        services.as_ref(),
                        initialized,
                        initialized_notify,
                        connection_cancellation,
                    )
                    .await;
                });
                listener = next;
            }
            task_cancellation.cancel();
            while connections.join_next().await.is_some() {}
        });
        Ok(Self {
            endpoint,
            authorization: None,
            initialized,
            initialized_notify,
            cancellation,
            task,
            socket_path: None,
        })
    }

    /// Fails closed on unsupported platforms.
    #[cfg(not(any(unix, windows)))]
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

    /// Returns the per-run authorization header value for an HTTP bridge.
    #[must_use]
    pub fn authorization(&self) -> Option<&str> {
        self.authorization.as_deref()
    }

    /// Waits until the official agent completes MCP initialization.
    pub async fn wait_until_initialized(&self, timeout: Duration) -> bool {
        if self.initialized.load(Ordering::Acquire) {
            return true;
        }
        let notified = self.initialized_notify.notified();
        if self.initialized.load(Ordering::Acquire) {
            return true;
        }
        tokio::time::timeout(timeout, notified).await.is_ok()
            && self.initialized.load(Ordering::Acquire)
    }

    /// Closes the endpoint and waits for its accept loop to finish.
    pub async fn shutdown(mut self) {
        self.cancellation.cancel();
        let _ = (&mut self.task).await;
        if let Some(socket_path) = self.socket_path.take() {
            let _ = std::fs::remove_file(socket_path);
        }
    }
}

impl Drop for HostToolBridge {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(socket_path) = self.socket_path.take() {
            let _ = std::fs::remove_file(socket_path);
        }
    }
}

#[cfg(unix)]
fn next_unix_socket_path(base: &Path) -> Result<PathBuf, StoreError> {
    use std::os::unix::ffi::OsStrExt;

    const MAX_PORTABLE_UNIX_SOCKET_PATH_BYTES: usize = 100;

    let socket = base.join(format!("ht-{}.sock", uuid::Uuid::new_v4().simple()));
    if socket.as_os_str().as_bytes().len() > MAX_PORTABLE_UNIX_SOCKET_PATH_BYTES {
        return Err(StoreError::Unavailable(
            "Host Tools endpoint unavailable".into(),
        ));
    }
    Ok(socket)
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

#[allow(clippy::too_many_arguments)]
async fn handle_http_connection(
    mut stream: tokio::net::TcpStream,
    expected_authority: &str,
    expected_authorization: &str,
    expected_run_id: &RunId,
    expected_policy_revision: u64,
    services: &HostToolServices,
    initialized: Arc<AtomicBool>,
    initialized_notify: Arc<Notify>,
    cancellation: CancellationToken,
) -> std::io::Result<()> {
    let request = tokio::time::timeout(BRIDGE_IO_TIMEOUT, read_http_request(&mut stream))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "read timeout"))??;
    let response = match request {
        Some(request)
            if request.method == "POST"
                && request.path == "/mcp"
                && request.host == expected_authority
                && request.authorization == expected_authorization
                && request.content_type.starts_with("application/json")
                && request.origin.as_deref().is_none_or(loopback_origin) =>
        {
            mcp_response(
                &request.body,
                expected_run_id,
                expected_policy_revision,
                services,
                initialized,
                initialized_notify,
                cancellation,
            )
            .await
        }
        Some(_) => HttpResponse::json(
            403,
            &json!({"jsonrpc":"2.0","id":Value::Null,"error":{"code":-32001,"message":"Host Tools request denied"}}),
        ),
        None => HttpResponse::empty(400),
    };
    write_http_response(&mut stream, response).await
}

struct HttpRequest {
    method: String,
    path: String,
    host: String,
    authorization: String,
    content_type: String,
    origin: Option<String>,
    body: Vec<u8>,
}

struct HttpResponse {
    status: u16,
    body: Vec<u8>,
}

impl HttpResponse {
    fn json(status: u16, value: &Value) -> Self {
        Self {
            status,
            body: serde_json::to_vec(value).unwrap_or_default(),
        }
    }

    const fn empty(status: u16) -> Self {
        Self {
            status,
            body: Vec::new(),
        }
    }
}

async fn read_http_request(
    stream: &mut tokio::net::TcpStream,
) -> std::io::Result<Option<HttpRequest>> {
    let mut bytes = Vec::with_capacity(4096);
    let header_end = loop {
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        if bytes.len() >= MAX_HTTP_HEADER_BYTES {
            return Ok(None);
        }
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(None);
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > MAX_HTTP_HEADER_BYTES + MAX_BRIDGE_MESSAGE_BYTES {
            return Ok(None);
        }
    };
    if header_end > MAX_HTTP_HEADER_BYTES {
        return Ok(None);
    }
    let Ok(headers) = std::str::from_utf8(&bytes[..header_end]) else {
        return Ok(None);
    };
    let mut lines = headers[..headers.len().saturating_sub(4)].split("\r\n");
    let Some(request_line) = lines.next() else {
        return Ok(None);
    };
    let parts = request_line.split_whitespace().collect::<Vec<_>>();
    if parts.len() != 3 || parts[2] != "HTTP/1.1" {
        return Ok(None);
    }
    let method = parts[0].to_owned();
    let path = parts[1].to_owned();
    let mut host = None;
    let mut authorization = None;
    let mut content_type = None;
    let mut content_length = None;
    let mut origin = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Ok(None);
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("host") {
            host = Some(value.to_owned());
        } else if name.eq_ignore_ascii_case("authorization") {
            authorization = Some(value.to_owned());
        } else if name.eq_ignore_ascii_case("content-type") {
            content_type = Some(value.to_ascii_lowercase());
        } else if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse::<usize>().ok();
        } else if name.eq_ignore_ascii_case("origin") {
            origin = Some(value.to_owned());
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Ok(None);
        }
    }
    let Some(content_length) = content_length.filter(|length| *length <= MAX_BRIDGE_MESSAGE_BYTES)
    else {
        return Ok(None);
    };
    let total = header_end.saturating_add(content_length);
    while bytes.len() < total {
        let remaining = total - bytes.len();
        let mut chunk = vec![0_u8; remaining.min(4096)];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(None);
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    Ok(Some(HttpRequest {
        method,
        path,
        host: host.unwrap_or_default(),
        authorization: authorization.unwrap_or_default(),
        content_type: content_type.unwrap_or_default(),
        origin,
        body: bytes[header_end..total].to_vec(),
    }))
}

fn loopback_origin(origin: &str) -> bool {
    origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .and_then(|authority| authority.split('/').next())
        .and_then(|authority| authority.split(':').next())
        .is_some_and(|host| matches!(host, "127.0.0.1" | "localhost" | "[::1]"))
}

#[allow(clippy::too_many_arguments)]
async fn mcp_response(
    bytes: &[u8],
    expected_run_id: &RunId,
    expected_policy_revision: u64,
    services: &HostToolServices,
    initialized: Arc<AtomicBool>,
    initialized_notify: Arc<Notify>,
    cancellation: CancellationToken,
) -> HttpResponse {
    let Ok(request) = serde_json::from_slice::<Value>(bytes) else {
        return mcp_error(&Value::Null, -32700, "invalid JSON");
    };
    let id = request.get("id").cloned();
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return id.map_or_else(
            || HttpResponse::empty(202),
            |id| mcp_error(&id, -32600, "invalid request"),
        );
    };
    let result = match method {
        "initialize" => {
            initialized.store(true, Ordering::Release);
            initialized_notify.notify_waiters();
            Ok(json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "grok-desktop-host-tools", "version": env!("CARGO_PKG_VERSION") }
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tool_catalog()),
        "tools/call" => {
            let Some(call_id) = id.as_ref() else {
                return mcp_error(&Value::Null, -32600, "tool call id is required");
            };
            let Some(parameters) = request.get("params") else {
                return mcp_error(call_id, -32602, "invalid tool arguments");
            };
            let bridge_request = json!({
                "version": 1,
                "runId": expected_run_id.to_string(),
                "policyRevision": expected_policy_revision,
                "toolCall": {
                    "callId": call_id,
                    "name": parameters.get("name"),
                    "arguments": parameters.get("arguments")
                }
            });
            Ok(dispatch(
                &serde_json::to_vec(&bridge_request).unwrap_or_default(),
                expected_run_id,
                expected_policy_revision,
                services,
                cancellation,
            )
            .await)
        }
        method if method.starts_with("notifications/") => return HttpResponse::empty(202),
        _ => Err((-32601, "method not found")),
    };
    let Some(id) = id else {
        return HttpResponse::empty(202);
    };
    match result {
        Ok(result) => HttpResponse::json(200, &json!({"jsonrpc":"2.0","id":id,"result":result})),
        Err((code, message)) => mcp_error(&id, code, message),
    }
}

fn mcp_error(id: &Value, code: i64, message: &str) -> HttpResponse {
    HttpResponse::json(
        200,
        &json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}),
    )
}

fn tool_catalog() -> Value {
    json!({ "tools": [
        {
            "name": "host_filesystem_list",
            "description": "List one enrolled host directory.",
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "properties": { "path": { "type": "string" } }, "required": ["path"]
            }
        },
        {
            "name": "host_filesystem_read",
            "description": "Read one bounded file inside an enrolled host root.",
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "properties": {
                    "path": { "type": "string" },
                    "maxBytes": { "type": "integer", "minimum": 1, "maximum": 8_388_608 }
                }, "required": ["path"]
            }
        },
        {
            "name": "host_filesystem_write",
            "description": "Write one exact enrolled host path after user approval.",
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "properties": {
                    "path": { "type": "string" }, "content": { "type": "string" }
                }, "required": ["path", "content"]
            }
        },
        {
            "name": "host_process_exec",
            "description": "Run one exact host process invocation after user approval. This has the desktop user's authority.",
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "properties": {
                    "argv": { "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": 64 },
                    "cwd": { "type": "string" },
                    "timeoutMs": { "type": "integer", "minimum": 1, "maximum": 300_000 }
                }, "required": ["argv", "cwd"]
            }
        }
    ] })
}

async fn write_http_response(
    stream: &mut tokio::net::TcpStream,
    response: HttpResponse,
) -> std::io::Result<()> {
    let reason = match response.status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        403 => "Forbidden",
        _ => "Error",
    };
    let content_type = if response.body.is_empty() {
        "application/octet-stream"
    } else {
        "application/json"
    };
    let headers = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        response.status,
        response.body.len()
    );
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(&response.body).await
}

async fn handle_connection<S>(
    mut stream: S,
    expected_run_id: &RunId,
    expected_policy_revision: u64,
    services: &HostToolServices,
    initialized: Arc<AtomicBool>,
    initialized_notify: Arc<Notify>,
    cancellation: CancellationToken,
) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let response = match tokio::time::timeout(BRIDGE_IO_TIMEOUT, read_line(&mut stream)).await {
        Ok(Ok(Some(bytes))) => {
            let initialize = serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|request| request.get("initialize").and_then(Value::as_bool))
                == Some(true);
            let response = dispatch(
                &bytes,
                expected_run_id,
                expected_policy_revision,
                services,
                cancellation,
            )
            .await;
            if initialize && response.get("isError").and_then(Value::as_bool) == Some(false) {
                initialized.store(true, Ordering::Release);
                initialized_notify.notify_waiters();
            }
            response
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
    if request.get("initialize").and_then(Value::as_bool) == Some(true) {
        return success_result("Host Tools ready");
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
    use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};

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

    #[test]
    fn unix_socket_path_is_bounded_before_bind() {
        let short = next_unix_socket_path(Path::new("/tmp/gd-1000")).expect("short endpoint");
        assert!(short.as_os_str().as_bytes().len() <= 100);

        let overlong = PathBuf::from(format!("/tmp/{}", "application-data-".repeat(8)));
        let error = next_unix_socket_path(&overlong).expect_err("overlong endpoint");
        assert_eq!(
            error,
            StoreError::Unavailable("Host Tools endpoint unavailable".into())
        );
    }

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
        let socket_path = PathBuf::from(bridge.endpoint());
        let socket_metadata = std::fs::metadata(&socket_path).expect("socket metadata");
        assert_eq!(socket_metadata.permissions().mode() & 0o077, 0);

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
        assert!(!socket_path.exists(), "socket must be removed on shutdown");
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
