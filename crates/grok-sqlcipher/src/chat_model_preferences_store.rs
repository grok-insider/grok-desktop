use async_trait::async_trait;
use grok_application::{ChatModelPreferenceStore, MutationCommand, StoreError};
use grok_domain::ChatModelPreference;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::{
    SqlCipherStore,
    store::{map_sqlite, number},
};

const PREFERENCE_SCOPE: &str = "select_chat_model";

#[async_trait]
impl ChatModelPreferenceStore for SqlCipherStore {
    async fn resolve_chat_model_preference_mutation(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<ChatModelPreference>, StoreError> {
        let command = command.clone();
        self.with_store(move |connection| prior_mutation(connection, &command))
            .await
    }

    async fn get_chat_model_preference(&self) -> Result<ChatModelPreference, StoreError> {
        self.with_store(load_preference).await
    }

    async fn save_chat_model_preference(
        &self,
        preference: ChatModelPreference,
        expected_revision: u64,
        command: &MutationCommand,
    ) -> Result<ChatModelPreference, StoreError> {
        let command = command.clone();
        self.with_store(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(map_sqlite)?;
            if let Some(existing) = prior_mutation(&transaction, &command)? {
                return Ok(existing);
            }
            if command.scope != PREFERENCE_SCOPE
                || preference.revision
                    != expected_revision
                        .checked_add(1)
                        .ok_or(StoreError::Conflict)?
            {
                return Err(StoreError::Conflict);
            }
            let changed = transaction
                .execute(
                    "UPDATE chat_model_preferences
                     SET selected_model_id=?1, revision=?2, updated_at=?3
                     WHERE singleton=1 AND revision=?4",
                    params![
                        preference.selected_model_id,
                        number(preference.revision)?,
                        number(preference.updated_at)?,
                        number(expected_revision)?,
                    ],
                )
                .map_err(map_sqlite)?;
            if changed != 1 {
                return Err(StoreError::Conflict);
            }
            transaction
                .execute(
                    "INSERT INTO chat_model_preference_commands(
                         scope,idempotency_key,request_fingerprint,
                         selected_model_id,revision,updated_at
                     ) VALUES (?1,?2,?3,?4,?5,?6)",
                    params![
                        command.scope,
                        command.key,
                        command.fingerprint.as_slice(),
                        preference.selected_model_id,
                        number(preference.revision)?,
                        number(preference.updated_at)?,
                    ],
                )
                .map_err(map_sqlite)?;
            transaction.commit().map_err(map_sqlite)?;
            Ok(preference)
        })
        .await
    }
}

fn load_preference(connection: &mut Connection) -> Result<ChatModelPreference, StoreError> {
    let values = connection
        .query_row(
            "SELECT selected_model_id,revision,updated_at
             FROM chat_model_preferences WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(map_sqlite)?;
    preference_from_values(values.0, values.1, values.2)
}

fn prior_mutation(
    connection: &Connection,
    command: &MutationCommand,
) -> Result<Option<ChatModelPreference>, StoreError> {
    let record = connection
        .query_row(
            "SELECT request_fingerprint,selected_model_id,revision,updated_at
             FROM chat_model_preference_commands WHERE scope=?1 AND idempotency_key=?2",
            params![command.scope, command.key],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()
        .map_err(map_sqlite)?;
    let Some((fingerprint, selected_model_id, revision, updated_at)) = record else {
        return Ok(None);
    };
    if fingerprint.as_slice() != command.fingerprint {
        return Err(StoreError::Conflict);
    }
    Ok(Some(preference_from_values(
        selected_model_id,
        revision,
        updated_at,
    )?))
}

fn preference_from_values(
    selected_model_id: String,
    revision: i64,
    updated_at: i64,
) -> Result<ChatModelPreference, StoreError> {
    let revision = u64::try_from(revision)
        .map_err(|_| StoreError::Internal("invalid persisted chat model preference".into()))?;
    let updated_at = u64::try_from(updated_at)
        .map_err(|_| StoreError::Internal("invalid persisted chat model preference".into()))?;
    ChatModelPreference::restore(selected_model_id, revision, updated_at)
        .map_err(|_| StoreError::Internal("invalid persisted chat model preference".into()))
}
