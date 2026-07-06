use std::sync::Arc;

use async_trait::async_trait;
use futures_util::lock::Mutex;
use thiserror::Error;

use crate::{
    AccountState, ApplicationError, CredentialMutationReservation, CredentialMutationStore,
    CredentialService, SecretValue, mutations::mutation_command_bytes,
};

/// Opaque native window token supplied by the trusted desktop host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CredentialEnrollmentRequest {
    /// Platform adapter interprets this value; application code never dereferences it.
    pub parent_window_token: u64,
}

/// Stable failure at the native credential-entry boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CredentialEnrollmentError {
    /// The person dismissed the native prompt without submitting a value.
    #[error("credential enrollment was cancelled")]
    Cancelled,
    /// The qualified native credential UI is unavailable.
    #[error("native credential enrollment is unavailable")]
    Unavailable,
    /// The packaged process or native UI boundary failed an integrity check.
    #[error("native credential enrollment integrity check failed")]
    Integrity,
}

/// Collects a secret through a qualified native UI without crossing renderer IPC.
#[async_trait]
pub trait CredentialEnrollment: Send + Sync {
    /// Returns a short-lived, zeroizing credential value owned by the daemon.
    ///
    /// # Errors
    ///
    /// Returns a stable error when the user cancels or the native boundary fails.
    async fn collect_xai_api_key(
        &self,
        request: CredentialEnrollmentRequest,
    ) -> Result<SecretValue, CredentialEnrollmentError>;
}

/// Durable application use case for renderer-free xAI credential enrollment.
pub struct CredentialEnrollmentService {
    credentials: Arc<CredentialService>,
    mutations: Arc<dyn CredentialMutationStore>,
    enrollment: Arc<dyn CredentialEnrollment>,
    enrollment_lock: Mutex<()>,
}

impl CredentialEnrollmentService {
    /// Creates the enrollment coordinator from narrow application and platform ports.
    #[must_use]
    pub fn new(
        credentials: Arc<CredentialService>,
        mutations: Arc<dyn CredentialMutationStore>,
        enrollment: Arc<dyn CredentialEnrollment>,
    ) -> Self {
        Self {
            credentials,
            mutations,
            enrollment,
            enrollment_lock: Mutex::new(()),
        }
    }

    /// Reserves an enrollment command before opening native UI, then validates and stores
    /// the collected key before recording the command's non-secret canonical result.
    ///
    /// Completed commands replay without opening native UI. A pending replay
    /// reconciles only when that command's exact local generation is already
    /// installed; it never reopens native entry under the same generation.
    ///
    /// # Errors
    ///
    /// Returns an application error when the command is invalid or conflicting, native entry
    /// fails, xAI rejects the key, or the durable journal and vault cannot commit the result.
    pub async fn enroll_xai_api_key(
        &self,
        request: CredentialEnrollmentRequest,
        idempotency_key: &str,
    ) -> Result<AccountState, ApplicationError> {
        let command = mutation_command_bytes("enroll_xai_api_key", idempotency_key, &[])?;
        if let Some(CredentialMutationReservation::Completed(outcome)) =
            self.mutations.resolve_credential_mutation(&command).await?
        {
            return Ok(outcome);
        }
        let _enrollment_guard = self.enrollment_lock.lock().await;
        match self.mutations.begin_credential_mutation(&command).await? {
            CredentialMutationReservation::Completed(outcome) => return Ok(outcome),
            CredentialMutationReservation::Pending => {
                return self
                    .credentials
                    .reconcile_pending_xai_enrollment(&command)
                    .await;
            }
            CredentialMutationReservation::NewlyReserved => {}
        }
        let api_key = self
            .enrollment
            .collect_xai_api_key(request)
            .await
            .map_err(map_enrollment_error)?;
        let outcome = self
            .credentials
            .validate_and_store_xai_api_key(api_key, &command)
            .await?;
        self.mutations
            .complete_credential_mutation(&command, outcome)
            .await?;
        Ok(outcome)
    }
}

fn map_enrollment_error(error: CredentialEnrollmentError) -> ApplicationError {
    match error {
        CredentialEnrollmentError::Cancelled => ApplicationError::Cancelled,
        CredentialEnrollmentError::Unavailable => {
            ApplicationError::Unavailable("native credential enrollment is unavailable".into())
        }
        CredentialEnrollmentError::Integrity => {
            ApplicationError::Integrity("native credential UI identity validation failed".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, VecDeque},
        sync::{
            Arc, Mutex as StdMutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    use async_trait::async_trait;

    use super::*;
    use crate::{
        MutationCommand, SecretName, SecretVault, StoreError, VaultError, XaiApiKeyValidation,
        XaiApiKeyValidationError, XaiApiKeyValidator,
    };

    #[derive(Debug, Clone)]
    struct MutationRecord {
        fingerprint: [u8; 32],
        outcome: Option<AccountState>,
    }

    #[derive(Debug, Default)]
    struct MemoryMutationStore {
        records: StdMutex<HashMap<(String, String), MutationRecord>>,
        begin_calls: AtomicUsize,
        fail_completion_once: AtomicBool,
    }

    impl MemoryMutationStore {
        fn fail_next_completion(&self) {
            self.fail_completion_once.store(true, Ordering::SeqCst);
        }

        fn reservation(&self, key: &str) -> Option<CredentialMutationReservation> {
            self.records
                .lock()
                .expect("mutation records")
                .get(&("enroll_xai_api_key".into(), key.into()))
                .map(|record| {
                    record.outcome.map_or(
                        CredentialMutationReservation::Pending,
                        CredentialMutationReservation::Completed,
                    )
                })
        }
    }

    #[async_trait]
    impl CredentialMutationStore for MemoryMutationStore {
        async fn resolve_credential_mutation(
            &self,
            command: &MutationCommand,
        ) -> Result<Option<CredentialMutationReservation>, StoreError> {
            let records = self.records.lock().map_err(|_| {
                StoreError::Internal("test mutation store lock was poisoned".into())
            })?;
            let Some(record) = records.get(&(command.scope.clone(), command.key.clone())) else {
                return Ok(None);
            };
            if record.fingerprint != command.fingerprint {
                return Err(StoreError::Conflict);
            }
            Ok(Some(record.outcome.map_or(
                CredentialMutationReservation::Pending,
                CredentialMutationReservation::Completed,
            )))
        }

        async fn begin_credential_mutation(
            &self,
            command: &MutationCommand,
        ) -> Result<CredentialMutationReservation, StoreError> {
            self.begin_calls.fetch_add(1, Ordering::SeqCst);
            let mut records = self.records.lock().map_err(|_| {
                StoreError::Internal("test mutation store lock was poisoned".into())
            })?;
            let record = records.get(&(command.scope.clone(), command.key.clone()));
            if let Some(record) = record {
                if record.fingerprint != command.fingerprint {
                    return Err(StoreError::Conflict);
                }
                return Ok(record.outcome.map_or(
                    CredentialMutationReservation::Pending,
                    CredentialMutationReservation::Completed,
                ));
            }
            records.insert(
                (command.scope.clone(), command.key.clone()),
                MutationRecord {
                    fingerprint: command.fingerprint,
                    outcome: None,
                },
            );
            Ok(CredentialMutationReservation::NewlyReserved)
        }

        async fn complete_credential_mutation(
            &self,
            command: &MutationCommand,
            outcome: AccountState,
        ) -> Result<(), StoreError> {
            if self.fail_completion_once.swap(false, Ordering::SeqCst) {
                return Err(StoreError::Internal(
                    "injected credential completion failure".into(),
                ));
            }
            let mut records = self.records.lock().map_err(|_| {
                StoreError::Internal("test mutation store lock was poisoned".into())
            })?;
            let record = records
                .get_mut(&(command.scope.clone(), command.key.clone()))
                .ok_or(StoreError::NotFound)?;
            if record.fingerprint != command.fingerprint {
                return Err(StoreError::Conflict);
            }
            match record.outcome {
                Some(existing) if existing != outcome => Err(StoreError::Conflict),
                Some(_) => Ok(()),
                None => {
                    record.outcome = Some(outcome);
                    Ok(())
                }
            }
        }
    }

    #[derive(Debug, Default)]
    struct MemoryVault {
        entries: StdMutex<HashMap<SecretName, SecretValue>>,
    }

    impl SecretVault for MemoryVault {
        fn get(&self, name: &SecretName) -> Result<SecretValue, VaultError> {
            self.entries
                .lock()
                .map_err(|_| VaultError::Internal)?
                .get(name)
                .cloned()
                .ok_or(VaultError::NotFound)
        }

        fn set(&self, name: &SecretName, value: &SecretValue) -> Result<(), VaultError> {
            self.entries
                .lock()
                .map_err(|_| VaultError::Internal)?
                .insert(name.clone(), value.clone());
            Ok(())
        }

        fn delete(&self, name: &SecretName) -> Result<(), VaultError> {
            self.entries
                .lock()
                .map_err(|_| VaultError::Internal)?
                .remove(name);
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct AcceptKey;

    #[async_trait]
    impl XaiApiKeyValidator for AcceptKey {
        async fn validate(
            &self,
            _api_key: &SecretValue,
        ) -> Result<XaiApiKeyValidation, XaiApiKeyValidationError> {
            Ok(XaiApiKeyValidation::CapabilitiesResolved)
        }
    }

    #[derive(Debug)]
    struct SequenceEnrollment {
        outcomes: StdMutex<VecDeque<Result<Vec<u8>, CredentialEnrollmentError>>>,
        calls: AtomicUsize,
    }

    impl SequenceEnrollment {
        fn new(
            outcomes: impl IntoIterator<Item = Result<Vec<u8>, CredentialEnrollmentError>>,
        ) -> Self {
            Self {
                outcomes: StdMutex::new(outcomes.into_iter().collect()),
                calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl CredentialEnrollment for SequenceEnrollment {
        async fn collect_xai_api_key(
            &self,
            _request: CredentialEnrollmentRequest,
        ) -> Result<SecretValue, CredentialEnrollmentError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            let outcome = self
                .outcomes
                .lock()
                .map_err(|_| CredentialEnrollmentError::Unavailable)?
                .pop_front()
                .ok_or(CredentialEnrollmentError::Unavailable)??;
            SecretValue::new(outcome).map_err(|_| CredentialEnrollmentError::Integrity)
        }
    }

    fn service(
        enrollment: Arc<SequenceEnrollment>,
    ) -> (
        CredentialEnrollmentService,
        Arc<MemoryMutationStore>,
        Arc<MemoryVault>,
        Arc<CredentialService>,
    ) {
        let mutations = Arc::new(MemoryMutationStore::default());
        let vault = Arc::new(MemoryVault::default());
        let credentials = Arc::new(CredentialService::new(
            vault.clone(),
            mutations.clone(),
            Arc::new(AcceptKey),
        ));
        (
            CredentialEnrollmentService::new(credentials.clone(), mutations.clone(), enrollment),
            mutations,
            vault,
            credentials,
        )
    }

    const fn request(parent_window_token: u64) -> CredentialEnrollmentRequest {
        CredentialEnrollmentRequest {
            parent_window_token,
        }
    }

    fn synthetic_key(seed: u8) -> Vec<u8> {
        vec![seed; 32]
    }

    #[tokio::test]
    async fn completed_command_replays_without_another_prompt() {
        let enrollment = Arc::new(SequenceEnrollment::new([Ok(synthetic_key(7))]));
        let (service, mutations, _, credentials) = service(enrollment.clone());

        let first = service
            .enroll_xai_api_key(request(11), "command-1")
            .await
            .expect("first enrollment");
        drop(service);
        let restarted_enrollment = Arc::new(SequenceEnrollment::new([]));
        let restarted = CredentialEnrollmentService::new(
            credentials,
            mutations.clone(),
            restarted_enrollment.clone(),
        );
        let replay = restarted
            .enroll_xai_api_key(request(22), "command-1")
            .await
            .expect("replayed enrollment");

        assert_eq!(replay, first);
        assert_eq!(enrollment.calls.load(Ordering::SeqCst), 1);
        assert_eq!(restarted_enrollment.calls.load(Ordering::SeqCst), 0);
        assert_eq!(mutations.begin_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            mutations.reservation("command-1"),
            Some(CredentialMutationReservation::Completed(first))
        );
    }

    #[tokio::test]
    async fn invalid_idempotency_key_is_rejected_before_prompt_or_reservation() {
        let enrollment = Arc::new(SequenceEnrollment::new([Ok(synthetic_key(7))]));
        let (service, mutations, _, _) = service(enrollment.clone());

        assert!(matches!(
            service.enroll_xai_api_key(request(11), "").await,
            Err(ApplicationError::InvalidInput(_))
        ));
        assert_eq!(enrollment.calls.load(Ordering::SeqCst), 0);
        assert_eq!(mutations.begin_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn cancelled_command_cannot_reopen_the_prompt_under_the_same_generation() {
        let enrollment = Arc::new(SequenceEnrollment::new([
            Err(CredentialEnrollmentError::Cancelled),
            Ok(synthetic_key(8)),
        ]));
        let (service, mutations, _, _) = service(enrollment.clone());

        assert!(matches!(
            service.enroll_xai_api_key(request(11), "command-1").await,
            Err(ApplicationError::Cancelled)
        ));
        assert_eq!(
            mutations.reservation("command-1"),
            Some(CredentialMutationReservation::Pending)
        );

        assert!(matches!(
            service.enroll_xai_api_key(request(11), "command-1").await,
            Err(ApplicationError::Integrity(_))
        ));
        assert_eq!(enrollment.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            mutations.reservation("command-1"),
            Some(CredentialMutationReservation::Pending)
        );

        let outcome = service
            .enroll_xai_api_key(request(11), "command-2")
            .await
            .expect("fresh enrollment command");
        assert!(outcome.xai_api_key_configured);
        assert_eq!(enrollment.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn vault_success_with_lost_completion_reconciles_without_another_prompt() {
        let first_key = synthetic_key(10);
        let enrollment = Arc::new(SequenceEnrollment::new([Ok(first_key.clone())]));
        let (service, mutations, vault, credentials) = service(enrollment.clone());
        mutations.fail_next_completion();

        assert!(matches!(
            service.enroll_xai_api_key(request(11), "command-1").await,
            Err(ApplicationError::Storage(_))
        ));
        assert_eq!(
            mutations.reservation("command-1"),
            Some(CredentialMutationReservation::Pending)
        );
        assert_eq!(
            vault
                .get(&SecretName::new("xai.api-key.primary").expect("key name"))
                .expect("installed ambiguous key")
                .expose_secret(),
            first_key
        );

        let second_prompt = Arc::new(SequenceEnrollment::new([Ok(synthetic_key(11))]));
        let restarted = CredentialEnrollmentService::new(
            credentials.clone(),
            mutations.clone(),
            second_prompt.clone(),
        );
        let reconciled = restarted
            .enroll_xai_api_key(request(22), "command-1")
            .await
            .expect("reconcile installed generation");
        assert!(reconciled.xai_api_key_configured);
        assert_eq!(second_prompt.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            vault
                .get(&SecretName::new("xai.api-key.primary").expect("key name"))
                .expect("reconciled key")
                .expose_secret(),
            first_key
        );
        assert_eq!(
            mutations.reservation("command-1"),
            Some(CredentialMutationReservation::Completed(reconciled))
        );
    }

    #[tokio::test]
    async fn concurrent_retries_share_one_prompt_and_one_completed_result() {
        let enrollment = Arc::new(SequenceEnrollment::new([Ok(synthetic_key(9))]));
        let (service, _, _, _) = service(enrollment.clone());

        let (first, second) = tokio::join!(
            service.enroll_xai_api_key(request(11), "command-1"),
            service.enroll_xai_api_key(request(22), "command-1")
        );

        assert_eq!(first.expect("first result"), second.expect("second result"));
        assert_eq!(enrollment.calls.load(Ordering::SeqCst), 1);
    }
}
