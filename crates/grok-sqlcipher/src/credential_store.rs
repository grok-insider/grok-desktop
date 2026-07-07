use async_trait::async_trait;
use grok_application::{
    AccountState, CredentialMutationReservation, CredentialMutationStore, MutationCommand,
    StoreError,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use crate::{SqlCipherStore, store::map_sqlite};

#[async_trait]
impl CredentialMutationStore for SqlCipherStore {
    async fn resolve_credential_mutation(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<CredentialMutationReservation>, StoreError> {
        let command = command.clone();
        self.with_store(move |connection| {
            let existing = connection
                .query_row(
                    "SELECT request_fingerprint,completed,xai_api_key_configured,
                            xai_capabilities_resolved
                     FROM credential_commands WHERE scope=?1 AND idempotency_key=?2",
                    params![command.scope, command.key],
                    |row| {
                        Ok((
                            row.get::<_, Vec<u8>>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, Option<i64>>(2)?,
                            row.get::<_, Option<i64>>(3)?,
                        ))
                    },
                )
                .optional()
                .map_err(map_sqlite)?;
            match existing {
                Some((fingerprint, _, _, _)) if fingerprint != command.fingerprint => {
                    Err(StoreError::Conflict)
                }
                Some((_, 0, None, None)) => Ok(Some(CredentialMutationReservation::Pending)),
                Some((_, 1, Some(configured), resolved)) => Ok(Some(
                    CredentialMutationReservation::Completed(AccountState {
                        xai_api_key_configured: configured != 0,
                        xai_capabilities_resolved: resolved.is_some_and(|value| value != 0),
                    }),
                )),
                Some(_) => Err(StoreError::Internal(
                    "stored credential command state is invalid".into(),
                )),
                None => Ok(None),
            }
        })
        .await
    }

    async fn begin_credential_mutation(
        &self,
        command: &MutationCommand,
    ) -> Result<CredentialMutationReservation, StoreError> {
        let command = command.clone();
        self.with_store(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(map_sqlite)?;
            let existing = transaction
                .query_row(
                    "SELECT request_fingerprint,completed,xai_api_key_configured,
                            xai_capabilities_resolved
                     FROM credential_commands WHERE scope=?1 AND idempotency_key=?2",
                    params![command.scope, command.key],
                    |row| {
                        Ok((
                            row.get::<_, Vec<u8>>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, Option<i64>>(2)?,
                            row.get::<_, Option<i64>>(3)?,
                        ))
                    },
                )
                .optional()
                .map_err(map_sqlite)?;
            let reservation = match existing {
                Some((fingerprint, _, _, _)) if fingerprint != command.fingerprint => {
                    return Err(StoreError::Conflict);
                }
                Some((_, 0, None, None)) => CredentialMutationReservation::Pending,
                Some((_, 1, Some(configured), resolved)) => {
                    CredentialMutationReservation::Completed(AccountState {
                        xai_api_key_configured: configured != 0,
                        xai_capabilities_resolved: resolved.is_some_and(|value| value != 0),
                    })
                }
                Some(_) => {
                    return Err(StoreError::Internal(
                        "stored credential command state is invalid".into(),
                    ));
                }
                None => {
                    transaction
                        .execute(
                            "INSERT INTO credential_commands(
                                scope,idempotency_key,request_fingerprint,completed,
                                xai_api_key_configured
                             ) VALUES (?1,?2,?3,0,NULL)",
                            params![command.scope, command.key, command.fingerprint.as_slice(),],
                        )
                        .map_err(map_sqlite)?;
                    CredentialMutationReservation::NewlyReserved
                }
            };
            transaction.commit().map_err(map_sqlite)?;
            Ok(reservation)
        })
        .await
    }

    async fn complete_credential_mutation(
        &self,
        command: &MutationCommand,
        outcome: AccountState,
    ) -> Result<(), StoreError> {
        let command = command.clone();
        self.with_store(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(map_sqlite)?;
            let existing = transaction
                .query_row(
                    "SELECT request_fingerprint,completed,xai_api_key_configured,
                            xai_capabilities_resolved
                     FROM credential_commands WHERE scope=?1 AND idempotency_key=?2",
                    params![command.scope, command.key],
                    |row| {
                        Ok((
                            row.get::<_, Vec<u8>>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, Option<i64>>(2)?,
                            row.get::<_, Option<i64>>(3)?,
                        ))
                    },
                )
                .optional()
                .map_err(map_sqlite)?
                .ok_or(StoreError::NotFound)?;
            if existing.0 != command.fingerprint {
                return Err(StoreError::Conflict);
            }
            let configured = i64::from(outcome.xai_api_key_configured);
            let resolved = i64::from(outcome.xai_capabilities_resolved);
            match (existing.1, existing.2, existing.3) {
                (0, None, None) => {
                    let changed = transaction
                        .execute(
                            "UPDATE credential_commands
                             SET completed=1,xai_api_key_configured=?1,
                                 xai_capabilities_resolved=?2
                             WHERE scope=?3 AND idempotency_key=?4 AND completed=0",
                            params![configured, resolved, command.scope, command.key],
                        )
                        .map_err(map_sqlite)?;
                    if changed != 1 {
                        return Err(StoreError::Conflict);
                    }
                }
                (1, Some(existing), existing_resolved)
                    if existing == configured
                        && existing_resolved.is_none_or(|value| value == resolved) => {}
                (1, Some(_), _) => return Err(StoreError::Conflict),
                _ => {
                    return Err(StoreError::Internal(
                        "stored credential command state is invalid".into(),
                    ));
                }
            }
            transaction.commit().map_err(map_sqlite)
        })
        .await
    }
}
