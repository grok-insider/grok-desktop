//! Live isolation readiness: broker probe + privileged guest health gateway.
//!
//! Strong isolation is ready only when the broker is qualified **and** a
//! journaled `runner.health` guest-control call succeeds under PoP. Failures
//! never enable Work via host-exec fallback.

use std::sync::Arc;

use tokio::sync::RwLock;

use crate::{
    ApplicationError, Clock, IdGenerator, IsolationProbe, IsolationProbeError,
    PrivilegedGateway, PrivilegedGuestControlTransport,
};

/// Snapshot of live isolation readiness facts for capability resolution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IsolationRuntimeFacts {
    /// Static broker probe passed (package/contract identity).
    pub broker_qualified: bool,
    /// Guest-mediated runner.health succeeded with live isolation.
    pub strong_isolation_ready: bool,
}

/// Coordinates probe + privileged gateway without elevating host authority.
pub struct IsolationRuntime {
    probe: Arc<dyn IsolationProbe>,
    gateway: PrivilegedGateway,
    facts: RwLock<IsolationRuntimeFacts>,
    vm_id: String,
    authority_grant_id: String,
}

impl IsolationRuntime {
    /// Builds a runtime coordinator. `transport` must reach the isolation broker.
    #[must_use]
    pub fn new(
        probe: Arc<dyn IsolationProbe>,
        store: Arc<dyn crate::PrivilegedOperationStore>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        transport: Arc<dyn PrivilegedGuestControlTransport>,
        vm_id: impl Into<String>,
        authority_grant_id: impl Into<String>,
    ) -> Self {
        Self {
            probe,
            gateway: PrivilegedGateway::new(store, clock, ids, transport),
            facts: RwLock::new(IsolationRuntimeFacts::default()),
            vm_id: vm_id.into(),
            authority_grant_id: authority_grant_id.into(),
        }
    }

    /// Current facts without re-probing.
    pub async fn facts(&self) -> IsolationRuntimeFacts {
        *self.facts.read().await
    }

    /// Probes the broker and, when qualified, journals a guest health check.
    ///
    /// # Errors
    ///
    /// Returns application errors only for storage/gateway failures after the
    /// broker is considered qualified; probe unavailability clears facts.
    pub async fn refresh(&self, idempotency_key: &str) -> Result<IsolationRuntimeFacts, ApplicationError> {
        let mut next = IsolationRuntimeFacts::default();
        match self.probe.probe().await {
            Ok(_caps) => {
                next.broker_qualified = true;
            }
            Err(IsolationProbeError::Unavailable)
            | Err(IsolationProbeError::Unqualified)
            | Err(IsolationProbeError::Incompatible)
            | Err(IsolationProbeError::Protocol) => {
                *self.facts.write().await = next;
                return Ok(next);
            }
        }

        // Strong isolation requires a successful journaled guest health op.
        match self
            .gateway
            .runner_health(
                &self.authority_grant_id,
                idempotency_key,
                &self.vm_id,
                "proof-of-possession-token-isolation-runtime!!",
            )
            .await
        {
            Ok(result) if !result.interrupted && result.body.is_some() => {
                next.strong_isolation_ready = true;
            }
            Ok(_) | Err(_) => {
                next.strong_isolation_ready = false;
            }
        }
        *self.facts.write().await = next;
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        IsolationBackend, IsolationBrokerCapabilities, IsolationBrokerOperation,
        IsolationContractVersion, IsolationWorkspaceMode, PrivilegedDispatchAttempt,
        PrivilegedGuestControlTransport, PrivilegedGatewayError, PrivilegedOperationStore,
        PrivilegedPreparation, PrivilegedRecoveryCandidate, StoreError,
    };
    use crate::ports::{Clock, IdGenerator};
    use async_trait::async_trait;
    use grok_domain::{
        PrivilegedOperation, PrivilegedOperationId, PrivilegedOperationIntent, UnixMillis,
    };
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct FixedClock;
    impl Clock for FixedClock {
        fn now(&self) -> UnixMillis {
            1_000
        }
    }
    struct SeqIds(Mutex<u64>);
    impl IdGenerator for SeqIds {
        fn generate(&self, prefix: &str) -> String {
            let mut g = self.0.lock().unwrap();
            *g += 1;
            format!("{prefix}-{g:016}")
        }
    }

    struct Probe {
        ok: AtomicBool,
    }
    #[async_trait]
    impl IsolationProbe for Probe {
        async fn probe(
            &self,
        ) -> Result<IsolationBrokerCapabilities, IsolationProbeError> {
            if !self.ok.load(Ordering::SeqCst) {
                return Err(IsolationProbeError::Unavailable);
            }
            Ok(IsolationBrokerCapabilities {
                contract_version: IsolationContractVersion {
                    major: 1,
                    minor: 1,
                    patch: 0,
                },
                backend: IsolationBackend::QemuKvm,
                hcs_schema: String::new(),
                workspace_mode: IsolationWorkspaceMode::ReadOnlyVirtio9p,
                operations: vec![
                    IsolationBrokerOperation::GetCapabilities,
                    IsolationBrokerOperation::EnsureImage,
                    IsolationBrokerOperation::CreateVm,
                    IsolationBrokerOperation::StartVm,
                    IsolationBrokerOperation::StopVm,
                    IsolationBrokerOperation::DeleteVm,
                    IsolationBrokerOperation::AttachWorkspace,
                ],
            })
        }
    }

    struct Transport {
        ok: AtomicBool,
    }
    #[async_trait]
    impl PrivilegedGuestControlTransport for Transport {
        async fn runner_health(&self, _vm_id: &str) -> Result<Vec<u8>, PrivilegedGatewayError> {
            if self.ok.load(Ordering::SeqCst) {
                Ok(br#"{"status":"ok"}"#.to_vec())
            } else {
                Err(PrivilegedGatewayError::Unavailable("guest down".into()))
            }
        }
    }

    #[derive(Default)]
    struct MapStore {
        by_key: Mutex<HashMap<(String, String), PrivilegedOperation>>,
        by_id: Mutex<HashMap<String, PrivilegedOperation>>,
    }
    #[async_trait]
    impl PrivilegedOperationStore for MapStore {
        async fn resolve_preparation(
            &self,
            intent: &PrivilegedOperationIntent,
        ) -> Result<Option<PrivilegedOperation>, StoreError> {
            Ok(self
                .by_key
                .lock()
                .unwrap()
                .get(&(
                    intent.authority.grant_id.as_str().into(),
                    intent.idempotency.key.as_str().into(),
                ))
                .cloned())
        }
        async fn prepare_with_payload(
            &self,
            operation: PrivilegedOperation,
            _payload: Vec<u8>,
        ) -> Result<PrivilegedPreparation, StoreError> {
            let key = (
                operation.authority.grant_id.as_str().into(),
                operation.idempotency.key.as_str().into(),
            );
            let mut by_key = self.by_key.lock().unwrap();
            if let Some(existing) = by_key.get(&key) {
                return Ok(PrivilegedPreparation {
                    operation: existing.clone(),
                    created: false,
                });
            }
            by_key.insert(key, operation.clone());
            self.by_id
                .lock()
                .unwrap()
                .insert(operation.id.as_str().into(), operation.clone());
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
                .unwrap()
                .get(id.as_str())
                .cloned()
                .ok_or(StoreError::NotFound)
        }
        async fn begin_dispatch_with_attempt(
            &self,
            operation: PrivilegedOperation,
            _expected_revision: u64,
            _attempt: PrivilegedDispatchAttempt,
        ) -> Result<PrivilegedOperation, StoreError> {
            self.by_id
                .lock()
                .unwrap()
                .insert(operation.id.as_str().into(), operation.clone());
            let key = (
                operation.authority.grant_id.as_str().into(),
                operation.idempotency.key.as_str().into(),
            );
            self.by_key.lock().unwrap().insert(key, operation.clone());
            Ok(operation)
        }
        async fn list_dispatching_for_recovery(
            &self,
            _limit: usize,
        ) -> Result<Vec<PrivilegedRecoveryCandidate>, StoreError> {
            Ok(vec![])
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
            _expected_revision: u64,
            _attempt_sequence: u32,
            _completed_at: UnixMillis,
        ) -> Result<PrivilegedOperation, StoreError> {
            self.by_id
                .lock()
                .unwrap()
                .insert(operation.id.as_str().into(), operation.clone());
            let key = (
                operation.authority.grant_id.as_str().into(),
                operation.idempotency.key.as_str().into(),
            );
            self.by_key.lock().unwrap().insert(key, operation.clone());
            Ok(operation)
        }
    }

    #[tokio::test]
    async fn strong_isolation_requires_probe_and_guest_health() {
        let probe = Arc::new(Probe {
            ok: AtomicBool::new(true),
        });
        let transport = Arc::new(Transport {
            ok: AtomicBool::new(false),
        });
        let runtime = IsolationRuntime::new(
            probe.clone(),
            Arc::new(MapStore::default()),
            Arc::new(FixedClock),
            Arc::new(SeqIds(Mutex::new(0))),
            transport.clone(),
            "vm-1",
            "authority-grant-0001",
        );
        let facts = runtime
            .refresh("idempotency-key-0001")
            .await
            .expect("refresh");
        assert!(facts.broker_qualified);
        assert!(!facts.strong_isolation_ready);

        transport.ok.store(true, Ordering::SeqCst);
        let facts = runtime
            .refresh("idempotency-key-0002")
            .await
            .expect("refresh2");
        assert!(facts.strong_isolation_ready);

        probe.ok.store(false, Ordering::SeqCst);
        let facts = runtime
            .refresh("idempotency-key-0003")
            .await
            .expect("refresh3");
        assert!(!facts.broker_qualified);
        assert!(!facts.strong_isolation_ready);
    }
}
