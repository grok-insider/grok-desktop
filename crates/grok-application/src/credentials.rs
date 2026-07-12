use std::{fmt::Write as _, sync::Arc};

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::{RwLock, RwLockReadGuard};

use crate::{
    ApplicationError, MutationCommand, SecretName, SecretValue, SecretVault, StoreError,
    VaultError, mutations::mutation_command_bytes,
};

const XAI_API_KEY_NAME: &str = "xai.api-key.primary";
const XAI_CAPABILITIES_RESOLVED_NAME: &str = "xai.api-key.capabilities-resolved";
const XAI_CREDENTIAL_BINDING_NAME: &str = "xai.api-key.local-binding";
const XAI_CREDENTIAL_BINDING_PREFIX: &str = "xai-binding-";

/// Daemon-only xAI credential material plus its non-secret local generation ID.
///
/// The binding identifies one local enrollment generation. It is not an xAI
/// account identifier and is never derived from credential bytes.
pub(crate) struct XaiApiCredential {
    api_key: SecretValue,
    binding_id: String,
}

/// Read-side lease which linearizes one official xAI request boundary against
/// local credential replacement or deletion.
pub(crate) struct XaiCredentialUseGuard<'a> {
    _guard: RwLockReadGuard<'a, ()>,
}

impl XaiApiCredential {
    /// Splits trusted provider material from its local generation binding.
    #[must_use]
    pub(crate) fn into_parts(self) -> (SecretValue, String) {
        (self.api_key, self.binding_id)
    }
}

/// Non-secret account state safe to return over local IPC.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AccountState {
    /// A user-owned xAI API key is present in the operating-system vault.
    pub xai_api_key_configured: bool,
    /// Official discovery found at least one text-capable conversation model for this key.
    /// Current selected-model readiness is resolved separately from a live catalog.
    pub xai_capabilities_resolved: bool,
    /// Host ACP Grok Build authenticate succeeded (non-secret). Does not unlock
    /// Work without strong isolation facts.
    pub grok_build_authenticated: bool,
}

/// Non-secret result of validating a key against official xAI contracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XaiApiKeyValidation {
    /// Discovery succeeded and at least one text-capable conversation model is available.
    CapabilitiesResolved,
    /// The key may be valid but endpoint scope prevents capability discovery.
    CapabilitiesUnresolved,
}

/// Durable state of a credential mutation reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialMutationReservation {
    /// This call atomically created the pending reservation.
    NewlyReserved,
    /// The same command may safely resume its idempotent vault operation.
    Pending,
    /// The original command already completed with this canonical result.
    Completed(AccountState),
}

/// Durable journal used to reserve credential mutations before vault side effects.
#[async_trait]
pub trait CredentialMutationStore: Send + Sync {
    /// Resolves an existing command without creating a reservation.
    async fn resolve_credential_mutation(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<CredentialMutationReservation>, StoreError>;

    /// Reserves a command fingerprint or replays its existing state.
    async fn begin_credential_mutation(
        &self,
        command: &MutationCommand,
    ) -> Result<CredentialMutationReservation, StoreError>;

    /// Marks a reserved command complete with a non-secret canonical result.
    async fn complete_credential_mutation(
        &self,
        command: &MutationCommand,
        outcome: AccountState,
    ) -> Result<(), StoreError>;
}

/// Stable validation failure returned by the official xAI adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum XaiApiKeyValidationError {
    /// Key bytes are empty, malformed, or outside supported bounds.
    #[error("xAI API key format is invalid")]
    InvalidFormat,
    /// The official xAI origin rejected the credential.
    #[error("xAI rejected the API key")]
    Rejected,
    /// The official validation endpoint could not be reached.
    #[error("xAI credential validation is unavailable")]
    Unavailable,
}

/// Validates one key against the fixed official xAI origin.
#[async_trait]
pub trait XaiApiKeyValidator: Send + Sync {
    /// Validates format and provider acceptance without retaining the key.
    async fn validate(
        &self,
        api_key: &SecretValue,
    ) -> Result<XaiApiKeyValidation, XaiApiKeyValidationError>;
}

/// Daemon-owned use cases for user-provided xAI credentials.
pub struct CredentialService {
    vault: Arc<dyn SecretVault>,
    mutations: Arc<dyn CredentialMutationStore>,
    validator: Arc<dyn XaiApiKeyValidator>,
    xai_key_name: SecretName,
    xai_capabilities_resolved_name: SecretName,
    xai_credential_binding_name: SecretName,
    credential_use_gate: RwLock<()>,
}

impl CredentialService {
    /// Creates the credential coordinator from narrow infrastructure ports.
    ///
    /// # Panics
    ///
    /// Panics only if the product-owned static vault entry name becomes invalid.
    #[must_use]
    pub fn new(
        vault: Arc<dyn SecretVault>,
        mutations: Arc<dyn CredentialMutationStore>,
        validator: Arc<dyn XaiApiKeyValidator>,
    ) -> Self {
        Self {
            vault,
            mutations,
            validator,
            xai_key_name: SecretName::new(XAI_API_KEY_NAME)
                .expect("the fixed xAI credential name is valid"),
            xai_capabilities_resolved_name: SecretName::new(XAI_CAPABILITIES_RESOLVED_NAME)
                .expect("the fixed xAI capability marker name is valid"),
            xai_credential_binding_name: SecretName::new(XAI_CREDENTIAL_BINDING_NAME)
                .expect("the fixed xAI credential binding name is valid"),
            credential_use_gate: RwLock::new(()),
        }
    }

    /// Returns non-secret account state without exposing credential bytes.
    ///
    /// # Errors
    ///
    /// Returns an application error when the operating-system vault is unavailable.
    pub fn account_state(&self) -> Result<AccountState, ApplicationError> {
        match self.vault.get(&self.xai_key_name) {
            Ok(_) => {
                let binding_ready = match self.load_credential_binding() {
                    Ok(_) => true,
                    Err(ApplicationError::Storage(_)) => false,
                    Err(error) => return Err(error),
                };
                Ok(AccountState {
                    xai_api_key_configured: true,
                    xai_capabilities_resolved: binding_ready
                        && matches!(
                            self.vault.get(&self.xai_capabilities_resolved_name),
                            Ok(value) if value.expose_secret() == [1]
                        ),
                    grok_build_authenticated: false,
                })
            }
            Err(VaultError::NotFound) => Ok(AccountState::default()),
            Err(error) => Err(map_vault_error(error)),
        }
    }

    /// Loads provider material together with its local credential generation.
    ///
    /// Legacy or partially written credentials without a valid binding fail
    /// closed. Callers must never infer that the binding is an official account
    /// identity.
    ///
    /// # Errors
    ///
    /// Returns unauthorized when the key is absent and a storage error when the
    /// key exists without a valid local generation binding.
    pub(crate) fn load_xai_api_credential(&self) -> Result<XaiApiCredential, ApplicationError> {
        let api_key = self
            .vault
            .get(&self.xai_key_name)
            .map_err(|error| match error {
                VaultError::NotFound => {
                    ApplicationError::Unauthorized("an xAI API key is not configured".into())
                }
                other => map_vault_error(other),
            })?;
        let binding_id = self.load_credential_binding()?;
        Ok(XaiApiCredential {
            api_key,
            binding_id,
        })
    }

    /// Returns only the non-secret local credential-generation identifier.
    ///
    /// This is suitable for daemon policy comparisons but is not an official
    /// xAI account identity and must not be presented as one.
    ///
    /// # Errors
    ///
    /// Returns the same fail-closed errors as [`Self::load_xai_api_credential`].
    pub(crate) fn current_xai_credential_binding_id(&self) -> Result<String, ApplicationError> {
        self.load_xai_api_credential()
            .map(XaiApiCredential::into_parts)
            .map(|(_, binding_id)| binding_id)
    }

    /// Acquires a generation-checked provider-use lease.
    ///
    /// Credential replacement and deletion take the exclusive side of this
    /// gate. Callers retain the lease until the provider request has either
    /// definitely not begun or crossed its network boundary.
    ///
    /// # Errors
    ///
    /// Returns fail-closed credential errors or invalid-state when the expected
    /// local generation is no longer current.
    pub(crate) async fn acquire_xai_credential_use(
        &self,
        expected_binding: &str,
    ) -> Result<XaiCredentialUseGuard<'_>, ApplicationError> {
        let guard = self.credential_use_gate.read().await;
        if self.current_xai_credential_binding_id()? != expected_binding {
            return Err(ApplicationError::InvalidState(
                "the xAI credential changed before provider dispatch".into(),
            ));
        }
        Ok(XaiCredentialUseGuard { _guard: guard })
    }

    pub(crate) async fn load_xai_api_credential_for_use(
        &self,
    ) -> Result<(XaiApiCredential, XaiCredentialUseGuard<'_>), ApplicationError> {
        let guard = self.credential_use_gate.read().await;
        let credential = self.load_xai_api_credential()?;
        Ok((credential, XaiCredentialUseGuard { _guard: guard }))
    }

    /// Revalidates the selected xAI model and updates the fail-closed readiness marker.
    ///
    /// Per-turn model discovery remains authoritative because provider scope and
    /// model availability can change after this startup/refresh observation.
    ///
    /// # Errors
    ///
    /// Returns unavailable after clearing readiness when capabilities cannot be resolved.
    pub async fn refresh_xai_capabilities(&self) -> Result<AccountState, ApplicationError> {
        let (credential, _credential_use) = match self.load_xai_api_credential_for_use().await {
            Ok(value) => value,
            Err(ApplicationError::Unauthorized(_)) => return Ok(AccountState::default()),
            Err(ApplicationError::Storage(_)) => {
                self.write_capability_marker(false)?;
                return Err(ApplicationError::Storage(
                    "xAI credential is missing its local generation binding".into(),
                ));
            }
            Err(error) => return Err(error),
        };
        let (api_key, _) = credential.into_parts();
        let validation = self.validator.validate(&api_key).await;
        let resolved = matches!(validation, Ok(XaiApiKeyValidation::CapabilitiesResolved));
        self.write_capability_marker(resolved)?;
        match validation {
            Ok(_) => Ok(AccountState {
                xai_api_key_configured: true,
                xai_capabilities_resolved: resolved,
                grok_build_authenticated: false,
            }),
            Err(error) => Err(map_validation_error(error)),
        }
    }

    /// Validates and stores a user-owned key with durable command reservation.
    ///
    /// # Errors
    ///
    /// Returns an application error when validation fails, the vault cannot store
    /// the key, or the idempotency key conflicts with a different command.
    pub async fn configure_xai_api_key(
        &self,
        api_key: SecretValue,
        idempotency_key: &str,
    ) -> Result<AccountState, ApplicationError> {
        let command = mutation_command_bytes(
            "configure_xai_api_key",
            idempotency_key,
            &[api_key.expose_secret()],
        )?;
        if let Some(CredentialMutationReservation::Completed(outcome)) =
            self.mutations.resolve_credential_mutation(&command).await?
        {
            return Ok(outcome);
        }
        let validation = self
            .validator
            .validate(&api_key)
            .await
            .map_err(map_validation_error)?;
        let _mutation_guard = self.credential_use_gate.write().await;
        match self.mutations.begin_credential_mutation(&command).await? {
            CredentialMutationReservation::Completed(outcome) => return Ok(outcome),
            CredentialMutationReservation::NewlyReserved
            | CredentialMutationReservation::Pending => {}
        }
        let outcome = self.store_validated_xai_api_key(
            &api_key,
            validation,
            credential_binding_id(&command),
        )?;
        self.mutations
            .complete_credential_mutation(&command, outcome)
            .await?;
        Ok(outcome)
    }

    pub(crate) async fn validate_and_store_xai_api_key(
        &self,
        api_key: SecretValue,
        command: &MutationCommand,
    ) -> Result<AccountState, ApplicationError> {
        let validation = self
            .validator
            .validate(&api_key)
            .await
            .map_err(map_validation_error)?;
        let _mutation_guard = self.credential_use_gate.write().await;
        self.store_validated_xai_api_key(&api_key, validation, credential_binding_id(command))
    }

    pub(crate) async fn reconcile_pending_xai_enrollment(
        &self,
        command: &MutationCommand,
    ) -> Result<AccountState, ApplicationError> {
        if command.scope != "enroll_xai_api_key" {
            return Err(ApplicationError::InvalidInput(
                "credential enrollment command scope is invalid".into(),
            ));
        }
        let expected_binding = credential_binding_id(command);
        let _mutation_guard = self.credential_use_gate.write().await;
        let current_binding = self.current_xai_credential_binding_id().map_err(|_| {
            ApplicationError::Integrity(
                "pending credential enrollment cannot be safely resumed".into(),
            )
        })?;
        if current_binding != expected_binding {
            return Err(ApplicationError::Integrity(
                "pending credential enrollment does not match the installed generation".into(),
            ));
        }
        let outcome = self.account_state()?;
        if !outcome.xai_api_key_configured {
            return Err(ApplicationError::Integrity(
                "pending credential enrollment has no installed key".into(),
            ));
        }
        self.mutations
            .complete_credential_mutation(command, outcome)
            .await?;
        Ok(outcome)
    }

    fn store_validated_xai_api_key(
        &self,
        api_key: &SecretValue,
        validation: XaiApiKeyValidation,
        credential_binding_id: String,
    ) -> Result<AccountState, ApplicationError> {
        let capabilities_resolved = matches!(validation, XaiApiKeyValidation::CapabilitiesResolved);
        // Clear readiness before replacing bytes so a crash can only degrade capabilities.
        self.write_capability_marker(false)?;
        match self.vault.delete(&self.xai_credential_binding_name) {
            Ok(()) | Err(VaultError::NotFound) => {}
            Err(error) => return Err(map_vault_error(error)),
        }
        self.vault
            .set(&self.xai_key_name, api_key)
            .map_err(map_vault_error)?;
        let binding =
            SecretValue::new(credential_binding_id.into_bytes()).map_err(map_vault_error)?;
        self.vault
            .set(&self.xai_credential_binding_name, &binding)
            .map_err(map_vault_error)?;
        if capabilities_resolved {
            self.write_capability_marker(true)?;
        }
        Ok(AccountState {
            xai_api_key_configured: true,
            xai_capabilities_resolved: capabilities_resolved,
                grok_build_authenticated: false,
        })
    }

    /// Deletes the xAI key idempotently and records the canonical result.
    ///
    /// # Errors
    ///
    /// Returns an application error when the vault or mutation journal is
    /// unavailable, or the idempotency key conflicts with a different command.
    pub async fn delete_xai_api_key(
        &self,
        idempotency_key: &str,
    ) -> Result<AccountState, ApplicationError> {
        let command = mutation_command_bytes("delete_xai_api_key", idempotency_key, &[])?;
        let _mutation_guard = self.credential_use_gate.write().await;
        match self.mutations.begin_credential_mutation(&command).await? {
            CredentialMutationReservation::Completed(outcome) => return Ok(outcome),
            CredentialMutationReservation::NewlyReserved
            | CredentialMutationReservation::Pending => {}
        }
        self.vault
            .delete(&self.xai_key_name)
            .map_err(map_vault_error)?;
        match self.vault.delete(&self.xai_credential_binding_name) {
            Ok(()) | Err(VaultError::NotFound) => {}
            Err(error) => return Err(map_vault_error(error)),
        }
        match self.vault.delete(&self.xai_capabilities_resolved_name) {
            Ok(()) | Err(VaultError::NotFound) => {}
            Err(error) => return Err(map_vault_error(error)),
        }
        let outcome = AccountState::default();
        self.mutations
            .complete_credential_mutation(&command, outcome)
            .await?;
        Ok(outcome)
    }

    fn write_capability_marker(&self, resolved: bool) -> Result<(), ApplicationError> {
        let marker = SecretValue::new(vec![u8::from(resolved)]).map_err(map_vault_error)?;
        self.vault
            .set(&self.xai_capabilities_resolved_name, &marker)
            .map_err(map_vault_error)
    }

    fn load_credential_binding(&self) -> Result<String, ApplicationError> {
        let value =
            self.vault
                .get(&self.xai_credential_binding_name)
                .map_err(|error| match error {
                    VaultError::NotFound => ApplicationError::Storage(
                        "xAI credential is missing its local generation binding".into(),
                    ),
                    other => map_vault_error(other),
                })?;
        let binding = String::from_utf8(value.expose_secret().to_vec()).map_err(|_| {
            ApplicationError::Storage("xAI credential generation binding is invalid".into())
        })?;
        if !valid_credential_binding_id(&binding) {
            return Err(ApplicationError::Storage(
                "xAI credential generation binding is invalid".into(),
            ));
        }
        Ok(binding)
    }
}

fn credential_binding_id(command: &MutationCommand) -> String {
    let mut digest = Sha256::new();
    digest.update(command.scope.as_bytes());
    digest.update([0]);
    digest.update(command.key.as_bytes());
    let mut encoded = String::with_capacity(XAI_CREDENTIAL_BINDING_PREFIX.len() + 64);
    encoded.push_str(XAI_CREDENTIAL_BINDING_PREFIX);
    for byte in digest.finalize() {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

fn valid_credential_binding_id(value: &str) -> bool {
    value.len() == XAI_CREDENTIAL_BINDING_PREFIX.len() + 64
        && value.starts_with(XAI_CREDENTIAL_BINDING_PREFIX)
        && value[XAI_CREDENTIAL_BINDING_PREFIX.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn map_validation_error(error: XaiApiKeyValidationError) -> ApplicationError {
    match error {
        XaiApiKeyValidationError::InvalidFormat => {
            ApplicationError::InvalidInput("xAI API key format is invalid".into())
        }
        XaiApiKeyValidationError::Rejected => {
            ApplicationError::Unauthorized("xAI rejected the API key".into())
        }
        XaiApiKeyValidationError::Unavailable => {
            ApplicationError::Unavailable("xAI credential validation is unavailable".into())
        }
    }
}

fn map_vault_error(error: VaultError) -> ApplicationError {
    match error {
        VaultError::Unavailable => {
            ApplicationError::Unavailable("secure credential vault is unavailable".into())
        }
        VaultError::NotFound => ApplicationError::NotFound,
        VaultError::InvalidName | VaultError::InvalidValue | VaultError::Internal => {
            ApplicationError::Storage("secure credential vault operation failed".into())
        }
    }
}
