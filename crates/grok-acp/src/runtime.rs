use std::{
    collections::{HashMap, HashSet},
    io,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};

use agent_client_protocol::schema::{ProtocolVersion, v1 as acp};
use agent_client_protocol::{Agent, ByteStreams, Client, ConnectionTo};
use async_trait::async_trait;
use futures_util::stream;
use grok_application::{
    AgentAuthMethod, AgentEvent, AgentEventStream, AgentPermissionDecision, AgentPermissionOption,
    AgentPermissionOptionKind, AgentPermissionRequest, AgentPrompt, AgentRuntime,
    AgentRuntimeCapabilities, AgentRuntimeError, AgentRuntimeErrorKind, AgentRuntimeProbe,
    AgentSession, AgentSessionRequest, AgentToolCall, AgentToolCallStatus,
};
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use tokio::{
    io::{AsyncRead, AsyncReadExt, ReadBuf},
    sync::{RwLock, mpsc, oneshot, watch},
};
use tokio_util::{
    compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt},
    sync::CancellationToken,
};

use crate::{
    GrokHomeSpec, PermissionBroker, VerifiedGrokComponent, isolation::ProvisionedGrokHome,
};

const DEFAULT_STDERR_LIMIT: usize = 16 * 1024;
const MAX_PROMPT_BYTES: usize = 1024 * 1024;
const MAX_ACP_LINE_BYTES: usize = 4 * 1024 * 1024;
const MAX_AGENT_TEXT_BYTES: usize = 256 * 1024;
const MAX_AGENT_TITLE_BYTES: usize = 1024;
const MAX_AGENT_ID_BYTES: usize = 512;
const MAX_PLAN_ENTRIES: usize = 256;
const MAX_PLAN_BYTES: usize = 1024 * 1024;
const MAX_AUTH_METHODS: usize = 32;
const MAX_PERMISSION_OPTIONS: usize = 32;
const MAX_ADDITIONAL_DIRECTORIES: usize = 7;
const MAX_HOST_MCP_ARGUMENTS: usize = 16;
const MAX_HOST_MCP_ARGUMENT_BYTES: usize = 4096;

type EventResult = Result<AgentEvent, AgentRuntimeError>;
type EventSender = mpsc::Sender<EventResult>;
type SessionEventSenders = Arc<RwLock<HashMap<String, EventSender>>>;

struct BoundedLineReader<R> {
    inner: R,
    current_line_bytes: usize,
    maximum_line_bytes: usize,
    pending: Vec<u8>,
    pending_offset: usize,
}

impl<R> BoundedLineReader<R> {
    fn new(inner: R, maximum_line_bytes: usize) -> Self {
        Self {
            inner,
            current_line_bytes: 0,
            maximum_line_bytes,
            pending: Vec::new(),
            pending_offset: 0,
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for BoundedLineReader<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.pending_offset < this.pending.len() {
            let count = buffer
                .remaining()
                .min(this.pending.len() - this.pending_offset);
            buffer.put_slice(&this.pending[this.pending_offset..this.pending_offset + count]);
            this.pending_offset += count;
            if this.pending_offset == this.pending.len() {
                this.pending.clear();
                this.pending_offset = 0;
            }
            return Poll::Ready(Ok(()));
        }

        let mut scratch = [0_u8; 8 * 1024];
        let mut scratch_buffer = ReadBuf::new(&mut scratch);
        match Pin::new(&mut this.inner).poll_read(context, &mut scratch_buffer) {
            Poll::Ready(Ok(())) => {
                let received = scratch_buffer.filled();
                for byte in received {
                    if *byte == b'\n' {
                        this.current_line_bytes = 0;
                    } else {
                        this.current_line_bytes = this.current_line_bytes.saturating_add(1);
                        if this.current_line_bytes > this.maximum_line_bytes {
                            return Poll::Ready(Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "ACP frame exceeded the configured size limit",
                            )));
                        }
                    }
                }
                let count = buffer.remaining().min(received.len());
                buffer.put_slice(&received[..count]);
                if count < received.len() {
                    this.pending.extend_from_slice(&received[count..]);
                }
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

/// Process, workspace, buffering, and timeout policy for one Grok ACP runtime.
#[derive(Debug, Clone)]
pub struct GrokAcpConfig {
    /// Verified external official Grok component.
    pub component: VerifiedGrokComponent,
    /// Application-owned configuration home that cannot inherit standalone CLI state.
    pub grok_home: GrokHomeSpec,
    /// Workspace roots within which sessions may be created or loaded.
    pub workspace_roots: Vec<PathBuf>,
    /// Maximum commands waiting for the runtime actor.
    pub command_capacity: NonZeroUsize,
    /// Maximum unread events for one prompt.
    pub event_capacity: NonZeroUsize,
    /// ACP initialization deadline.
    pub initialize_timeout: Duration,
    /// Session request deadline.
    pub request_timeout: Duration,
    /// Maximum sanitized stderr bytes retained in memory.
    pub stderr_limit: usize,
    execution_boundary: GrokAcpExecutionBoundary,
}

impl GrokAcpConfig {
    /// Creates a host runtime that permits negotiation and authentication only.
    #[must_use]
    pub fn host_control(component: VerifiedGrokComponent, grok_home: GrokHomeSpec) -> Self {
        Self {
            component,
            grok_home,
            workspace_roots: Vec::new(),
            command_capacity: NonZeroUsize::new(32).unwrap_or(NonZeroUsize::MIN),
            event_capacity: NonZeroUsize::new(256).unwrap_or(NonZeroUsize::MIN),
            initialize_timeout: Duration::from_secs(15),
            request_timeout: Duration::from_secs(30),
            stderr_limit: DEFAULT_STDERR_LIMIT,
            execution_boundary: GrokAcpExecutionBoundary::HostControl,
        }
    }

    /// Creates a session-capable runtime for a qualified isolated guest only.
    #[must_use]
    pub fn isolated_guest(
        component: VerifiedGrokComponent,
        workspace_roots: Vec<PathBuf>,
        grok_home: GrokHomeSpec,
    ) -> Self {
        Self {
            workspace_roots,
            execution_boundary: GrokAcpExecutionBoundary::IsolatedGuest,
            ..Self::host_control(component, grok_home)
        }
    }

    /// Creates a host runtime allowed to open sessions only through the
    /// daemon-mediated Host Tools boundary.
    #[must_use]
    pub fn host_work_tools(
        component: VerifiedGrokComponent,
        workspace_roots: Vec<PathBuf>,
        grok_home: GrokHomeSpec,
    ) -> Self {
        Self {
            workspace_roots,
            execution_boundary: GrokAcpExecutionBoundary::HostWorkTools,
            ..Self::host_control(component, grok_home)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GrokAcpExecutionBoundary {
    HostControl,
    HostWorkTools,
    IsolatedGuest,
}

/// Managed official Grok Build ACP runtime.
#[derive(Clone)]
pub struct GrokAcpRuntime {
    inner: Arc<RuntimeInner>,
}

impl std::fmt::Debug for GrokAcpRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GrokAcpRuntime")
            .finish_non_exhaustive()
    }
}

struct RuntimeInner {
    commands: mpsc::Sender<RuntimeCommand>,
    process: Arc<Mutex<Box<dyn ChildWrapper>>>,
    probe: AgentRuntimeProbe,
    workspace_roots: Vec<PathBuf>,
    request_timeout: Duration,
    event_capacity: usize,
    execution_boundary: GrokAcpExecutionBoundary,
    _grok_home: ProvisionedGrokHome,
    stderr: Arc<Mutex<BoundedStderr>>,
    closed: watch::Receiver<Option<AgentRuntimeError>>,
}

impl Drop for RuntimeInner {
    fn drop(&mut self) {
        let _ = self
            .commands
            .try_send(RuntimeCommand::Shutdown { response: None });
        kill_process(&self.process);
    }
}

enum RuntimeCommand {
    Authenticate {
        method_id: String,
        response: oneshot::Sender<Result<(), AgentRuntimeError>>,
    },
    OpenSession {
        request: AgentSessionRequest,
        response: oneshot::Sender<Result<AgentSession, AgentRuntimeError>>,
    },
    Prompt {
        prompt: AgentPrompt,
        events: mpsc::Sender<Result<AgentEvent, AgentRuntimeError>>,
        response: oneshot::Sender<Result<(), AgentRuntimeError>>,
    },
    Cancel {
        session_id: String,
        response: oneshot::Sender<Result<(), AgentRuntimeError>>,
    },
    Shutdown {
        response: Option<oneshot::Sender<()>>,
    },
}

#[derive(Debug)]
struct BoundedStderr {
    text: String,
    pending_line: Vec<u8>,
    limit: usize,
    truncated: bool,
    discarding_line: bool,
}

impl BoundedStderr {
    fn new(limit: usize) -> Self {
        Self {
            text: String::new(),
            pending_line: Vec::new(),
            limit,
            truncated: false,
            discarding_line: false,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        for byte in bytes {
            if *byte == b'\n' {
                self.finish_line();
            } else if !self.discarding_line {
                if self.pending_line.len() < self.limit {
                    self.pending_line.push(*byte);
                } else {
                    self.pending_line.clear();
                    self.discarding_line = true;
                    self.truncated = true;
                }
            }
        }
    }

    fn finish(&mut self) {
        if !self.pending_line.is_empty() || self.discarding_line {
            self.finish_line();
        }
    }

    fn finish_line(&mut self) {
        if self.discarding_line {
            self.discarding_line = false;
            self.append("[oversize diagnostic discarded]\n");
            return;
        }
        let line = String::from_utf8_lossy(&self.pending_line);
        let mut sanitized = sanitize_diagnostic(&line);
        sanitized.push('\n');
        self.pending_line.clear();
        self.append(sanitized.as_ref());
    }

    fn append(&mut self, sanitized: &str) {
        if self.text.len() >= self.limit {
            self.truncated = true;
            return;
        }
        let remaining = self.limit - self.text.len();
        if sanitized.len() > remaining {
            self.text
                .push_str(&sanitized[..sanitized.floor_char_boundary(remaining)]);
            self.truncated = true;
        } else {
            self.text.push_str(sanitized);
        }
    }
}

impl GrokAcpRuntime {
    /// Verifies the component again, starts `grok agent stdio`, and negotiates ACP v1.
    ///
    /// # Errors
    ///
    /// Returns [`AgentRuntimeError`] when component verification, workspace
    /// validation, process startup, initialization, or protocol negotiation fails.
    pub async fn start(
        config: GrokAcpConfig,
        permissions: PermissionBroker,
    ) -> Result<Self, AgentRuntimeError> {
        config.component.reverify().map_err(component_error)?;
        let roots = match config.execution_boundary {
            GrokAcpExecutionBoundary::HostControl if config.workspace_roots.is_empty() => {
                Vec::new()
            }
            GrokAcpExecutionBoundary::HostControl => {
                return Err(invalid(
                    "host ACP control runtime cannot accept workspace roots",
                ));
            }
            GrokAcpExecutionBoundary::HostWorkTools if config.workspace_roots.is_empty() => {
                return Err(invalid(
                    "HostWorkTools runtime requires at least one workspace root",
                ));
            }
            GrokAcpExecutionBoundary::HostWorkTools | GrokAcpExecutionBoundary::IsolatedGuest => {
                canonical_roots(&config.workspace_roots)?
            }
        };
        let grok_home = config.grok_home.provision().map_err(isolation_error)?;
        let spawned = spawn_component(&config.component, &grok_home, config.stderr_limit)?;
        let SpawnedProcess {
            stdin,
            stdout,
            stderr,
            process,
        } = spawned;
        let stderr_buffer = Arc::new(Mutex::new(BoundedStderr::new(config.stderr_limit)));
        spawn_stderr_reader(stderr, stderr_buffer.clone());
        let (commands, command_rx) = mpsc::channel(config.command_capacity.get());
        let (ready_tx, ready_rx) = oneshot::channel();
        let (closed_tx, closed) = watch::channel(None);
        let (exit_tx, exit_rx) = watch::channel(None);
        spawn_process_monitor(process.clone(), exit_tx);
        let event_senders = Arc::new(RwLock::new(HashMap::new()));
        let cancellations = Arc::new(RwLock::new(HashMap::new()));
        spawn_protocol_actor(ActorContext {
            stdin,
            stdout,
            commands: command_rx,
            ready: ready_tx,
            closed: closed_tx,
            process: process.clone(),
            exit: exit_rx,
            permissions,
            event_senders,
            cancellations,
            request_timeout: config.request_timeout,
        });
        let probe = tokio::time::timeout(config.initialize_timeout, ready_rx)
            .await
            .map_err(|_| unavailable("ACP initialization timed out"))?
            .map_err(|_| unavailable("ACP process exited during initialization"))??;
        Ok(Self {
            inner: Arc::new(RuntimeInner {
                commands,
                process,
                probe,
                workspace_roots: roots,
                request_timeout: config.request_timeout,
                event_capacity: config.event_capacity.get(),
                execution_boundary: config.execution_boundary,
                _grok_home: grok_home,
                stderr: stderr_buffer,
                closed,
            }),
        })
    }

    /// Returns retained sanitized diagnostics for an explicit local support bundle.
    #[must_use]
    pub fn sanitized_stderr(&self) -> String {
        let Ok(stderr) = self.inner.stderr.lock() else {
            return "diagnostic buffer unavailable".into();
        };
        if stderr.truncated {
            format!("{}\n[truncated]", stderr.text)
        } else {
            stderr.text.clone()
        }
    }

    async fn request<T>(
        &self,
        command: RuntimeCommand,
        response: oneshot::Receiver<Result<T, AgentRuntimeError>>,
    ) -> Result<T, AgentRuntimeError> {
        self.ensure_open()?;
        tokio::time::timeout(
            self.inner.request_timeout,
            self.inner.commands.send(command),
        )
        .await
        .map_err(|_| unavailable("ACP command queue timed out"))?
        .map_err(|_| self.closed_error())?;
        tokio::time::timeout(self.inner.request_timeout, response)
            .await
            .map_err(|_| unavailable("ACP command timed out"))?
            .map_err(|_| self.closed_error())?
    }

    fn ensure_open(&self) -> Result<(), AgentRuntimeError> {
        if self.inner.closed.borrow().is_some() {
            return Err(self.closed_error());
        }
        Ok(())
    }

    fn require_session_execution(&self) -> Result<(), AgentRuntimeError> {
        if self.inner.execution_boundary == GrokAcpExecutionBoundary::HostControl {
            return Err(unavailable(
                "ACP control runtime cannot open execution sessions",
            ));
        }
        Ok(())
    }

    fn closed_error(&self) -> AgentRuntimeError {
        self.inner
            .closed
            .borrow()
            .clone()
            .unwrap_or_else(|| unavailable("ACP runtime is unavailable"))
    }
}

#[async_trait]
impl AgentRuntime for GrokAcpRuntime {
    async fn probe(&self) -> Result<AgentRuntimeProbe, AgentRuntimeError> {
        self.ensure_open()?;
        Ok(self.inner.probe.clone())
    }

    async fn authenticate(&self, method_id: &str) -> Result<(), AgentRuntimeError> {
        if method_id.trim().is_empty()
            || !self
                .inner
                .probe
                .auth_methods
                .iter()
                .any(|method| method.id == method_id)
        {
            return Err(invalid(
                "authentication method was not advertised by the agent",
            ));
        }
        let (response_tx, response_rx) = oneshot::channel();
        self.request(
            RuntimeCommand::Authenticate {
                method_id: method_id.into(),
                response: response_tx,
            },
            response_rx,
        )
        .await
    }

    async fn open_session(
        &self,
        mut request: AgentSessionRequest,
    ) -> Result<AgentSession, AgentRuntimeError> {
        self.require_session_execution()?;
        request.working_directory =
            validate_workspace(&request.working_directory, &self.inner.workspace_roots)?;
        if request.additional_directories.len() > MAX_ADDITIONAL_DIRECTORIES {
            return Err(invalid("too many additional workspace directories"));
        }
        request.additional_directories = request
            .additional_directories
            .iter()
            .map(|directory| validate_workspace(directory, &self.inner.workspace_roots))
            .collect::<Result<Vec<_>, _>>()?;
        match (self.inner.execution_boundary, &request.host_tools_mcp) {
            (GrokAcpExecutionBoundary::HostWorkTools, Some(server)) => {
                if !server.executable.is_absolute()
                    || server.arguments.len() > MAX_HOST_MCP_ARGUMENTS
                    || server.arguments.iter().any(|argument| {
                        argument.is_empty() || argument.len() > MAX_HOST_MCP_ARGUMENT_BYTES
                    })
                {
                    return Err(invalid("invalid Host Tools MCP descriptor"));
                }
            }
            (GrokAcpExecutionBoundary::HostWorkTools, None) => {
                return Err(invalid("Host Tools MCP descriptor is required"));
            }
            (_, Some(_)) => return Err(invalid("Host Tools MCP requires HostWorkTools boundary")),
            (_, None) => {}
        }
        let (response_tx, response_rx) = oneshot::channel();
        self.request(
            RuntimeCommand::OpenSession {
                request,
                response: response_tx,
            },
            response_rx,
        )
        .await
    }

    async fn prompt(&self, prompt: AgentPrompt) -> Result<AgentEventStream, AgentRuntimeError> {
        self.require_session_execution()?;
        if prompt.session_id.trim().is_empty()
            || prompt.text.trim().is_empty()
            || prompt.text.len() > MAX_PROMPT_BYTES
        {
            return Err(invalid("prompt session and bounded text are required"));
        }
        let (event_tx, event_rx) = mpsc::channel(self.inner.event_capacity);
        let (response_tx, response_rx) = oneshot::channel();
        self.request(
            RuntimeCommand::Prompt {
                prompt,
                events: event_tx,
                response: response_tx,
            },
            response_rx,
        )
        .await?;
        Ok(Box::pin(stream::unfold(event_rx, |mut receiver| async {
            receiver.recv().await.map(|event| (event, receiver))
        })))
    }

    async fn cancel(&self, session_id: &str) -> Result<(), AgentRuntimeError> {
        self.require_session_execution()?;
        if session_id.trim().is_empty() {
            return Err(invalid("session id is required"));
        }
        let (response_tx, response_rx) = oneshot::channel();
        self.request(
            RuntimeCommand::Cancel {
                session_id: session_id.into(),
                response: response_tx,
            },
            response_rx,
        )
        .await
    }

    async fn shutdown(&self) -> Result<(), AgentRuntimeError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.inner
            .commands
            .send(RuntimeCommand::Shutdown {
                response: Some(response_tx),
            })
            .await
            .map_err(|_| self.closed_error())?;
        let _ = tokio::time::timeout(self.inner.request_timeout, response_rx).await;
        kill_process(&self.inner.process);
        Ok(())
    }
}

struct SpawnedProcess {
    stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    process: Arc<Mutex<Box<dyn ChildWrapper>>>,
}

fn spawn_component(
    component: &VerifiedGrokComponent,
    grok_home: &ProvisionedGrokHome,
    stderr_limit: usize,
) -> Result<SpawnedProcess, AgentRuntimeError> {
    if stderr_limit == 0 || stderr_limit > 1024 * 1024 {
        return Err(invalid("stderr limit must be between 1 byte and 1 MiB"));
    }
    let mut command = CommandWrap::with_new(component.executable(), |command| {
        command
            .args(["--no-auto-update", "agent", "stdio"])
            .current_dir(grok_home.launch_directory())
            .env_clear()
            .envs(isolated_environment(grok_home))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
    });
    command.wrap(KillOnDrop);
    #[cfg(unix)]
    command.wrap(process_wrap::tokio::ProcessGroup::leader());
    #[cfg(windows)]
    command.wrap(process_wrap::tokio::JobObject);
    let mut child = command
        .spawn()
        .map_err(|_| process_error("failed to start official Grok component"))?;
    let stdin = child
        .stdin()
        .take()
        .ok_or_else(|| process_error("Grok component stdin unavailable"))?;
    let stdout = child
        .stdout()
        .take()
        .ok_or_else(|| process_error("Grok component stdout unavailable"))?;
    let stderr = child
        .stderr()
        .take()
        .ok_or_else(|| process_error("Grok component stderr unavailable"))?;
    Ok(SpawnedProcess {
        stdin,
        stdout,
        stderr,
        process: Arc::new(Mutex::new(child)),
    })
}

struct ActorContext {
    stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
    commands: mpsc::Receiver<RuntimeCommand>,
    ready: oneshot::Sender<Result<AgentRuntimeProbe, AgentRuntimeError>>,
    closed: watch::Sender<Option<AgentRuntimeError>>,
    process: Arc<Mutex<Box<dyn ChildWrapper>>>,
    exit: watch::Receiver<Option<String>>,
    permissions: PermissionBroker,
    event_senders: SessionEventSenders,
    cancellations: Arc<RwLock<HashMap<String, CancellationToken>>>,
    request_timeout: Duration,
}

fn spawn_protocol_actor(context: ActorContext) {
    tokio::spawn(async move {
        let process = context.process.clone();
        let closed = context.closed.clone();
        let event_senders = context.event_senders.clone();
        let result = run_protocol(context).await;
        let error = result
            .err()
            .unwrap_or_else(|| unavailable("ACP runtime stopped"));
        let active = {
            let mut senders = event_senders.write().await;
            senders
                .drain()
                .map(|(_, sender)| sender)
                .collect::<Vec<_>>()
        };
        for sender in active {
            let _ = sender.send(Err(error.clone())).await;
        }
        let _ = closed.send(Some(error));
        kill_process(&process);
    });
}

#[allow(clippy::too_many_lines)]
async fn run_protocol(context: ActorContext) -> Result<(), AgentRuntimeError> {
    let ActorContext {
        stdin,
        stdout,
        mut commands,
        ready,
        closed: _,
        process: _,
        exit,
        permissions,
        event_senders,
        cancellations,
        request_timeout,
    } = context;
    let notification_senders = event_senders.clone();
    let permission_cancellations = cancellations.clone();
    let mut actor_exit = exit.clone();
    let protocol = Client
        .builder()
        .on_receive_notification(
            async move |notification: acp::SessionNotification, _connection| {
                route_session_update(&notification_senders, notification).await;
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: acp::RequestPermissionRequest, responder, connection| {
                let broker = permissions.clone();
                let cancellation = permission_cancellations
                    .read()
                    .await
                    .get(&request.session_id.to_string())
                    .cloned();
                connection.spawn(async move {
                    let decision = if let Some(mapped) = map_permission_request(request) {
                        if let Some(cancellation) = cancellation {
                            tokio::select! {
                                decision = broker.decide(mapped) => decision,
                                () = cancellation.cancelled() => AgentPermissionDecision::Cancelled,
                            }
                        } else {
                            broker.decide(mapped).await
                        }
                    } else {
                        AgentPermissionDecision::Cancelled
                    };
                    responder.respond(map_permission_decision(decision))
                })?;
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(
            ByteStreams::new(
                stdin.compat_write(),
                BoundedLineReader::new(stdout, MAX_ACP_LINE_BYTES).compat(),
            ),
            move |connection: ConnectionTo<Agent>| async move {
                let initialized = tokio::time::timeout(
                    request_timeout,
                    connection
                        .send_request(
                            acp::InitializeRequest::new(ProtocolVersion::V1).client_info(
                                acp::Implementation::new("grok-desktop", env!("CARGO_PKG_VERSION")),
                            ),
                        )
                        .block_task(),
                )
                .await
                .map_err(|_| agent_client_protocol::Error::internal_error())??;
                if initialized.protocol_version != ProtocolVersion::V1 {
                    let _ = ready.send(Err(protocol_error("agent rejected ACP v1")));
                    return Err(agent_client_protocol::Error::internal_error());
                }
                let probe = map_probe(initialized);
                if ready.send(Ok(probe.clone())).is_err() {
                    return Ok(());
                }
                let mut sessions = HashSet::new();
                loop {
                    tokio::select! {
                        biased;
                        command = commands.recv() => {
                            if !handle_command(
                                command,
                                &connection,
                                &probe,
                                &mut sessions,
                                &event_senders,
                                &cancellations,
                                request_timeout,
                            ).await? {
                                return Ok(());
                            }
                        }
                        changed = actor_exit.changed() => {
                            if changed.is_err() || actor_exit.borrow().is_some() {
                                return Err(agent_client_protocol::Error::internal_error());
                            }
                        }
                    }
                }
            },
        )
        .await;
    protocol.map_err(|_| {
        if let Some(status) = exit.borrow().clone() {
            process_error(&format!("official Grok component exited ({status})"))
        } else {
            protocol_error("ACP connection failed or emitted malformed output")
        }
    })
}

#[allow(clippy::too_many_lines)]
async fn handle_command(
    command: Option<RuntimeCommand>,
    connection: &ConnectionTo<Agent>,
    probe: &AgentRuntimeProbe,
    sessions: &mut HashSet<String>,
    event_senders: &SessionEventSenders,
    cancellations: &Arc<RwLock<HashMap<String, CancellationToken>>>,
    request_timeout: Duration,
) -> Result<bool, agent_client_protocol::Error> {
    match command {
        Some(RuntimeCommand::Authenticate {
            method_id,
            response,
        }) => {
            let result = tokio::time::timeout(
                request_timeout,
                connection
                    .send_request(acp::AuthenticateRequest::new(method_id))
                    .block_task(),
            )
            .await
            .map_err(|_| AgentRuntimeError {
                kind: AgentRuntimeErrorKind::Authentication,
                message: "Grok authentication timed out".into(),
                retryable: true,
            })
            .and_then(|response| {
                response.map(|_| ()).map_err(|_| AgentRuntimeError {
                    kind: AgentRuntimeErrorKind::Authentication,
                    message: "Grok authentication failed".into(),
                    retryable: true,
                })
            });
            let _ = response.send(result);
        }
        Some(RuntimeCommand::OpenSession { request, response }) => {
            let result = open_session(connection, probe, request, request_timeout).await;
            if let Ok(session) = &result {
                sessions.insert(session.id.clone());
            }
            let _ = response.send(result);
        }
        Some(RuntimeCommand::Prompt {
            prompt,
            events,
            response,
        }) => {
            if !sessions.contains(&prompt.session_id) {
                let _ = response.send(Err(invalid("session is not owned by this runtime")));
                return Ok(true);
            }
            let mut active_prompts = event_senders.write().await;
            if active_prompts.contains_key(&prompt.session_id) {
                let _ = response.send(Err(invalid("session already has an active prompt")));
                return Ok(true);
            }
            active_prompts.insert(prompt.session_id.clone(), events.clone());
            drop(active_prompts);
            let cancellation = CancellationToken::new();
            cancellations
                .write()
                .await
                .insert(prompt.session_id.clone(), cancellation);
            let session_id = prompt.session_id.clone();
            let callback_senders = event_senders.clone();
            let callback_cancellations = cancellations.clone();
            let sent = connection.send_request(acp::PromptRequest::new(
                session_id.clone(),
                vec![acp::ContentBlock::Text(acp::TextContent::new(prompt.text))],
            ));
            if let Err(error) = sent.on_receiving_result(async move |result| {
                let event = match result {
                    Ok(result) => Ok(AgentEvent::Completed {
                        stop_reason: stop_reason(result.stop_reason).into(),
                    }),
                    Err(_) => Err(protocol_error("ACP prompt failed")),
                };
                let _ = events.send(event).await;
                callback_senders.write().await.remove(&session_id);
                callback_cancellations.write().await.remove(&session_id);
                Ok(())
            }) {
                event_senders.write().await.remove(&prompt.session_id);
                cancellations.write().await.remove(&prompt.session_id);
                let _ = response.send(Err(protocol_error("failed to register ACP prompt")));
                return Err(error);
            }
            let _ = response.send(Ok(()));
        }
        Some(RuntimeCommand::Cancel {
            session_id,
            response,
        }) => {
            if sessions.contains(&session_id) {
                if let Some(cancellation) = cancellations.read().await.get(&session_id) {
                    cancellation.cancel();
                }
                let result = connection
                    .send_notification(acp::CancelNotification::new(session_id))
                    .map_err(|_| protocol_error("failed to cancel ACP prompt"));
                let _ = response.send(result);
            } else {
                let _ = response.send(Err(invalid("session is not owned by this runtime")));
            }
        }
        Some(RuntimeCommand::Shutdown { response }) => {
            for cancellation in cancellations.read().await.values() {
                cancellation.cancel();
            }
            if let Some(response) = response {
                let _ = response.send(());
            }
            return Ok(false);
        }
        None => return Ok(false),
    }
    Ok(true)
}

async fn open_session(
    connection: &ConnectionTo<Agent>,
    probe: &AgentRuntimeProbe,
    request: AgentSessionRequest,
    request_timeout: Duration,
) -> Result<AgentSession, AgentRuntimeError> {
    if let Some(session_id) = request.existing_session_id {
        if !request.additional_directories.is_empty() || request.host_tools_mcp.is_some() {
            return Err(invalid(
                "loaded sessions cannot change workspace or Host Tools MCP bindings",
            ));
        }
        if !probe.capabilities.load_session {
            return Err(invalid("agent does not support loading sessions"));
        }
        tokio::time::timeout(
            request_timeout,
            connection
                .send_request(acp::LoadSessionRequest::new(
                    session_id.clone(),
                    request.working_directory,
                ))
                .block_task(),
        )
        .await
        .map_err(|_| unavailable("loading ACP session timed out"))?
        .map_err(|_| protocol_error("failed to load ACP session"))?;
        Ok(AgentSession { id: session_id })
    } else {
        let mut session_request = acp::NewSessionRequest::new(request.working_directory)
            .additional_directories(request.additional_directories);
        if let Some(server) = request.host_tools_mcp {
            session_request = session_request.mcp_servers(vec![acp::McpServer::Stdio(
                acp::McpServerStdio::new("grok-desktop-host-tools", server.executable)
                    .args(server.arguments),
            )]);
        }
        let response = tokio::time::timeout(
            request_timeout,
            connection.send_request(session_request).block_task(),
        )
        .await
        .map_err(|_| unavailable("creating ACP session timed out"))?
        .map_err(|_| protocol_error("failed to create ACP session"))?;
        Ok(AgentSession {
            id: response.session_id.to_string(),
        })
    }
}

fn map_probe(response: acp::InitializeResponse) -> AgentRuntimeProbe {
    let capabilities = response.agent_capabilities;
    AgentRuntimeProbe {
        protocol_version: response.protocol_version.as_u16(),
        agent_name: response
            .agent_info
            .as_ref()
            .and_then(|info| bounded_agent_text(info.name.clone(), 128)),
        agent_version: response
            .agent_info
            .as_ref()
            .and_then(|info| bounded_agent_text(info.version.clone(), 128)),
        auth_methods: response
            .auth_methods
            .iter()
            .take(MAX_AUTH_METHODS)
            .filter_map(|method| {
                Some(AgentAuthMethod {
                    id: bounded_agent_text(method.id().to_string(), MAX_AGENT_ID_BYTES)?,
                    name: bounded_agent_text(method.name().to_owned(), MAX_AGENT_TITLE_BYTES)?,
                    description: method
                        .description()
                        .map(str::to_owned)
                        .and_then(|value| bounded_agent_text(value, 4 * 1024)),
                })
            })
            .collect(),
        capabilities: AgentRuntimeCapabilities {
            load_session: capabilities.load_session,
            embedded_context: capabilities.prompt_capabilities.embedded_context,
            image_input: capabilities.prompt_capabilities.image,
            audio_input: capabilities.prompt_capabilities.audio,
            mcp_http: capabilities.mcp_capabilities.http,
            mcp_sse: capabilities.mcp_capabilities.sse,
        },
    }
}

async fn route_session_update(
    senders: &SessionEventSenders,
    notification: acp::SessionNotification,
) {
    let session_id = notification.session_id.to_string();
    let Some(sender) = senders.read().await.get(&session_id).cloned() else {
        return;
    };
    if let Some(event) = map_session_update(notification.update) {
        let _ = sender.send(Ok(event)).await;
    }
}

fn map_session_update(update: acp::SessionUpdate) -> Option<AgentEvent> {
    match update {
        acp::SessionUpdate::AgentMessageChunk(chunk) => content_text(chunk.content)
            .map(AgentEvent::MessageDelta)
            .or_else(|| {
                Some(AgentEvent::Warning(
                    "unsupported or oversized ACP message content ignored".into(),
                ))
            }),
        acp::SessionUpdate::AgentThoughtChunk(chunk) => content_text(chunk.content)
            .map(AgentEvent::ThoughtDelta)
            .or_else(|| {
                Some(AgentEvent::Warning(
                    "unsupported or oversized ACP thought content ignored".into(),
                ))
            }),
        acp::SessionUpdate::ToolCall(call) => Some(map_tool_call(
            call.tool_call_id.to_string(),
            call.title,
            map_tool_status(call.status),
        )),
        acp::SessionUpdate::ToolCallUpdate(update) => Some(map_tool_call(
            update.tool_call_id.to_string(),
            update
                .fields
                .title
                .unwrap_or_else(|| "Tool activity".into()),
            update
                .fields
                .status
                .map_or(AgentToolCallStatus::InProgress, map_tool_status),
        )),
        acp::SessionUpdate::Plan(plan) => Some(map_plan(
            plan.entries
                .into_iter()
                .map(|entry| entry.content)
                .collect(),
        )),
        acp::SessionUpdate::UserMessageChunk(_)
        | acp::SessionUpdate::AvailableCommandsUpdate(_)
        | acp::SessionUpdate::CurrentModeUpdate(_)
        | acp::SessionUpdate::ConfigOptionUpdate(_)
        | acp::SessionUpdate::SessionInfoUpdate(_)
        | acp::SessionUpdate::UsageUpdate(_) => None,
        _ => Some(AgentEvent::Warning("unsupported ACP update ignored".into())),
    }
}

fn content_text(content: acp::ContentBlock) -> Option<String> {
    if let acp::ContentBlock::Text(text) = content {
        bounded_agent_text(text.text, MAX_AGENT_TEXT_BYTES)
    } else {
        None
    }
}

fn map_tool_call(id: String, title: String, status: AgentToolCallStatus) -> AgentEvent {
    let Some(id) = bounded_agent_text(id, MAX_AGENT_ID_BYTES) else {
        return AgentEvent::Warning("oversized ACP tool activity ignored".into());
    };
    let Some(title) = bounded_agent_text(title, MAX_AGENT_TITLE_BYTES) else {
        return AgentEvent::Warning("oversized ACP tool activity ignored".into());
    };
    AgentEvent::ToolCall(AgentToolCall { id, title, status })
}

fn map_plan(entries: Vec<String>) -> AgentEvent {
    if entries.len() > MAX_PLAN_ENTRIES {
        return AgentEvent::Warning("oversized ACP plan ignored".into());
    }
    let mut total = 0_usize;
    let mut bounded = Vec::with_capacity(entries.len());
    for entry in entries {
        total = total.saturating_add(entry.len());
        let Some(entry) = bounded_agent_text(entry, MAX_AGENT_TEXT_BYTES) else {
            return AgentEvent::Warning("oversized ACP plan ignored".into());
        };
        if total > MAX_PLAN_BYTES {
            return AgentEvent::Warning("oversized ACP plan ignored".into());
        }
        bounded.push(entry);
    }
    AgentEvent::Plan(bounded)
}

fn map_tool_status(status: acp::ToolCallStatus) -> AgentToolCallStatus {
    match status {
        acp::ToolCallStatus::Pending => AgentToolCallStatus::Pending,
        acp::ToolCallStatus::Completed => AgentToolCallStatus::Completed,
        acp::ToolCallStatus::Failed => AgentToolCallStatus::Failed,
        _ => AgentToolCallStatus::InProgress,
    }
}

fn map_permission_request(
    request: acp::RequestPermissionRequest,
) -> Option<AgentPermissionRequest> {
    if request.options.len() > MAX_PERMISSION_OPTIONS {
        return None;
    }
    let session_id = bounded_agent_text(request.session_id.to_string(), MAX_AGENT_ID_BYTES)?;
    let title = bounded_agent_text(
        request
            .tool_call
            .fields
            .title
            .unwrap_or_else(|| "Tool permission".into()),
        MAX_AGENT_TITLE_BYTES,
    )?;
    let options = request
        .options
        .into_iter()
        .map(|option| {
            Some(AgentPermissionOption {
                id: bounded_agent_text(option.option_id.to_string(), MAX_AGENT_ID_BYTES)?,
                name: bounded_agent_text(option.name, MAX_AGENT_TITLE_BYTES)?,
                kind: match option.kind {
                    acp::PermissionOptionKind::AllowOnce => AgentPermissionOptionKind::AllowOnce,
                    acp::PermissionOptionKind::AllowAlways => {
                        AgentPermissionOptionKind::AllowAlways
                    }
                    acp::PermissionOptionKind::RejectAlways => {
                        AgentPermissionOptionKind::RejectAlways
                    }
                    _ => AgentPermissionOptionKind::RejectOnce,
                },
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(AgentPermissionRequest {
        request_id: format!("permission-{}", uuid::Uuid::new_v4()),
        session_id,
        title,
        options,
    })
}

fn bounded_agent_text(value: String, maximum: usize) -> Option<String> {
    if value.is_empty()
        || value.len() > maximum
        || value.chars().any(|character| {
            character == '\0'
                || (character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        })
    {
        return None;
    }
    Some(value)
}

fn map_permission_decision(decision: AgentPermissionDecision) -> acp::RequestPermissionResponse {
    let outcome = match decision {
        AgentPermissionDecision::Selected(id) => {
            acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome::new(id))
        }
        AgentPermissionDecision::Cancelled => acp::RequestPermissionOutcome::Cancelled,
    };
    acp::RequestPermissionResponse::new(outcome)
}

fn stop_reason(reason: acp::StopReason) -> &'static str {
    match reason {
        acp::StopReason::EndTurn => "end_turn",
        acp::StopReason::MaxTokens => "max_tokens",
        acp::StopReason::MaxTurnRequests => "max_turn_requests",
        acp::StopReason::Refusal => "refusal",
        acp::StopReason::Cancelled => "cancelled",
        _ => "unknown",
    }
}

fn canonical_roots(roots: &[PathBuf]) -> Result<Vec<PathBuf>, AgentRuntimeError> {
    let mut canonical = Vec::with_capacity(roots.len());
    for root in roots {
        let root = root
            .canonicalize()
            .map_err(|_| invalid("workspace root is unavailable"))?;
        if !root.is_dir() || canonical.contains(&root) {
            return Err(invalid("workspace roots must be unique directories"));
        }
        canonical.push(root);
    }
    Ok(canonical)
}

fn validate_workspace(path: &Path, roots: &[PathBuf]) -> Result<PathBuf, AgentRuntimeError> {
    let canonical = path
        .canonicalize()
        .map_err(|_| invalid("working directory is unavailable"))?;
    if !canonical.is_dir() || !roots.iter().any(|root| canonical.starts_with(root)) {
        return Err(invalid("working directory is outside configured roots"));
    }
    Ok(canonical)
}

fn spawn_stderr_reader(mut stderr: tokio::process::ChildStderr, buffer: Arc<Mutex<BoundedStderr>>) {
    tokio::spawn(async move {
        let mut chunk = [0; 1024];
        loop {
            let Ok(read) = stderr.read(&mut chunk).await else {
                return;
            };
            if read == 0 {
                if let Ok(mut buffer) = buffer.lock() {
                    buffer.finish();
                }
                return;
            }
            if let Ok(mut buffer) = buffer.lock() {
                buffer.push(&chunk[..read]);
            }
        }
    });
}

fn spawn_process_monitor(
    process: Arc<Mutex<Box<dyn ChildWrapper>>>,
    state: watch::Sender<Option<String>>,
) {
    tokio::spawn(async move {
        loop {
            let status = process
                .lock()
                .ok()
                .and_then(|mut process| process.try_wait().ok())
                .flatten();
            if let Some(status) = status {
                let _ = state.send(Some(status.to_string()));
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
}

fn kill_process(process: &Arc<Mutex<Box<dyn ChildWrapper>>>) {
    if let Ok(mut process) = process.lock() {
        let _ = process.start_kill();
    }
}

fn sanitize_diagnostic(value: &str) -> String {
    let lowered = value.to_ascii_lowercase();
    if lowered.contains("authorization:")
        || lowered.contains("bearer ")
        || lowered.contains("xai-")
        || lowered.contains("api_key")
        || lowered.contains("token=")
    {
        return "[redacted sensitive diagnostic]".into();
    }
    value
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\r' | '\t'))
        .collect()
}

fn isolated_environment(
    grok_home: &ProvisionedGrokHome,
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let environment = grok_home
        .environment()
        .into_iter()
        .map(|(name, value)| (name.into(), value.into_os_string()))
        .collect::<Vec<_>>();
    #[cfg(windows)]
    {
        let mut environment = environment;
        for name in ["SystemRoot", "WINDIR"] {
            if let Some(value) = std::env::var_os(name) {
                environment.push((name.into(), value));
            }
        }
        environment
    }
    #[cfg(not(windows))]
    environment
}

fn component_error(error: impl std::fmt::Display) -> AgentRuntimeError {
    AgentRuntimeError {
        kind: AgentRuntimeErrorKind::ComponentVerification,
        message: format!("official Grok component verification failed: {error}"),
        retryable: false,
    }
}

fn isolation_error(_error: impl std::fmt::Display) -> AgentRuntimeError {
    AgentRuntimeError {
        kind: AgentRuntimeErrorKind::ComponentVerification,
        message: "official Grok configuration isolation failed".into(),
        retryable: false,
    }
}

fn process_error(message: &str) -> AgentRuntimeError {
    AgentRuntimeError {
        kind: AgentRuntimeErrorKind::Process,
        message: message.into(),
        retryable: true,
    }
}

fn protocol_error(message: &str) -> AgentRuntimeError {
    AgentRuntimeError {
        kind: AgentRuntimeErrorKind::Protocol,
        message: message.into(),
        retryable: false,
    }
}

fn invalid(message: &str) -> AgentRuntimeError {
    AgentRuntimeError {
        kind: AgentRuntimeErrorKind::InvalidRequest,
        message: message.into(),
        retryable: false,
    }
}

fn unavailable(message: &str) -> AgentRuntimeError {
    AgentRuntimeError {
        kind: AgentRuntimeErrorKind::Unavailable,
        message: message.into(),
        retryable: true,
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn diagnostics_are_bounded_and_sensitive_lines_are_redacted() {
        let mut buffer = BoundedStderr::new(32);
        buffer.push(b"Authoriza");
        buffer.push(b"tion: Bea");
        buffer.push(b"rer secret\n");
        buffer.push(&[b'a'; 100]);
        buffer.finish();
        assert!(!buffer.text.contains("secret"));
        assert!(buffer.text.len() <= 32);
        assert!(buffer.truncated);
    }

    #[test]
    fn host_control_boundary_rejects_session_execution() {
        assert_ne!(
            GrokAcpExecutionBoundary::HostControl,
            GrokAcpExecutionBoundary::IsolatedGuest
        );
    }

    #[test]
    fn workspace_validation_resolves_symlinks_before_authorizing() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path().join("root");
        let outside = directory.path().join("outside");
        std::fs::create_dir_all(&root).expect("root");
        std::fs::create_dir_all(&outside).expect("outside");
        let roots = canonical_roots(std::slice::from_ref(&root)).expect("roots");
        assert!(validate_workspace(&root, &roots).is_ok());
        assert!(validate_workspace(&outside, &roots).is_err());
    }

    #[tokio::test]
    async fn acp_reader_rejects_an_oversized_line_before_json_parsing() {
        let input = Cursor::new(b"123456789".to_vec());
        let mut reader = BoundedLineReader::new(input, 8);
        let mut output = Vec::new();
        let error = reader
            .read_to_end(&mut output)
            .await
            .expect_err("oversized line must fail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn acp_reader_resets_its_budget_at_each_newline() {
        let input = Cursor::new(b"12345678\n12345678\n".to_vec());
        let mut reader = BoundedLineReader::new(input, 8);
        let mut output = Vec::new();
        reader
            .read_to_end(&mut output)
            .await
            .expect("bounded lines");
        assert_eq!(output, b"12345678\n12345678\n");
    }

    #[test]
    fn semantic_agent_values_are_bounded_before_queueing() {
        assert!(bounded_agent_text("ok".into(), 2).is_some());
        assert!(bounded_agent_text("too long".into(), 2).is_none());
        assert!(matches!(
            map_plan(vec!["x".repeat(MAX_AGENT_TEXT_BYTES + 1)]),
            AgentEvent::Warning(_)
        ));
    }
}
