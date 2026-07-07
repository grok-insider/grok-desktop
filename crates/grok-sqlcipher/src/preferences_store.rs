use async_trait::async_trait;
use grok_application::{DesktopPreferencesStore, MutationCommand, StoreError};
use grok_domain::DesktopPreferences;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::{
    SqlCipherStore,
    store::{map_sqlite, number},
};

const PREFERENCE_SCOPE: &str = "update_desktop_preferences";

#[async_trait]
impl DesktopPreferencesStore for SqlCipherStore {
    async fn resolve_desktop_preferences_mutation(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<DesktopPreferences>, StoreError> {
        let command = command.clone();
        self.with_store(move |connection| prior_mutation(connection, &command))
            .await
    }

    async fn get_desktop_preferences(&self) -> Result<DesktopPreferences, StoreError> {
        self.with_store(load_preferences).await
    }

    async fn save_desktop_preferences(
        &self,
        preferences: DesktopPreferences,
        expected_revision: u64,
        command: &MutationCommand,
    ) -> Result<DesktopPreferences, StoreError> {
        let command = command.clone();
        self.with_store(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(map_sqlite)?;
            if let Some(existing) = prior_mutation(&transaction, &command)? {
                return Ok(existing);
            }
            if command.scope != PREFERENCE_SCOPE
                || preferences.revision
                    != expected_revision
                        .checked_add(1)
                        .ok_or(StoreError::Conflict)?
            {
                return Err(StoreError::Conflict);
            }
            let changed = transaction
                .execute(
                    "UPDATE desktop_preferences
                     SET keep_running_in_notification_area=?1, revision=?2, updated_at=?3
                     WHERE singleton=1 AND revision=?4",
                    params![
                        i64::from(preferences.keep_running_in_notification_area),
                        number(preferences.revision)?,
                        number(preferences.updated_at)?,
                        number(expected_revision)?,
                    ],
                )
                .map_err(map_sqlite)?;
            if changed != 1 {
                return Err(StoreError::Conflict);
            }
            transaction
                .execute(
                    "INSERT INTO desktop_preference_commands(
                         scope,idempotency_key,request_fingerprint,
                         keep_running_in_notification_area,revision,updated_at
                     ) VALUES (?1,?2,?3,?4,?5,?6)",
                    params![
                        command.scope,
                        command.key,
                        command.fingerprint.as_slice(),
                        i64::from(preferences.keep_running_in_notification_area),
                        number(preferences.revision)?,
                        number(preferences.updated_at)?,
                    ],
                )
                .map_err(map_sqlite)?;
            transaction.commit().map_err(map_sqlite)?;
            Ok(preferences)
        })
        .await
    }
}

fn load_preferences(connection: &mut Connection) -> Result<DesktopPreferences, StoreError> {
    connection
        .query_row(
            "SELECT keep_running_in_notification_area,revision,updated_at
             FROM desktop_preferences WHERE singleton=1",
            [],
            |row| preference_from_row(row.get(0)?, row.get(1)?, row.get(2)?),
        )
        .map_err(map_sqlite)
}

fn prior_mutation(
    connection: &Connection,
    command: &MutationCommand,
) -> Result<Option<DesktopPreferences>, StoreError> {
    let record = connection
        .query_row(
            "SELECT request_fingerprint,keep_running_in_notification_area,revision,updated_at
             FROM desktop_preference_commands WHERE scope=?1 AND idempotency_key=?2",
            params![command.scope, command.key],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()
        .map_err(map_sqlite)?;
    let Some((fingerprint, keep_running, revision, updated_at)) = record else {
        return Ok(None);
    };
    if fingerprint.as_slice() != command.fingerprint {
        return Err(StoreError::Conflict);
    }
    Ok(Some(
        preference_from_row(keep_running, revision, updated_at).map_err(map_sqlite)?,
    ))
}

fn preference_from_row(
    keep_running: i64,
    revision: i64,
    updated_at: i64,
) -> rusqlite::Result<DesktopPreferences> {
    let keep_running_in_notification_area = match keep_running {
        0 => false,
        1 => true,
        _ => return Err(rusqlite::Error::IntegralValueOutOfRange(0, keep_running)),
    };
    Ok(DesktopPreferences {
        keep_running_in_notification_area,
        revision: u64::try_from(revision)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, revision))?,
        updated_at: u64::try_from(updated_at)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, updated_at))?,
    })
}
