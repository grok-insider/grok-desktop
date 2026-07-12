//! Typed privileged gateway: journal prepare → dispatch → terminal outcome.
//!
//! Interrupted outcomes are never auto-replayed. The first production method is
//! `runner.health`.

use std::sync::Arc;

use async_trait::async_trait;
use grok_domain::{
    AuthorityGrantId, PayloadDigest, PrivilegedAuthority, PrivilegedIdempotency,
    PrivilegedIdempotencyKey, PrivilegedOperationId, PrivilegedOperationIntent,
    PrivilegedOperationKind, PrivilegedOperationLinks, PrivilegedOperationState,
    PrivilegedOperationTarget, PrivilegedResourceId, RequestDigest, UnixMillis,
};
use sha2::{Digest, Sha256};

use crate::{
    ApplicationError, BeginPrivilegedDispatch, Clock, IdGenerator, PreparePrivilegedOperation,
    PrivilegedOperationService, PrivilegedOperationStore,
};

/// Transport that performs one allowlisted guest-mediated call.
#[async_trait]
pub trait PrivilegedGuestControlTransport: Send + Sync {
    /// Invokes `runner.health` for a running VM after PoP grant.
    async fn runner_health(&self, vm_id: &str) -> Result<Vec<u8>, PrivilegedGatewayError>;
}

/// Failures at the gateway boundary (never secrets).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PrivilegedGatewayError {
    /// Isolation or guest control is not ready.
    #[error("privileged gateway unavailable: {0}")]
    Unavailable(String),
    /// Peer/PoP/authorization failure.
    #[error("privileged gateway unauthorized: {0}")]
    Unauthorized(String),
    /// Invalid arguments or method.
    #[error("privileged gateway invalid: {0}")]
    Invalid(String),
    /// Transport/protocol failure after dispatch began (outcome unknown).
    #[error("privileged gateway transport: {0}")]
    Transport(String),
}

/// Outcome of a gateway invocation after journal coordination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivilegedGatewayResult {
    /// Durable journal identity.
    pub operation_id: PrivilegedOperationId,
    /// Non-secret response body when completed successfully.
    pub body: Option<Vec<u8>>,
    /// True when the journal recorded interrupted/retry-pending instead of success.
    pub interrupted: bool,
}

/// Coordinates durable journal state with one guest-control transport call.
pub struct PrivilegedGateway {
    store: Arc<dyn PrivilegedOperationStore>,
    clock: Arc<dyn Clock>,
    service: PrivilegedOperationService,
    transport: Arc<dyn PrivilegedGuestControlTransport>,
}

impl PrivilegedGateway {
    /// Creates a gateway over an existing privileged journal store.
    #[must_use]
    pub fn new(
        store: Arc<dyn PrivilegedOperationStore>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        transport: Arc<dyn PrivilegedGuestControlTransport>,
    ) -> Self {
        let service =
            PrivilegedOperationService::new(Arc::clone(&store), Arc::clone(&clock), ids);
        Self {
            store,
            clock,
            service,
            transport,
        }
    }

    /// Runs `runner.health` with prepare → begin_dispatch → complete/interrupt.
    ///
    /// # Errors
    ///
    /// Returns application errors for invalid inputs, conflicts, or storage.
    pub async fn runner_health(
        &self,
        authority_grant_id: &str,
        idempotency_key: &str,
        vm_id: &str,
        proof_token: &str,
    ) -> Result<PrivilegedGatewayResult, ApplicationError> {
        if proof_token.len() < 32 {
            return Err(ApplicationError::InvalidInput(
                "proof of possession token must be at least 32 bytes".into(),
            ));
        }
        let vm_resource = PrivilegedResourceId::new(vm_id)
            .map_err(|error| ApplicationError::InvalidInput(error.to_string()))?;
        let payload = serde_json::json!({
            "method": "runner.health",
            "vmId": vm_id,
        })
        .to_string()
        .into_bytes();
        let payload_digest = PayloadDigest::new(Sha256::digest(&payload).into());
        let request_digest = RequestDigest::new(Sha256::digest(&payload).into());
        let expires: UnixMillis = self.clock.now().saturating_add(30_000);
        let intent = PrivilegedOperationIntent::new(
            PrivilegedOperationKind::RunnerHealth,
            PrivilegedOperationTarget::Runner {
                vm_id: vm_resource,
            },
            payload_digest,
            PrivilegedAuthority::new(
                AuthorityGrantId::new(authority_grant_id)
                    .map_err(|error| ApplicationError::InvalidInput(error.to_string()))?,
                expires,
            ),
            PrivilegedIdempotency::new(
                PrivilegedIdempotencyKey::new(idempotency_key)
                    .map_err(|error| ApplicationError::InvalidInput(error.to_string()))?,
                request_digest,
            ),
            PrivilegedOperationLinks::default(),
        );

        let preparation = self
            .service
            .prepare(PreparePrivilegedOperation {
                intent,
                payload: payload.clone(),
            })
            .await?;

        if !preparation.created {
            return Ok(match preparation.operation.state {
                PrivilegedOperationState::Succeeded => PrivilegedGatewayResult {
                    operation_id: preparation.operation.id.clone(),
                    body: None,
                    interrupted: false,
                },
                PrivilegedOperationState::InterruptedNeedsReview
                | PrivilegedOperationState::RetryPending => PrivilegedGatewayResult {
                    operation_id: preparation.operation.id.clone(),
                    body: None,
                    interrupted: true,
                },
                PrivilegedOperationState::Failed => {
                    return Err(ApplicationError::Unavailable(
                        "privileged runner.health previously failed".into(),
                    ));
                }
                PrivilegedOperationState::Prepared
                | PrivilegedOperationState::Dispatching
                | PrivilegedOperationState::Reviewed
                | PrivilegedOperationState::Cancelled => PrivilegedGatewayResult {
                    operation_id: preparation.operation.id.clone(),
                    body: None,
                    interrupted: true,
                },
            });
        }

        let digest = Sha256::digest(
            format!("{}:{}", preparation.operation.id.as_str(), vm_id).as_bytes(),
        );
        let transport_id = format!(
            "transport-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            digest[0],
            digest[1],
            digest[2],
            digest[3],
            digest[4],
            digest[5],
            digest[6],
            digest[7],
            digest[8],
            digest[9],
            digest[10],
            digest[11]
        );

        let mut wire_digest = [0_u8; 32];
        wire_digest.copy_from_slice(&Sha256::digest(&payload));

        let dispatching = self
            .service
            .begin_dispatch(BeginPrivilegedDispatch {
                operation_id: preparation.operation.id.clone(),
                expected_revision: preparation.operation.revision,
                transport_operation_id: transport_id,
                wire_digest,
                broker_boot_id: [1; 16],
                guest_boot_id: [2; 16],
                timeout_ms: 5_000,
            })
            .await?;

        match self.transport.runner_health(vm_id).await {
            Ok(body) => {
                let mut completed = dispatching.clone();
                completed
                    .succeed(self.clock.now())
                    .map_err(|error| ApplicationError::InvalidState(error.to_string()))?;
                let stored = self
                    .store
                    .complete_dispatch_outcome(
                        completed.clone(),
                        dispatching.revision,
                        dispatching.attempt_count,
                        self.clock.now(),
                    )
                    .await?;
                Ok(PrivilegedGatewayResult {
                    operation_id: stored.id.clone(),
                    body: Some(body),
                    interrupted: false,
                })
            }
            Err(PrivilegedGatewayError::Transport(_)) => {
                let mut interrupted = dispatching.clone();
                interrupted
                    .interrupt(self.clock.now())
                    .map_err(|error| ApplicationError::InvalidState(error.to_string()))?;
                let stored = self
                    .store
                    .complete_dispatch_outcome(
                        interrupted.clone(),
                        dispatching.revision,
                        dispatching.attempt_count,
                        self.clock.now(),
                    )
                    .await?;
                Ok(PrivilegedGatewayResult {
                    operation_id: stored.id.clone(),
                    body: None,
                    interrupted: true,
                })
            }
            Err(other) => {
                let mut failed = dispatching.clone();
                failed
                    .fail(self.clock.now())
                    .map_err(|error| ApplicationError::InvalidState(error.to_string()))?;
                let _ = self
                    .store
                    .complete_dispatch_outcome(
                        failed,
                        dispatching.revision,
                        dispatching.attempt_count,
                        self.clock.now(),
                    )
                    .await?;
                Err(ApplicationError::Unavailable(other.to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        PrivilegedDispatchAttempt, PrivilegedPreparation, PrivilegedRecoveryCandidate, StoreError,
    };
    use crate::ports::{Clock, IdGenerator};
    use grok_domain::PrivilegedOperation;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FixedClock(Mutex<u64>);
    impl Clock for FixedClock {
        fn now(&self) -> UnixMillis {
            *self.0.lock().expect("clock")
        }
    }

    struct SeqIds(Mutex<u64>);
    impl IdGenerator for SeqIds {
        fn generate(&self, prefix: &str) -> String {
            let mut guard = self.0.lock().expect("ids");
            *guard += 1;
            format!("{prefix}-{guard:016}")
        }
    }

    #[derive(Default)]
    struct MapStore {
        by_key: Mutex<HashMap<(String, String), PrivilegedOperation>>,
        by_id: Mutex<HashMap<String, PrivilegedOperation>>,
        payloads: Mutex<HashMap<String, Vec<u8>>>,
        attempts: Mutex<HashMap<String, Vec<PrivilegedDispatchAttempt>>>,
        transports: Mutex<std::collections::HashSet<String>>,
    }

    #[async_trait]
    impl PrivilegedOperationStore for MapStore {
        async fn resolve_preparation(
            &self,
            intent: &PrivilegedOperationIntent,
        ) -> Result<Option<PrivilegedOperation>, StoreError> {
            let key = (
                intent.authority.grant_id.as_str().to_owned(),
                intent.idempotency.key.as_str().to_owned(),
            );
            Ok(self.by_key.lock().expect("lock").get(&key).cloned())
        }

        async fn prepare_with_payload(
            &self,
            operation: PrivilegedOperation,
            payload: Vec<u8>,
        ) -> Result<PrivilegedPreparation, StoreError> {
            let key = (
                operation.authority.grant_id.as_str().to_owned(),
                operation.idempotency.key.as_str().to_owned(),
            );
            let mut by_key = self.by_key.lock().expect("lock");
            if let Some(existing) = by_key.get(&key) {
                return Ok(PrivilegedPreparation {
                    operation: existing.clone(),
                    created: false,
                });
            }
            by_key.insert(key, operation.clone());
            self.by_id
                .lock()
                .expect("lock")
                .insert(operation.id.as_str().to_owned(), operation.clone());
            self.payloads
                .lock()
                .expect("lock")
                .insert(operation.id.as_str().to_owned(), payload);
            Ok(PrivilegedPreparation {
                operation,
                created: true,
            })
        }

        async fn get_privileged_operation(
            &self,
            id: &PrivilegedOperationId,
        ) -> Result<PrivilegedOperation, StoreError> {
            self.by_id
                .lock()
                .expect("lock")
                .get(id.as_str())
                .cloned()
                .ok_or(StoreError::NotFound)
        }

        async fn begin_dispatch_with_attempt(
            &self,
            operation: PrivilegedOperation,
            expected_revision: u64,
            attempt: PrivilegedDispatchAttempt,
        ) -> Result<PrivilegedOperation, StoreError> {
            let mut by_id = self.by_id.lock().expect("lock");
            let current = by_id
                .get(operation.id.as_str())
                .cloned()
                .ok_or(StoreError::NotFound)?;
            if current.revision != expected_revision {
                return Err(StoreError::Conflict);
            }
            let mut transports = self.transports.lock().expect("lock");
            if !transports.insert(attempt.transport_operation_id.clone()) {
                return Err(StoreError::Conflict);
            }
            self.attempts
                .lock()
                .expect("lock")
                .entry(operation.id.as_str().to_owned())
                .or_default()
                .push(attempt);
            by_id.insert(operation.id.as_str().to_owned(), operation.clone());
            let key = (
                operation.authority.grant_id.as_str().to_owned(),
                operation.idempotency.key.as_str().to_owned(),
            );
            self.by_key.lock().expect("lock").insert(key, operation.clone());
            Ok(operation)
        }

        async fn list_dispatching_for_recovery(
            &self,
            _limit: usize,
        ) -> Result<Vec<PrivilegedRecoveryCandidate>, StoreError> {
            Ok(Vec::new())
        }

        async fn recover_interrupted_attempt(
            &self,
            operation: PrivilegedOperation,
            _expected_revision: u64,
            _attempt_sequence: u32,
            _completed_at: UnixMillis,
        ) -> Result<PrivilegedOperation, StoreError> {
            Ok(operation)
        }

        async fn complete_dispatch_outcome(
            &self,
            operation: PrivilegedOperation,
            expected_revision: u64,
            attempt_sequence: u32,
            _completed_at: UnixMillis,
        ) -> Result<PrivilegedOperation, StoreError> {
            let mut by_id = self.by_id.lock().expect("lock");
            let current = by_id
                .get(operation.id.as_str())
                .cloned()
                .ok_or(StoreError::NotFound)?;
            if current.revision != expected_revision || current.attempt_count != attempt_sequence {
                return Err(StoreError::Conflict);
            }
            by_id.insert(operation.id.as_str().to_owned(), operation.clone());
            let key = (
                operation.authority.grant_id.as_str().to_owned(),
                operation.idempotency.key.as_str().to_owned(),
            );
            self.by_key.lock().expect("lock").insert(key, operation.clone());
            Ok(operation)
        }
    }

    struct CountingTransport {
        calls: AtomicUsize,
        fail_transport: bool,
    }

    #[async_trait]
    impl PrivilegedGuestControlTransport for CountingTransport {
        async fn runner_health(&self, vm_id: &str) -> Result<Vec<u8>, PrivilegedGatewayError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_transport {
                return Err(PrivilegedGatewayError::Transport("broken pipe".into()));
            }
            Ok(format!(r#"{{"status":"ok","vm":"{vm_id}"}}"#).into_bytes())
        }
    }

    #[tokio::test]
    async fn runner_health_journals_and_does_not_replay_transport_on_exact_key() {
        let store = Arc::new(MapStore::default());
        let transport = Arc::new(CountingTransport {
            calls: AtomicUsize::new(0),
            fail_transport: false,
        });
        let gateway = PrivilegedGateway::new(
            store,
            Arc::new(FixedClock(Mutex::new(1_000))),
            Arc::new(SeqIds(Mutex::new(0))),
            transport.clone(),
        );
        let first = gateway
            .runner_health(
                "authority-grant-0001",
                "idempotency-key-0001",
                "vm-1",
                "proof-of-possession-token-32bytes!!",
            )
            .await
            .expect("first");
        assert!(!first.interrupted);
        assert!(first.body.is_some());
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);

        let second = gateway
            .runner_health(
                "authority-grant-0001",
                "idempotency-key-0001",
                "vm-1",
                "proof-of-possession-token-32bytes!!",
            )
            .await
            .expect("replay");
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
        assert_eq!(second.operation_id, first.operation_id);
    }

    #[tokio::test]
    async fn transport_break_after_dispatch_is_interrupted_without_auto_replay() {
        let store = Arc::new(MapStore::default());
        let transport = Arc::new(CountingTransport {
            calls: AtomicUsize::new(0),
            fail_transport: true,
        });
        let gateway = PrivilegedGateway::new(
            store,
            Arc::new(FixedClock(Mutex::new(1_000))),
            Arc::new(SeqIds(Mutex::new(0))),
            transport.clone(),
        );
        let result = gateway
            .runner_health(
                "authority-grant-0002",
                "idempotency-key-0002",
                "vm-2",
                "proof-of-possession-token-32bytes!!",
            )
            .await
            .expect("interrupted path");
        assert!(result.interrupted);
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);

        let again = gateway
            .runner_health(
                "authority-grant-0002",
                "idempotency-key-0002",
                "vm-2",
                "proof-of-possession-token-32bytes!!",
            )
            .await
            .expect("replay interrupted");
        assert!(again.interrupted);
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    }
}
