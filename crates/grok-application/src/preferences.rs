use std::sync::Arc;

use grok_domain::DesktopPreferences;

use crate::{ApplicationError, Clock, DesktopPreferencesStore, mutations::mutation_command_bytes};

/// Revisioned update to process-wide desktop behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateDesktopPreferences {
    /// Revision observed by the caller.
    pub expected_revision: u64,
    /// Whether closing the primary window should hide it instead of quitting.
    pub keep_running_in_notification_area: bool,
}

/// Daemon-owned desktop preference use cases.
pub struct DesktopPreferencesService {
    store: Arc<dyn DesktopPreferencesStore>,
    clock: Arc<dyn Clock>,
}

impl DesktopPreferencesService {
    /// Creates the service around a durable store and daemon clock.
    #[must_use]
    pub fn new(store: Arc<dyn DesktopPreferencesStore>, clock: Arc<dyn Clock>) -> Self {
        Self { store, clock }
    }

    /// Loads the current authoritative preference snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`ApplicationError`] when persistence is unavailable.
    pub async fn get(&self) -> Result<DesktopPreferences, ApplicationError> {
        Ok(self.store.get_desktop_preferences().await?)
    }

    /// Updates close behavior with optimistic concurrency and idempotent replay.
    ///
    /// # Errors
    ///
    /// Returns [`ApplicationError`] for invalid metadata, revision conflicts, or persistence failure.
    pub async fn update(
        &self,
        input: UpdateDesktopPreferences,
        idempotency_key: &str,
    ) -> Result<DesktopPreferences, ApplicationError> {
        let expected_revision = input.expected_revision.to_be_bytes();
        let keep_running = [u8::from(input.keep_running_in_notification_area)];
        let command = mutation_command_bytes(
            "update_desktop_preferences",
            idempotency_key,
            &[&expected_revision, &keep_running],
        )?;
        if let Some(preferences) = self
            .store
            .resolve_desktop_preferences_mutation(&command)
            .await?
        {
            return Ok(preferences);
        }
        let mut preferences = self.store.get_desktop_preferences().await?;
        if preferences.revision != input.expected_revision {
            return Err(ApplicationError::Conflict);
        }
        preferences.set_keep_running_in_notification_area(
            input.keep_running_in_notification_area,
            self.clock.now(),
        )?;
        Ok(self
            .store
            .save_desktop_preferences(preferences, input.expected_revision, &command)
            .await?)
    }
}
