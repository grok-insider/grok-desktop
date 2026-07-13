use std::{num::NonZeroUsize, time::Duration};

use grok_application::{AgentPermissionDecision, AgentPermissionRequest};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

/// Receiving side owned by the trusted daemon approval coordinator.
pub struct HostPermissionChannel {
    receiver: mpsc::Receiver<PendingPermission>,
}

impl std::fmt::Debug for HostPermissionChannel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostPermissionChannel")
            .finish_non_exhaustive()
    }
}

impl HostPermissionChannel {
    /// Waits for the next bounded permission request.
    pub async fn recv(&mut self) -> Option<PendingPermission> {
        self.receiver.recv().await
    }
}

/// One permission request awaiting a fail-closed host response.
pub struct PendingPermission {
    request: AgentPermissionRequest,
    response: oneshot::Sender<AgentPermissionDecision>,
}

impl std::fmt::Debug for PendingPermission {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingPermission")
            .field("request", &self.request)
            .finish_non_exhaustive()
    }
}

impl PendingPermission {
    /// Returns the exact request for policy evaluation and user presentation.
    #[must_use]
    pub const fn request(&self) -> &AgentPermissionRequest {
        &self.request
    }

    /// Resolves the request; invalid or late selections are rejected by the broker.
    ///
    /// # Errors
    ///
    /// Returns [`PermissionResponseError`] when the runtime request was cancelled,
    /// timed out, or shut down before the host decision arrived.
    pub fn respond(self, decision: AgentPermissionDecision) -> Result<(), PermissionResponseError> {
        self.response
            .send(decision)
            .map_err(|_| PermissionResponseError::Closed)
    }
}

/// Sending side used only by the ACP request handler.
#[derive(Debug, Clone)]
pub struct PermissionBroker {
    sender: mpsc::Sender<PendingPermission>,
    timeout: Duration,
}

impl PermissionBroker {
    /// Requests a host decision, cancelling on saturation, timeout, disconnect,
    /// or selection of an option the agent did not offer.
    pub(crate) async fn decide(&self, request: AgentPermissionRequest) -> AgentPermissionDecision {
        let offered_ids = request
            .options
            .iter()
            .map(|option| option.id.clone())
            .collect::<Vec<_>>();
        let (response, receiver) = oneshot::channel();
        if self
            .sender
            .try_send(PendingPermission { request, response })
            .is_err()
        {
            return AgentPermissionDecision::Cancelled;
        }
        let Ok(Ok(decision)) = tokio::time::timeout(self.timeout, receiver).await else {
            return AgentPermissionDecision::Cancelled;
        };
        match decision {
            AgentPermissionDecision::Selected(id) if offered_ids.contains(&id) => {
                AgentPermissionDecision::Selected(id)
            }
            AgentPermissionDecision::Selected(_) | AgentPermissionDecision::Cancelled => {
                AgentPermissionDecision::Cancelled
            }
        }
    }
}

/// Host attempted to answer a request that is no longer active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PermissionResponseError {
    /// Response receiver has already failed closed.
    #[error("permission request is no longer active")]
    Closed,
}

/// Creates the bounded bridge between ACP and trusted host approval handling.
#[must_use]
pub fn permission_channel(
    capacity: NonZeroUsize,
    timeout: Duration,
) -> (HostPermissionChannel, PermissionBroker) {
    let (sender, receiver) = mpsc::channel(capacity.get());
    (
        HostPermissionChannel { receiver },
        PermissionBroker { sender, timeout },
    )
}

#[cfg(test)]
mod tests {
    use grok_application::{
        AgentPermissionOption, AgentPermissionOptionKind, AgentPermissionRequest,
    };

    use super::*;

    fn request() -> AgentPermissionRequest {
        AgentPermissionRequest {
            request_id: "permission-1".into(),
            session_id: "session-1".into(),
            title: "Write report".into(),
            managed_host_tool: None,
            options: vec![AgentPermissionOption {
                id: "allow-once".into(),
                name: "Allow once".into(),
                kind: AgentPermissionOptionKind::AllowOnce,
            }],
        }
    }

    #[tokio::test]
    async fn accepts_only_an_offered_option() {
        let (mut host, broker) = permission_channel(
            NonZeroUsize::new(1).expect("nonzero"),
            Duration::from_secs(1),
        );
        let decision = tokio::spawn(async move { broker.decide(request()).await });
        host.recv()
            .await
            .expect("request")
            .respond(AgentPermissionDecision::Selected("other".into()))
            .expect("respond");
        assert_eq!(
            decision.await.expect("join"),
            AgentPermissionDecision::Cancelled
        );
    }

    #[tokio::test]
    async fn saturation_and_timeout_fail_closed() {
        let (_host, broker) = permission_channel(
            NonZeroUsize::new(1).expect("nonzero"),
            Duration::from_millis(10),
        );
        assert_eq!(
            broker.decide(request()).await,
            AgentPermissionDecision::Cancelled
        );
    }
}
