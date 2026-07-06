use thiserror::Error;
use zeroize::{Zeroize, Zeroizing};

/// Validated opaque name for one credential-vault entry.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SecretName(String);

impl SecretName {
    /// Creates a stable vault key.
    ///
    /// # Errors
    ///
    /// Returns [`VaultError::InvalidName`] for empty, oversized, or unsafe
    /// values.
    pub fn new(value: impl Into<String>) -> Result<Self, VaultError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || !value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b".-_".contains(&byte)
            })
        {
            return Err(VaultError::InvalidName);
        }
        Ok(Self(value))
    }

    /// Borrows the platform-vault account name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Secret bytes that zero their allocation on drop and never print contents.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretValue(Zeroizing<Vec<u8>>);

impl SecretValue {
    /// Wraps non-empty secret material.
    ///
    /// # Errors
    ///
    /// Returns [`VaultError::InvalidValue`] when the value is empty or exceeds
    /// the one-megabyte platform-vault limit.
    pub fn new(mut value: Vec<u8>) -> Result<Self, VaultError> {
        if value.is_empty() || value.len() > 1024 * 1024 {
            value.zeroize();
            return Err(VaultError::InvalidValue);
        }
        Ok(Self(Zeroizing::new(value)))
    }

    /// Borrows secret material for immediate use by a trusted adapter.
    #[must_use]
    pub fn expose_secret(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SecretValue([REDACTED])")
    }
}

/// Failure at the operating-system credential boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum VaultError {
    /// Entry does not exist.
    #[error("secret not found")]
    NotFound,
    /// Vault entry name is invalid.
    #[error("secret name is invalid")]
    InvalidName,
    /// Secret is empty or too large for the product boundary.
    #[error("secret value is invalid")]
    InvalidValue,
    /// Platform vault is locked or unavailable.
    #[error("secure vault unavailable")]
    Unavailable,
    /// Platform vault returned an unexpected failure.
    #[error("secure vault operation failed")]
    Internal,
}

/// Platform-backed credential operations used by trusted adapters.
pub trait SecretVault: Send + Sync {
    /// Loads one entry.
    ///
    /// # Errors
    ///
    /// Returns [`VaultError`] when the entry is absent or the native vault is
    /// unavailable.
    fn get(&self, name: &SecretName) -> Result<SecretValue, VaultError>;

    /// Creates or replaces one entry.
    ///
    /// # Errors
    ///
    /// Returns [`VaultError`] when the native vault refuses the write.
    fn set(&self, name: &SecretName, value: &SecretValue) -> Result<(), VaultError>;

    /// Removes one entry. Missing entries succeed to keep cleanup idempotent.
    ///
    /// # Errors
    ///
    /// Returns [`VaultError`] when the native vault cannot perform cleanup.
    fn delete(&self, name: &SecretName) -> Result<(), VaultError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_portable_and_secrets_are_redacted() {
        assert!(SecretName::new("xai.api-key.primary").is_ok());
        assert!(SecretName::new("Uppercase").is_err());
        assert!(SecretName::new("../escape").is_err());
        let value = SecretValue::new(b"not-printed".to_vec()).expect("secret");
        assert_eq!(format!("{value:?}"), "SecretValue([REDACTED])");
    }
}
