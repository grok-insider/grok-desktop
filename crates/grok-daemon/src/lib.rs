//! Trusted daemon composition and bounded Protobuf IPC dispatch.

mod handler;
mod transport;

pub use handler::{
    AgentRuntimeUnavailableReason, AutomationSchedulerLifecycle, Daemon, HandlerError,
};
pub use transport::{
    FrameReadPhase, MAX_FRAME_BYTES, TransportError, read_frame, serve_connection, write_frame,
};
