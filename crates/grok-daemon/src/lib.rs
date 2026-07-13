//! Trusted daemon composition and bounded Protobuf IPC dispatch.

mod handler;
mod host_tool_bridge;
mod host_work_runtime;
mod managed_integration;
mod transport;

pub use handler::{
    AgentRuntimeUnavailableReason, AutomationSchedulerLifecycle, Daemon, HandlerError,
};
pub use host_tool_bridge::HostToolBridge;
pub use host_work_runtime::{
    GrokAcpRoleFactory, HostWorkRoleFactory, HostWorkRuntime, VerifiedHostToolsHelper,
};
pub use managed_integration::{
    IntegrationRecord, ManagedIntegrationAction, ManagedIntegrationError,
    ManagedIntegrationService, ManagedIntegrationState, VerifiedManifest,
    managed_integration_publication_qualified,
};
pub use transport::{
    FrameReadPhase, MAX_FRAME_BYTES, TransportError, read_frame, serve_connection, write_frame,
};
