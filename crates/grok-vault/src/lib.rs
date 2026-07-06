//! Fail-closed native credential storage for Grok Desktop.

use grok_application::{
    DatabaseKey, KeyProviderError, SecretName, SecretValue, SecretVault, SecureKeyProvider,
    VaultError,
};
use keyring::Entry;
use std::sync::Mutex;
use zeroize::Zeroize;

const DATABASE_KEY_NAME: &str = "database.master-key.v1";
static NATIVE_VAULT_GATE: Mutex<()> = Mutex::new(());

/// Credential vault backed by Credential Manager, Keychain, or Secret Service.
#[derive(Debug, Clone)]
pub struct OsVault {
    service: String,
}

impl OsVault {
    /// Creates a vault namespace for the current product installation.
    ///
    /// # Errors
    ///
    /// Returns [`VaultError::InvalidName`] when the installation identifier is
    /// not portable across supported platform vaults.
    pub fn new(installation_id: &str) -> Result<Self, VaultError> {
        let installation = SecretName::new(installation_id)?;
        Ok(Self {
            service: format!("desktop.grok.{}", installation.as_str()),
        })
    }

    /// Ensures the database master key exists without exposing it to callers.
    ///
    /// # Errors
    ///
    /// Fails closed when the operating-system random source or native vault is
    /// unavailable.
    pub fn ensure_database_key(&self) -> Result<(), VaultError> {
        let name = SecretName::new(DATABASE_KEY_NAME)?;
        match self.get(&name) {
            Ok(value) if value.expose_secret().len() == 32 => return Ok(()),
            Ok(_) => return Err(VaultError::Internal),
            Err(VaultError::NotFound) => {}
            Err(error) => return Err(error),
        }

        let mut key = [0_u8; 32];
        if getrandom::fill(&mut key).is_err() {
            return Err(VaultError::Unavailable);
        }
        let value = SecretValue::new(key.to_vec());
        key.zeroize();
        self.set(&name, &value?)
    }
}

impl SecretVault for OsVault {
    fn get(&self, name: &SecretName) -> Result<SecretValue, VaultError> {
        let service = self.service.clone();
        let name = name.as_str().to_owned();
        native_vault_call(move || {
            let bytes = entry(&service, &name)?
                .get_secret()
                .map_err(|error| map_keyring(&error))?;
            SecretValue::new(bytes)
        })
    }

    fn set(&self, name: &SecretName, value: &SecretValue) -> Result<(), VaultError> {
        let service = self.service.clone();
        let name = name.as_str().to_owned();
        let value = value.clone();
        native_vault_call(move || {
            entry(&service, &name)?
                .set_secret(value.expose_secret())
                .map_err(|error| map_keyring(&error))
        })
    }

    fn delete(&self, name: &SecretName) -> Result<(), VaultError> {
        let service = self.service.clone();
        let name = name.as_str().to_owned();
        native_vault_call(move || match entry(&service, &name)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(map_keyring(&error)),
        })
    }
}

impl SecureKeyProvider for OsVault {
    fn database_key(&self) -> Result<DatabaseKey, KeyProviderError> {
        let name = SecretName::new(DATABASE_KEY_NAME)
            .map_err(|_| KeyProviderError::Internal("database key name is invalid".into()))?;
        let value = self.get(&name).map_err(|error| match error {
            VaultError::NotFound | VaultError::Unavailable => {
                KeyProviderError::Unavailable("native credential store is unavailable".into())
            }
            _ => KeyProviderError::Internal("native credential store failed".into()),
        })?;
        DatabaseKey::from_slice(value.expose_secret())
    }
}

fn map_keyring(error: &keyring::Error) -> VaultError {
    match error {
        keyring::Error::NoEntry => VaultError::NotFound,
        keyring::Error::NoDefaultStore
        | keyring::Error::PlatformFailure(_)
        | keyring::Error::Ambiguous(_) => VaultError::Unavailable,
        _ => VaultError::Internal,
    }
}

fn entry(service: &str, name: &str) -> Result<Entry, VaultError> {
    Entry::new(service, name).map_err(|error| map_keyring(&error))
}

/// The Linux keyring backend is synchronous and internally owns a blocking
/// zbus runtime. Calling it directly from a Tokio worker panics because that
/// would nest runtimes. Keep the synchronous application port, but serialize
/// every native call onto a short-lived plain thread so renderer-triggered
/// daemon work cannot enter the backend from an async executor.
fn native_vault_call<T, F>(operation: F) -> Result<T, VaultError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, VaultError> + Send + 'static,
{
    let _guard = NATIVE_VAULT_GATE.lock().map_err(|_| VaultError::Internal)?;
    std::thread::Builder::new()
        .name("grok-native-vault".into())
        .spawn(operation)
        .map_err(|_| VaultError::Unavailable)?
        .join()
        .map_err(|_| VaultError::Internal)?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installation_names_are_validated_before_platform_access() {
        assert!(OsVault::new("default").is_ok());
        assert!(OsVault::new("../other-user").is_err());
    }

    #[test]
    fn native_calls_run_outside_the_callers_thread() {
        let caller = std::thread::current().id();
        let worker =
            native_vault_call(move || Ok(std::thread::current().id())).expect("worker operation");
        assert_ne!(caller, worker);
    }
}
