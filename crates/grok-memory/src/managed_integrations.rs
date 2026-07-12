use std::{collections::HashMap, sync::Mutex};

use async_trait::async_trait;
use grok_application::{
    ApplyManagedIntegrationLifecycle, MAX_MANAGED_INTEGRATION_RECOVERY_BATCH,
    ManagedIntegrationLifecycle, ManagedIntegrationLifecycleCommit,
    ManagedIntegrationLifecycleStore, ManagedIntegrationMutation, ManagedIntegrationPhase,
    ManagedIntegrationRecoveryEntry, StoreError,
};
use grok_domain::UnixMillis;

#[derive(Clone)]
struct JournalEntry {
    request_fingerprint: [u8; 32],
    lifecycle: ManagedIntegrationLifecycle,
    mutation: ManagedIntegrationMutation,
    published_at: Option<UnixMillis>,
}

#[derive(Default)]
struct State {
    lifecycles: HashMap<String, ManagedIntegrationLifecycle>,
    journal: HashMap<String, JournalEntry>,
}

/// Deterministic in-memory parity adapter for managed-integration lifecycle tests.
#[derive(Default)]
pub struct InMemoryManagedIntegrationLifecycleStore {
    state: Mutex<State>,
}

impl InMemoryManagedIntegrationLifecycleStore {
    /// Creates an empty lifecycle store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ManagedIntegrationLifecycleStore for InMemoryManagedIntegrationLifecycleStore {
    async fn get_managed_integration(
        &self,
        integration_id: &str,
    ) -> Result<Option<ManagedIntegrationLifecycle>, StoreError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| StoreError::Internal("managed integration store lock poisoned".into()))?
            .lifecycles
            .get(integration_id)
            .cloned())
    }

    async fn get_published_managed_integration(
        &self,
        integration_id: &str,
    ) -> Result<Option<ManagedIntegrationLifecycle>, StoreError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| StoreError::Internal("managed integration store lock poisoned".into()))?
            .journal
            .values()
            .filter(|entry| {
                entry.published_at.is_some() && entry.lifecycle.integration_id == integration_id
            })
            .max_by_key(|entry| entry.lifecycle.revision)
            .map(|entry| entry.lifecycle.clone()))
    }

    async fn apply_managed_integration_lifecycle(
        &self,
        command: ApplyManagedIntegrationLifecycle,
    ) -> Result<ManagedIntegrationLifecycleCommit, StoreError> {
        validate_command(&command)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| StoreError::Internal("managed integration store lock poisoned".into()))?;
        if let Some(entry) = state.journal.get(&command.idempotency_key) {
            if entry.request_fingerprint != command.request_fingerprint {
                return Err(StoreError::Conflict);
            }
            return Ok(ManagedIntegrationLifecycleCommit {
                lifecycle: entry.lifecycle.clone(),
                replayed: true,
            });
        }
        if state.journal.values().any(|entry| {
            entry.lifecycle.integration_id == command.integration_id && entry.published_at.is_none()
        }) {
            return Err(StoreError::Conflict);
        }

        let current = state.lifecycles.get(&command.integration_id).cloned();
        let revision = current.as_ref().map_or(0, |value| value.revision);
        if revision != command.expected_revision {
            return Err(StoreError::Conflict);
        }
        let next_revision = revision.checked_add(1).ok_or(StoreError::Conflict)?;
        let lifecycle = transition(&command, current.as_ref(), next_revision)?;
        state
            .lifecycles
            .insert(command.integration_id.clone(), lifecycle.clone());
        state.journal.insert(
            command.idempotency_key,
            JournalEntry {
                request_fingerprint: command.request_fingerprint,
                lifecycle: lifecycle.clone(),
                mutation: command.mutation,
                published_at: None,
            },
        );
        Ok(ManagedIntegrationLifecycleCommit {
            lifecycle,
            replayed: false,
        })
    }

    async fn pending_managed_integration_publications(
        &self,
        limit: usize,
    ) -> Result<Vec<ManagedIntegrationRecoveryEntry>, StoreError> {
        if limit == 0 || limit > MAX_MANAGED_INTEGRATION_RECOVERY_BATCH + 1 {
            return Err(StoreError::Conflict);
        }
        let state = self
            .state
            .lock()
            .map_err(|_| StoreError::Internal("managed integration store lock poisoned".into()))?;
        let mut rows: Vec<_> = state
            .journal
            .iter()
            .filter(|(_, entry)| entry.published_at.is_none())
            .map(|(key, entry)| ManagedIntegrationRecoveryEntry {
                idempotency_key: key.clone(),
                integration_id: entry.lifecycle.integration_id.clone(),
                mutation: entry.mutation,
                committed_revision: entry.lifecycle.revision,
                candidate_manifest_digest: entry.lifecycle.available_manifest_digest,
            })
            .collect();
        rows.sort_by(|left, right| left.idempotency_key.cmp(&right.idempotency_key));
        rows.truncate(limit);
        Ok(rows)
    }

    async fn acknowledge_managed_integration_publication(
        &self,
        idempotency_key: &str,
        committed_revision: u64,
        published_at: UnixMillis,
    ) -> Result<(), StoreError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| StoreError::Internal("managed integration store lock poisoned".into()))?;
        let entry = state
            .journal
            .get_mut(idempotency_key)
            .ok_or(StoreError::NotFound)?;
        if entry.lifecycle.revision != committed_revision {
            return Err(StoreError::Conflict);
        }
        match entry.published_at {
            Some(existing) if existing != published_at => Err(StoreError::Conflict),
            Some(_) => Ok(()),
            None => {
                entry.published_at = Some(published_at);
                Ok(())
            }
        }
    }
}

fn validate_command(command: &ApplyManagedIntegrationLifecycle) -> Result<(), StoreError> {
    if command.idempotency_key.is_empty()
        || command.idempotency_key.len() > 128
        || command.integration_id.is_empty()
        || command.integration_id.len() > 128
        || command.candidate_version.is_empty()
        || command.candidate_version.len() > 64
    {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

fn transition(
    command: &ApplyManagedIntegrationLifecycle,
    current: Option<&ManagedIntegrationLifecycle>,
    revision: u64,
) -> Result<ManagedIntegrationLifecycle, StoreError> {
    let (phase, installed_version, installed_digest, rollback_version, rollback_digest) =
        match command.mutation {
            ManagedIntegrationMutation::Install if current.is_none() => (
                ManagedIntegrationPhase::Installed,
                Some(command.candidate_version.clone()),
                Some(command.candidate_manifest_digest),
                None,
                None,
            ),
            ManagedIntegrationMutation::Update => {
                let current = current.ok_or(StoreError::Conflict)?;
                let installed = current
                    .installed_version
                    .clone()
                    .ok_or(StoreError::Conflict)?;
                let digest = current
                    .installed_manifest_digest
                    .ok_or(StoreError::Conflict)?;
                if installed == command.candidate_version {
                    return Err(StoreError::Conflict);
                }
                (
                    ManagedIntegrationPhase::RollbackAvailable,
                    Some(command.candidate_version.clone()),
                    Some(command.candidate_manifest_digest),
                    Some(installed),
                    Some(digest),
                )
            }
            ManagedIntegrationMutation::Rollback => {
                let current = current.ok_or(StoreError::Conflict)?;
                let rollback = current
                    .rollback_version
                    .clone()
                    .ok_or(StoreError::Conflict)?;
                let digest = current
                    .rollback_manifest_digest
                    .ok_or(StoreError::Conflict)?;
                (
                    ManagedIntegrationPhase::UpdateAvailable,
                    Some(rollback),
                    Some(digest),
                    None,
                    None,
                )
            }
            ManagedIntegrationMutation::Install => return Err(StoreError::Conflict),
        };
    Ok(ManagedIntegrationLifecycle {
        integration_id: command.integration_id.clone(),
        phase,
        installed_version,
        installed_manifest_digest: installed_digest,
        available_version: command.candidate_version.clone(),
        available_manifest_digest: command.candidate_manifest_digest,
        rollback_version,
        rollback_manifest_digest: rollback_digest,
        revision,
        updated_at: command.observed_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install(key: &str) -> ApplyManagedIntegrationLifecycle {
        ApplyManagedIntegrationLifecycle {
            idempotency_key: key.into(),
            request_fingerprint: [1; 32],
            integration_id: "desktop.grok.wisp".into(),
            mutation: ManagedIntegrationMutation::Install,
            expected_revision: 0,
            candidate_version: "1.0.0".into(),
            candidate_manifest_digest: [2; 32],
            observed_at: 100,
        }
    }

    #[tokio::test]
    async fn exact_replay_is_stable_and_mismatch_conflicts() {
        let store = InMemoryManagedIntegrationLifecycleStore::new();
        let command = install("install-1");
        let first = store
            .apply_managed_integration_lifecycle(command.clone())
            .await
            .expect("first commit");
        assert!(!first.replayed);
        let replay = store
            .apply_managed_integration_lifecycle(command.clone())
            .await
            .expect("replay");
        assert!(replay.replayed);
        let mut forged = command;
        forged.request_fingerprint = [9; 32];
        assert_eq!(
            store.apply_managed_integration_lifecycle(forged).await,
            Err(StoreError::Conflict)
        );
    }

    #[tokio::test]
    async fn publication_recovery_requires_exact_revision() {
        let store = InMemoryManagedIntegrationLifecycleStore::new();
        let committed = store
            .apply_managed_integration_lifecycle(install("install-1"))
            .await
            .expect("commit");
        assert_eq!(
            store
                .pending_managed_integration_publications(10)
                .await
                .expect("pending")
                .len(),
            1
        );
        let published_at = 101;
        assert_eq!(
            store
                .acknowledge_managed_integration_publication("install-1", 99, published_at)
                .await,
            Err(StoreError::Conflict)
        );
        store
            .acknowledge_managed_integration_publication(
                "install-1",
                committed.lifecycle.revision,
                published_at,
            )
            .await
            .expect("acknowledge");
        assert!(
            store
                .pending_managed_integration_publications(10)
                .await
                .expect("pending")
                .is_empty()
        );
    }
}
