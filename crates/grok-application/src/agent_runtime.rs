use std::{path::PathBuf, pin::Pin};

use async_trait::async_trait;
use futures_core::Stream;
use thiserror::Error;

/// Authentication method advertised by the official Grok agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAuthMethod {
    /// Stable ACP method identifier.
    pub id: String,
    /// Human-readable method name.
    pub name: String,
    /// Optional provider explanation.
    pub description: Option<String>,
}

/// Negotiated ACP features used to degrade the desktop UI safely.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct AgentRuntimeCapabilities {
    /// Existing ACP sessions can be loaded.
    pub load_session: bool,
    /// Prompt content may contain embedded context blocks.
    pub embedded_context: bool,
    /// Prompt content may contain images.
    pub image_input: bool,
    /// Prompt content may contain audio.
    pub audio_input: bool,
    /// Agent accepts HTTP MCP servers.
    pub mcp_http: bool,
    /// Agent accepts SSE MCP servers.
    pub mcp_sse: bool,
}

/// Health, identity, authentication, and capability result of ACP initialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRuntimeProbe {
    /// Negotiated wire protocol version.
    pub protocol_version: u16,
    /// Provider-reported agent name.
    pub agent_name: Option<String>,
    /// Provider-reported agent version.
    pub agent_version: Option<String>,
    /// Authentication methods accepted by the agent.
    pub auth_methods: Vec<AgentAuthMethod>,
    /// Negotiated optional behavior.
    pub capabilities: AgentRuntimeCapabilities,
}

/// Request to create or load a session within an allowlisted workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSessionRequest {
    /// Canonical or canonically resolvable workspace path.
    pub working_directory: PathBuf,
    /// Existing provider session identifier, or `None` for a new session.
    pub existing_session_id: Option<String>,
}

/// Session accepted by the official Grok agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSession {
    /// Opaque provider session identifier.
    pub id: String,
}

/// Text prompt for an established ACP session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPrompt {
    /// Opaque session identifier returned by this runtime.
    pub session_id: String,
    /// User-authored UTF-8 prompt.
    pub text: String,
}

/// Coarse tool-call lifecycle independent of ACP schema revisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentToolCallStatus {
    /// Agent has announced the call.
    Pending,
    /// Tool is actively running.
    InProgress,
    /// Tool completed successfully.
    Completed,
    /// Tool failed.
    Failed,
}

/// Sanitized tool activity emitted by the runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentToolCall {
    /// Opaque call identifier.
    pub id: String,
    /// Human-readable title supplied by the agent.
    pub title: String,
    /// Current lifecycle status.
    pub status: AgentToolCallStatus,
}

/// Ordered event from an ACP prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    /// Incremental assistant text.
    MessageDelta(String),
    /// Incremental private reasoning summary intended for display.
    ThoughtDelta(String),
    /// Tool activity was created or updated.
    ToolCall(AgentToolCall),
    /// Agent reported an execution plan as displayable text entries.
    Plan(Vec<String>),
    /// Prompt finished with the provider stop reason.
    Completed {
        /// Stable ACP stop reason.
        stop_reason: String,
    },
    /// Runtime degraded or ignored an unsupported non-critical update.
    Warning(String),
}

/// ACP permission option classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentPermissionOptionKind {
    /// Approve this request only.
    AllowOnce,
    /// Approve matching requests beyond this request.
    AllowAlways,
    /// Deny this request only.
    RejectOnce,
    /// Deny matching future requests.
    RejectAlways,
}

/// One exact option offered by the agent for a permission request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPermissionOption {
    /// Opaque ACP option identifier.
    pub id: String,
    /// User-visible option label.
    pub name: String,
    /// Scope classification.
    pub kind: AgentPermissionOptionKind,
}

/// Permission request routed to the host policy and approval layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPermissionRequest {
    /// Opaque local correlation identifier.
    pub request_id: String,
    /// Session making the request.
    pub session_id: String,
    /// Sanitized tool title or operation summary.
    pub title: String,
    /// Exact options advertised by the agent.
    pub options: Vec<AgentPermissionOption>,
}

/// Host decision returned to the ACP permission responder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentPermissionDecision {
    /// Select exactly one option offered in the request.
    Selected(String),
    /// Reject without selecting an option.
    Cancelled,
}

/// Stable class for agent runtime failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRuntimeErrorKind {
    /// External component failed verification.
    ComponentVerification,
    /// Agent process could not start or exited unexpectedly.
    Process,
    /// Agent emitted malformed or incompatible ACP messages.
    Protocol,
    /// Session or prompt input violates local policy.
    InvalidRequest,
    /// Authentication is absent or rejected.
    Authentication,
    /// Bounded host channel was unavailable or timed out.
    Permission,
    /// Requested operation was cancelled.
    Cancelled,
    /// Runtime is shutting down or no longer reachable.
    Unavailable,
}

/// Sanitized agent runtime error safe to cross daemon IPC.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct AgentRuntimeError {
    /// Stable failure classification.
    pub kind: AgentRuntimeErrorKind,
    /// Message with credentials, raw provider output, and paths removed.
    pub message: String,
    /// Whether retrying after state changes may succeed.
    pub retryable: bool,
}

/// Sendable ordered event stream for one prompt.
pub type AgentEventStream =
    Pin<Box<dyn Stream<Item = Result<AgentEvent, AgentRuntimeError>> + Send + 'static>>;

/// Subscription-backed Grok agent capability.
#[async_trait]
pub trait AgentRuntime: Send + Sync {
    /// Returns the initialized identity, auth methods, and negotiated capabilities.
    async fn probe(&self) -> Result<AgentRuntimeProbe, AgentRuntimeError>;

    /// Starts an authentication method advertised by [`Self::probe`].
    async fn authenticate(&self, method_id: &str) -> Result<(), AgentRuntimeError>;

    /// Creates or loads a session within a configured workspace root.
    async fn open_session(
        &self,
        request: AgentSessionRequest,
    ) -> Result<AgentSession, AgentRuntimeError>;

    /// Starts a streamed text prompt.
    async fn prompt(&self, prompt: AgentPrompt) -> Result<AgentEventStream, AgentRuntimeError>;

    /// Cancels the active prompt for a session.
    async fn cancel(&self, session_id: &str) -> Result<(), AgentRuntimeError>;

    /// Stops accepting commands and terminates the managed process tree.
    async fn shutdown(&self) -> Result<(), AgentRuntimeError>;
}
