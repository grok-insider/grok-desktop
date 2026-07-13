use thiserror::Error;

use crate::UnixMillis;

/// Product default used until the user commits another live-discovered xAI Chat model.
pub const DEFAULT_XAI_CHAT_MODEL_ID: &str = "grok-4.3";

const MAX_MODEL_ID_BYTES: usize = 512;

/// Invalid mutation of daemon-owned desktop preferences.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DesktopPreferencesError {
    /// An update timestamp predates the persisted preference revision.
    #[error("desktop preference timestamp predates the current revision")]
    ClockRegression,
    /// Optimistic revision cannot advance without wrapping.
    #[error("desktop preference revision is exhausted")]
    RevisionExhausted,
}

/// Public signed application update channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DesktopUpdateChannel {
    /// Production releases only.
    #[default]
    Stable,
    /// Signed prerelease builds plus later stable releases.
    Beta,
}

impl DesktopUpdateChannel {
    /// Stable wire and persistence label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Beta => "beta",
        }
    }
}

/// Durable desktop behavior that must remain authoritative outside the renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopPreferences {
    /// Closing the primary window hides it while the process remains available in the tray.
    pub keep_running_in_notification_area: bool,
    /// Signed public application update channel.
    pub update_channel: DesktopUpdateChannel,
    /// Optimistic concurrency revision.
    pub revision: u64,
    /// Last successful update timestamp.
    pub updated_at: UnixMillis,
}

impl DesktopPreferences {
    /// Product default: closing the window keeps Grok Desktop running.
    #[must_use]
    pub const fn default_at(now: UnixMillis) -> Self {
        Self {
            keep_running_in_notification_area: true,
            update_channel: DesktopUpdateChannel::Stable,
            revision: 0,
            updated_at: now,
        }
    }

    /// Updates desktop behavior with optimistic revision metadata.
    ///
    /// # Errors
    ///
    /// Returns [`DesktopPreferencesError`] for clock regression or revision overflow.
    pub fn update(
        &mut self,
        keep_running: bool,
        update_channel: DesktopUpdateChannel,
        now: UnixMillis,
    ) -> Result<(), DesktopPreferencesError> {
        if now < self.updated_at {
            return Err(DesktopPreferencesError::ClockRegression);
        }
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(DesktopPreferencesError::RevisionExhausted)?;
        self.updated_at = now;
        self.keep_running_in_notification_area = keep_running;
        self.update_channel = update_channel;
        Ok(())
    }
}

impl Default for DesktopPreferences {
    fn default() -> Self {
        Self::default_at(0)
    }
}

/// Invalid mutation of the daemon-owned default Chat model.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ChatModelPreferenceError {
    /// The model identifier is empty, oversized, padded, or contains control characters.
    #[error("selected xAI model identifier is invalid")]
    InvalidModelId,
    /// An update timestamp predates the persisted preference revision.
    #[error("chat model preference timestamp predates the current revision")]
    ClockRegression,
    /// Optimistic revision cannot advance without wrapping.
    #[error("chat model preference revision is exhausted")]
    RevisionExhausted,
}

/// Durable model policy applied to newly reserved direct xAI Chat turns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatModelPreference {
    /// Exact provider identifier or alias validated against a live official xAI catalog.
    pub selected_model_id: String,
    /// Optimistic concurrency revision.
    pub revision: u64,
    /// Last successful update timestamp.
    pub updated_at: UnixMillis,
}

impl ChatModelPreference {
    /// Creates the product default preference.
    ///
    /// # Errors
    ///
    /// Returns [`ChatModelPreferenceError::InvalidModelId`] for an unsafe product default.
    pub fn initial(
        selected_model_id: String,
        now: UnixMillis,
    ) -> Result<Self, ChatModelPreferenceError> {
        validate_model_id(&selected_model_id)?;
        Ok(Self {
            selected_model_id,
            revision: 0,
            updated_at: now,
        })
    }

    /// Rehydrates a persisted preference through the same identifier validation as mutations.
    ///
    /// # Errors
    ///
    /// Returns [`ChatModelPreferenceError::InvalidModelId`] for corrupt persisted state.
    pub fn restore(
        selected_model_id: String,
        revision: u64,
        updated_at: UnixMillis,
    ) -> Result<Self, ChatModelPreferenceError> {
        validate_model_id(&selected_model_id)?;
        Ok(Self {
            selected_model_id,
            revision,
            updated_at,
        })
    }

    /// Commits a provider-validated selection and advances its optimistic revision.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe identifiers, clock regression, or revision exhaustion.
    pub fn select_model(
        &mut self,
        selected_model_id: String,
        now: UnixMillis,
    ) -> Result<(), ChatModelPreferenceError> {
        validate_model_id(&selected_model_id)?;
        if now < self.updated_at {
            return Err(ChatModelPreferenceError::ClockRegression);
        }
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(ChatModelPreferenceError::RevisionExhausted)?;
        self.updated_at = now;
        self.selected_model_id = selected_model_id;
        Ok(())
    }
}

impl Default for ChatModelPreference {
    fn default() -> Self {
        Self::initial(DEFAULT_XAI_CHAT_MODEL_ID.into(), 0)
            .expect("the product default xAI model identifier is valid")
    }
}

fn validate_model_id(value: &str) -> Result<(), ChatModelPreferenceError> {
    if value.is_empty()
        || value.len() > MAX_MODEL_ID_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(ChatModelPreferenceError::InvalidModelId);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_keep_running_and_advances_revision() {
        let mut preferences = DesktopPreferences::default_at(10);
        assert!(preferences.keep_running_in_notification_area);
        preferences
            .update(false, DesktopUpdateChannel::Beta, 11)
            .expect("valid preference update");
        assert!(!preferences.keep_running_in_notification_area);
        assert_eq!(preferences.update_channel, DesktopUpdateChannel::Beta);
        assert_eq!(preferences.revision, 1);
        assert_eq!(preferences.updated_at, 11);
    }

    #[test]
    fn rejects_clock_regression() {
        let mut preferences = DesktopPreferences::default_at(10);
        assert_eq!(
            preferences.update(false, DesktopUpdateChannel::Stable, 9),
            Err(DesktopPreferencesError::ClockRegression)
        );
    }

    #[test]
    fn chat_model_selection_is_bounded_and_revisioned() {
        let mut preference =
            ChatModelPreference::initial("grok-default".into(), 10).expect("valid default");
        preference
            .select_model("grok-current".into(), 11)
            .expect("valid selection");
        assert_eq!(preference.selected_model_id, "grok-current");
        assert_eq!(preference.revision, 1);
        assert_eq!(preference.updated_at, 11);
        assert_eq!(
            preference.select_model(" padded ".into(), 12),
            Err(ChatModelPreferenceError::InvalidModelId)
        );
        assert_eq!(
            preference.select_model("grok-next".into(), 9),
            Err(ChatModelPreferenceError::ClockRegression)
        );
    }
}
