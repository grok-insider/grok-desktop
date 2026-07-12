//! `SQLCipher` parity and restart tests for managed-integration lifecycle state.

use std::sync::Arc;

use grok_application::{
    ApplyManagedIntegrationLifecycle, ManagedIntegrationLifecycleStore, ManagedIntegrationMutation,
    ManagedIntegrationPhase, SecureKeyProvider, StoreError,
};
use grok_memory::EphemeralKeyProvider;
use grok_sqlcipher::SqlCipherStore;

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

async fn open(path: &std::path::Path, key: Arc<dyn SecureKeyProvider>) -> SqlCipherStore {
    SqlCipherStore::open(path, key).await.expect("open store")
}

#[tokio::test]
async fn lifecycle_and_unpublished_journal_survive_restart() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("managed.db");
    let key: Arc<dyn SecureKeyProvider> = Arc::new(EphemeralKeyProvider::new([42; 32]));
    let store = open(&path, key.clone()).await;
    let committed = store
        .apply_managed_integration_lifecycle(install("install-1"))
        .await
        .expect("install");
    assert_eq!(
        committed.lifecycle.phase,
        ManagedIntegrationPhase::Installed
    );
    assert!(
        store
            .get_published_managed_integration("desktop.grok.wisp")
            .await
            .expect("published projection")
            .is_none()
    );
    drop(store);

    let reopened = open(&path, key).await;
    assert_eq!(
        reopened
            .get_managed_integration("desktop.grok.wisp")
            .await
            .expect("load")
            .expect("lifecycle"),
        committed.lifecycle
    );
    let pending = reopened
        .pending_managed_integration_publications(10)
        .await
        .expect("pending");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].committed_revision, 1);
}

#[tokio::test]
async fn exact_replay_and_publication_ack_are_fail_closed() {
    let directory = tempfile::tempdir().expect("directory");
    let key: Arc<dyn SecureKeyProvider> = Arc::new(EphemeralKeyProvider::new([43; 32]));
    let store = open(&directory.path().join("managed.db"), key).await;
    let command = install("install-1");
    let committed = store
        .apply_managed_integration_lifecycle(command.clone())
        .await
        .expect("install");
    assert!(
        store
            .apply_managed_integration_lifecycle(command.clone())
            .await
            .expect("replay")
            .replayed
    );
    let mut forged = command;
    forged.request_fingerprint = [9; 32];
    assert_eq!(
        store.apply_managed_integration_lifecycle(forged).await,
        Err(StoreError::Conflict)
    );
    assert_eq!(
        store
            .acknowledge_managed_integration_publication("install-1", 2, 101)
            .await,
        Err(StoreError::Conflict)
    );
    store
        .acknowledge_managed_integration_publication("install-1", committed.lifecycle.revision, 101)
        .await
        .expect("acknowledge");
    assert_eq!(
        store
            .get_published_managed_integration("desktop.grok.wisp")
            .await
            .expect("published projection")
            .expect("published lifecycle")
            .revision,
        1
    );
    assert!(
        store
            .pending_managed_integration_publications(10)
            .await
            .expect("pending")
            .is_empty()
    );
    assert_eq!(
        store
            .acknowledge_managed_integration_publication("install-1", 1, 102)
            .await,
        Err(StoreError::Conflict)
    );
}

#[tokio::test]
async fn update_and_rollback_preserve_exact_digest_lineage() {
    let directory = tempfile::tempdir().expect("directory");
    let key: Arc<dyn SecureKeyProvider> = Arc::new(EphemeralKeyProvider::new([44; 32]));
    let store = open(&directory.path().join("managed.db"), key).await;
    store
        .apply_managed_integration_lifecycle(install("install-1"))
        .await
        .expect("install");
    store
        .acknowledge_managed_integration_publication("install-1", 1, 101)
        .await
        .expect("publish install");
    let update = ApplyManagedIntegrationLifecycle {
        idempotency_key: "update-1".into(),
        request_fingerprint: [3; 32],
        integration_id: "desktop.grok.wisp".into(),
        mutation: ManagedIntegrationMutation::Update,
        expected_revision: 1,
        candidate_version: "2.0.0".into(),
        candidate_manifest_digest: [4; 32],
        observed_at: 200,
    };
    let updated = store
        .apply_managed_integration_lifecycle(update)
        .await
        .expect("update")
        .lifecycle;
    assert_eq!(updated.rollback_version.as_deref(), Some("1.0.0"));
    assert_eq!(updated.rollback_manifest_digest, Some([2; 32]));
    store
        .acknowledge_managed_integration_publication("update-1", 2, 201)
        .await
        .expect("publish update");
    let rollback = ApplyManagedIntegrationLifecycle {
        idempotency_key: "rollback-1".into(),
        request_fingerprint: [5; 32],
        integration_id: "desktop.grok.wisp".into(),
        mutation: ManagedIntegrationMutation::Rollback,
        expected_revision: 2,
        candidate_version: "2.0.0".into(),
        candidate_manifest_digest: [4; 32],
        observed_at: 300,
    };
    let rolled_back = store
        .apply_managed_integration_lifecycle(rollback)
        .await
        .expect("rollback")
        .lifecycle;
    assert_eq!(rolled_back.installed_version.as_deref(), Some("1.0.0"));
    assert_eq!(rolled_back.installed_manifest_digest, Some([2; 32]));
    assert_eq!(rolled_back.phase, ManagedIntegrationPhase::UpdateAvailable);
}
