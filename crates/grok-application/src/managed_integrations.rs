//! Durable lifecycle contract for signed, out-of-process managed integrations.

use async_trait::async_trait;
use grok_domain::UnixMillis;

use crate::StoreError;

/// Maximum integration records or journal rows returned by one recovery page.
pub const MAX_MANAGED_INTEGRATION_RECOVERY_BATCH: usize = 100;

/// Stable lifecycle phase persisted by the daemon store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedIntegrationPhase {
    /// No published bundle exists.
    Available,
    /// One verified bundle is published.
    Installed,
    /// A newer verified bundle can replace the published bundle.
    UpdateAvailable,
    /// The immediately previous verified bundle can be restored.
    RollbackAvailable,
}

/// A lifecycle mutation whose identity is journaled before publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedIntegrationMutation {
    /// Publish the first verified bundle.
    Install,
    /// Replace the current bundle and retain it as rollback material.
    Update,
    /// Restore the retained previous bundle.
    Rollback,
}

/// Canonical durable state for one integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedIntegrationLifecycle {
    /// Signed manifest integration identifier.
    pub integration_id: String,
    /// Current lifecycle phase.
    pub phase: ManagedIntegrationPhase,
    /// Published version, if installed.
    pub installed_version: Option<String>,
    /// Digest of the exact published manifest signing payload.
    pub installed_manifest_digest: Option<[u8; 32]>,
    /// Verified candidate version.
    pub available_version: String,
    /// Digest of the exact candidate manifest signing payload.
    pub available_manifest_digest: [u8; 32],
    /// Immediately previous version retained for rollback.
    pub rollback_version: Option<String>,
    /// Digest paired with `rollback_version`.
    pub rollback_manifest_digest: Option<[u8; 32]>,
    /// Monotonic optimistic revision. Overflow fails closed.
    pub revision: u64,
    /// Last durable transition time.
    pub updated_at: UnixMillis,
}

/// Exact request persisted in the lifecycle command journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyManagedIntegrationLifecycle {
    /// Globally unique idempotency key.
    pub idempotency_key: String,
    /// Hash of every semantically relevant request field.
    pub request_fingerprint: [u8; 32],
    /// Target integration.
    pub integration_id: String,
    /// Requested mutation.
    pub mutation: ManagedIntegrationMutation,
    /// Optimistic lifecycle revision.
    pub expected_revision: u64,
    /// Exact verified candidate version.
    pub candidate_version: String,
    /// Exact verified candidate manifest signing digest.
    pub candidate_manifest_digest: [u8; 32],
    /// Observation time recorded on first commit.
    pub observed_at: UnixMillis,
}

/// First-commit or exact-replay result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedIntegrationLifecycleCommit {
    /// Canonical state produced by the command.
    pub lifecycle: ManagedIntegrationLifecycle,
    /// True when the exact idempotency key and fingerprint already committed.
    pub replayed: bool,
}

/// Pending publication evidence recovered after a crash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedIntegrationRecoveryEntry {
    /// Original idempotency key.
    pub idempotency_key: String,
    /// Target integration.
    pub integration_id: String,
    /// Requested mutation.
    pub mutation: ManagedIntegrationMutation,
    /// Lifecycle revision produced transactionally before filesystem publish.
    pub committed_revision: u64,
    /// Manifest digest which must match the staged snapshot before publication.
    pub candidate_manifest_digest: [u8; 32],
}

/// Encrypted durable store for integration state and exact mutation journals.
#[async_trait]
pub trait ManagedIntegrationLifecycleStore: Send + Sync {
    /// Returns canonical state when the integration is known.
    async fn get_managed_integration(
        &self,
        integration_id: &str,
    ) -> Result<Option<ManagedIntegrationLifecycle>, StoreError>;

    /// Returns only the newest lifecycle outcome whose exact filesystem
    /// publication was durably acknowledged.
    async fn get_published_managed_integration(
        &self,
        integration_id: &str,
    ) -> Result<Option<ManagedIntegrationLifecycle>, StoreError>;

    /// Atomically checks idempotency/revision, journals the command, and updates
    /// lifecycle state. The filesystem publication remains a separate,
    /// recoverable step and must not execute before this commit.
    async fn apply_managed_integration_lifecycle(
        &self,
        command: ApplyManagedIntegrationLifecycle,
    ) -> Result<ManagedIntegrationLifecycleCommit, StoreError>;

    /// Returns bounded journal entries that committed but have not recorded a
    /// successful atomic filesystem publication.
    async fn pending_managed_integration_publications(
        &self,
        limit: usize,
    ) -> Result<Vec<ManagedIntegrationRecoveryEntry>, StoreError>;

    /// Marks one exact committed revision as published. Stale acknowledgements
    /// fail closed rather than completing a newer operation.
    async fn acknowledge_managed_integration_publication(
        &self,
        idempotency_key: &str,
        committed_revision: u64,
        published_at: UnixMillis,
    ) -> Result<(), StoreError>;
}
