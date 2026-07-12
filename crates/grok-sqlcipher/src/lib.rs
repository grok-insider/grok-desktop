//! Encrypted `SQLite` execution store with transactional migrations and recovery hooks.

mod artifact_store;
mod automation_scheduler_store;
mod chat_model_preferences_store;
mod conversation_store;
mod credential_store;
mod managed_integration_store;
mod mapping;
mod preferences_store;
mod privileged_operation_store;
mod schema;
mod store;
mod workspace_store;

pub use store::{DatabaseLock, IntegrityReport, SqlCipherStore, SqlCipherStoreError};
