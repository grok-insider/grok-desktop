//! Daemon-owned Grok Build host authentication lifecycle.
//!
//! Calls official ACP `authenticate` only. Does not open Work sessions and does
//! not place secrets on IPC.

use std::sync::Arc;

use tokio::sync::RwLock;

use crate::{
    AgentRuntime, AgentRuntimeErrorKind, ApplicationError, Clock,
};

/// Non-secret Grok Build host-auth status for Setup and AccountState.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GrokBuildAuthStatus {
    /// No successful authenticate in this daemon process (or cleared).
    #[default]
    NotAuthenticated,
    /// Authenticate is in flight.
    InProgress,
    /// Official component reported successful authentication.
    Authenticated,
    /// Last attempt failed with a known non-secret reason.
    Failed,
}

/// Process-local durable-enough auth status for the host ACP control surface.
#[derive(Debug, Default)]
pub struct GrokBuildAuthState {
    status: GrokBuildAuthStatus,
    authenticated: bool,
}

/// Coordinates host ACP authenticate with non-secret status projection.
pub struct GrokBuildAuthService {
    runtime: Arc<dyn AgentRuntime>,
    state: RwLock<GrokBuildAuthState>,
    _clock: Arc<dyn Clock>,
}

impl GrokBuildAuthService {
    /// Creates a service bound to a host-control agent runtime.
    #[must_use]
    pub fn new(runtime: Arc<dyn AgentRuntime>, clock: Arc<dyn Clock>) -> Self {
        Self {
            runtime,
            state: RwLock::new(GrokBuildAuthState::default()),
            _clock: clock,
        }
    }

    /// Current non-secret status.
    pub async fn status(&self) -> GrokBuildAuthStatus {
        self.state.read().await.status
    }

    /// Whether subscription_authenticated capability fact should be true.
    pub async fn is_authenticated(&self) -> bool {
        self.state.read().await.authenticated
    }

    /// Starts host ACP authentication using the first advertised method.
    ///
    /// # Errors
    ///
    /// Returns unavailable/invalid when the runtime cannot authenticate.
    pub async fn authenticate(&self) -> Result<GrokBuildAuthStatus, ApplicationError> {
        {
            let mut state = self.state.write().await;
            if state.authenticated {
                return Ok(GrokBuildAuthStatus::Authenticated);
            }
            state.status = GrokBuildAuthStatus::InProgress;
        }

        let probe = self
            .runtime
            .probe()
            .await
            .map_err(|error| map_runtime(error))?;
        let method_id = probe
            .auth_methods
            .first()
            .map(|method| method.id.clone())
            .ok_or_else(|| {
                ApplicationError::Unavailable(
                    "Grok Build advertised no authentication methods".into(),
                )
            })?;

        match self.runtime.authenticate(&method_id).await {
            Ok(()) => {
                let mut state = self.state.write().await;
                state.authenticated = true;
                state.status = GrokBuildAuthStatus::Authenticated;
                Ok(GrokBuildAuthStatus::Authenticated)
            }
            Err(error) => {
                let mut state = self.state.write().await;
                state.authenticated = false;
                state.status = GrokBuildAuthStatus::Failed;
                Err(map_runtime(error))
            }
        }
    }
}

fn map_runtime(error: crate::AgentRuntimeError) -> ApplicationError {
    match error.kind {
        AgentRuntimeErrorKind::Authentication => ApplicationError::Unauthorized(error.message),
        AgentRuntimeErrorKind::InvalidRequest => ApplicationError::InvalidInput(error.message),
        AgentRuntimeErrorKind::ComponentVerification
        | AgentRuntimeErrorKind::Process
        | AgentRuntimeErrorKind::Protocol
        | AgentRuntimeErrorKind::Permission
        | AgentRuntimeErrorKind::Cancelled
        | AgentRuntimeErrorKind::Unavailable => ApplicationError::Unavailable(error.message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AgentAuthMethod, AgentEventStream, AgentPrompt, AgentRuntime, AgentRuntimeCapabilities,
        AgentRuntimeError, AgentRuntimeErrorKind, AgentRuntimeProbe, AgentSession,
        AgentSessionRequest,
    };
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FixedClock;
    impl Clock for FixedClock {
        fn now(&self) -> grok_domain::UnixMillis {
            1
        }
    }

    struct FakeRuntime {
        calls: AtomicUsize,
        fail: bool,
    }

    #[async_trait]
    impl AgentRuntime for FakeRuntime {
        async fn probe(&self) -> Result<AgentRuntimeProbe, AgentRuntimeError> {
            Ok(AgentRuntimeProbe {
                protocol_version: 1,
                agent_name: Some("fake".into()),
                agent_version: Some("0".into()),
                auth_methods: vec![AgentAuthMethod {
                    id: "grok.com".into(),
                    name: "Grok.com OAuth".into(),
                    description: None,
                }],
                capabilities: AgentRuntimeCapabilities::default(),
            })
        }

        async fn authenticate(&self, method_id: &str) -> Result<(), AgentRuntimeError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(method_id, "grok.com");
            if self.fail {
                return Err(AgentRuntimeError {
                    kind: AgentRuntimeErrorKind::Authentication,
                    message: "denied".into(),
                    retryable: true,
                });
            }
            Ok(())
        }

        async fn open_session(
            &self,
            _request: AgentSessionRequest,
        ) -> Result<AgentSession, AgentRuntimeError> {
            Err(AgentRuntimeError {
                kind: AgentRuntimeErrorKind::Unavailable,
                message: "guest only".into(),
                retryable: false,
            })
        }

        async fn prompt(
            &self,
            _prompt: AgentPrompt,
        ) -> Result<AgentEventStream, AgentRuntimeError> {
            Err(AgentRuntimeError {
                kind: AgentRuntimeErrorKind::Unavailable,
                message: "guest only".into(),
                retryable: false,
            })
        }

        async fn cancel(&self, _session_id: &str) -> Result<(), AgentRuntimeError> {
            Ok(())
        }

        async fn shutdown(&self) -> Result<(), AgentRuntimeError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn authenticate_sets_subscription_fact_without_second_call_when_already_done() {
        let runtime = Arc::new(FakeRuntime {
            calls: AtomicUsize::new(0),
            fail: false,
        });
        let service = GrokBuildAuthService::new(runtime.clone(), Arc::new(FixedClock));
        assert!(!service.is_authenticated().await);
        assert_eq!(
            service.authenticate().await.expect("auth"),
            GrokBuildAuthStatus::Authenticated
        );
        assert!(service.is_authenticated().await);
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            service.authenticate().await.expect("replay"),
            GrokBuildAuthStatus::Authenticated
        );
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn authenticate_failure_is_not_authenticated() {
        let runtime = Arc::new(FakeRuntime {
            calls: AtomicUsize::new(0),
            fail: true,
        });
        let service = GrokBuildAuthService::new(runtime, Arc::new(FixedClock));
        assert!(service.authenticate().await.is_err());
        assert!(!service.is_authenticated().await);
        assert_eq!(service.status().await, GrokBuildAuthStatus::Failed);
    }
}
