use std::{collections::HashSet, sync::Arc, time::Duration};

use grok_domain::{ChatModelPreference, ChatRail, DEFAULT_XAI_CHAT_MODEL_ID};

use crate::{
    ApplicationError, ChatModelPreferenceStore, Clock, ConversationModelFactory, CredentialService,
    ModelDescriptor, ModelError, ModelErrorKind, SuperGrokEnrollmentService,
    mutations::mutation_command_bytes,
};

const MAX_DISCOVERED_MODELS: usize = 256;
const MAX_MODEL_ID_BYTES: usize = 512;
const MAX_MODEL_ALIASES: usize = 64;
const MAX_MODEL_MODALITIES: usize = 16;
const MAX_MODALITY_BYTES: usize = 64;
// The daemon handler retains an additional commit/response reserve around this read-only call.
const MODEL_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(12);

/// One bounded provider descriptor with daemon-derived Chat readiness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatModelCatalogEntry {
    /// Canonical xAI model identifier.
    pub id: String,
    /// Bounded aliases advertised by xAI for this descriptor.
    pub aliases: Vec<String>,
    /// Input modalities reported by xAI.
    pub input_modalities: Vec<String>,
    /// Output modalities reported by xAI.
    pub output_modalities: Vec<String>,
    /// Whether the descriptor does not contradict text-in/text-out Chat support.
    pub text_conversation_ready: bool,
}

/// Live official xAI catalog plus the daemon-owned selection snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatModelCatalog {
    /// Bounded descriptors returned by the fixed official adapter.
    pub models: Vec<ChatModelCatalogEntry>,
    /// Durable canonical selection applied to new turns.
    pub preference: ChatModelPreference,
    /// Product default used for a fresh profile.
    pub default_model_id: String,
    /// The current selection is present and text-capable in this exact catalog.
    pub selected_model_ready: bool,
    /// The product default is present and text-capable in this exact catalog.
    pub default_model_ready: bool,
}

/// Revisioned request to select a live-discovered xAI Chat model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectChatModel {
    /// Preference revision observed by the caller.
    pub expected_revision: u64,
    /// Exact canonical identifier or advertised alias from the live catalog.
    pub model_id: String,
}

/// Discovers official xAI models and owns the default model policy for new Chat turns.
pub struct ChatModelService {
    store: Arc<dyn ChatModelPreferenceStore>,
    credentials: Arc<CredentialService>,
    factory: Arc<dyn ConversationModelFactory>,
    supergrok: Option<Arc<SuperGrokEnrollmentService>>,
    supergrok_factory: Option<Arc<dyn ConversationModelFactory>>,
    rail: Arc<crate::ChatRailSelection>,
    clock: Arc<dyn Clock>,
}

impl ChatModelService {
    /// Creates the service from daemon-owned stores, credentials, and the fixed provider factory.
    #[must_use]
    pub fn new(
        store: Arc<dyn ChatModelPreferenceStore>,
        credentials: Arc<CredentialService>,
        factory: Arc<dyn ConversationModelFactory>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            store,
            credentials,
            factory,
            supergrok: None,
            supergrok_factory: None,
            rail: Arc::new(crate::ChatRailSelection::new(ChatRail::XaiApiKey)),
            clock,
        }
    }

    /// Creates a catalog service for one explicit credential rail.
    #[must_use]
    pub fn new_with_supergrok(
        store: Arc<dyn ChatModelPreferenceStore>,
        credentials: Arc<CredentialService>,
        api_key_factory: Arc<dyn ConversationModelFactory>,
        supergrok: Arc<SuperGrokEnrollmentService>,
        supergrok_factory: Arc<dyn ConversationModelFactory>,
        rail: Arc<crate::ChatRailSelection>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            store,
            credentials,
            factory: api_key_factory,
            supergrok: Some(supergrok),
            supergrok_factory: Some(supergrok_factory),
            rail,
            clock,
        }
    }

    /// Loads the durable selection without making a provider request.
    ///
    /// # Errors
    ///
    /// Returns a storage error if the canonical singleton cannot be read.
    pub async fn preference(&self) -> Result<ChatModelPreference, ApplicationError> {
        Ok(self.store.get_chat_model_preference().await?)
    }

    /// Retrieves a live catalog using the daemon-owned key.
    ///
    /// # Errors
    ///
    /// Returns a sanitized credential, provider, timeout, or persistence error. A stale
    /// catalog is never returned as if it were live.
    pub async fn catalog(&self) -> Result<ChatModelCatalog, ApplicationError> {
        let preference = self.store.get_chat_model_preference().await?;
        let models = self.discover().await?;
        Ok(catalog_snapshot(preference, models))
    }

    /// Validates a requested identifier against a live catalog, canonicalizes aliases,
    /// and atomically commits the new selection.
    ///
    /// An exact completed replay returns before credential access or another provider
    /// call. If the process stops before the local commit, repeating discovery is safe
    /// because model listing is read-only.
    ///
    /// # Errors
    ///
    /// Returns invalid input, unavailable, conflict, credential, timeout, or storage errors.
    pub async fn select(
        &self,
        input: SelectChatModel,
        idempotency_key: &str,
    ) -> Result<ChatModelPreference, ApplicationError> {
        validate_identifier(
            &input.model_id,
            MAX_MODEL_ID_BYTES,
            "selected model identifier",
        )?;
        let expected_revision = input.expected_revision.to_be_bytes();
        let command = mutation_command_bytes(
            "select_chat_model",
            idempotency_key,
            &[&expected_revision, input.model_id.as_bytes()],
        )?;
        if let Some(preference) = self
            .store
            .resolve_chat_model_preference_mutation(&command)
            .await?
        {
            return Ok(preference);
        }

        let models = self.discover().await?;
        let canonical_model_id = canonical_text_model_id(&input.model_id, &models)?;
        let mut preference = self.store.get_chat_model_preference().await?;
        if preference.revision != input.expected_revision {
            return Err(ApplicationError::Conflict);
        }
        preference.select_model(canonical_model_id, self.clock.now())?;
        Ok(self
            .store
            .save_chat_model_preference(preference, input.expected_revision, &command)
            .await?)
    }

    async fn discover(&self) -> Result<Vec<ChatModelCatalogEntry>, ApplicationError> {
        let models = match self.rail.current() {
            ChatRail::XaiApiKey => {
                let (credential, _credential_use) =
                    self.credentials.load_xai_api_credential_for_use().await?;
                let (api_key, _) = credential.into_parts();
                let model = self.factory.create(api_key).map_err(map_model_error)?;
                tokio::time::timeout(MODEL_DISCOVERY_TIMEOUT, model.list_models())
                    .await
                    .map_err(|_| ApplicationError::DeadlineExceeded)?
                    .map_err(map_model_error)?
            }
            ChatRail::SuperGrokApi => {
                let service = self.supergrok.as_ref().ok_or_else(|| {
                    ApplicationError::Unavailable("SuperGrok API Chat is not configured".into())
                })?;
                let factory = self.supergrok_factory.as_ref().ok_or_else(|| {
                    ApplicationError::Unavailable("SuperGrok API Chat is not configured".into())
                })?;
                let now_ms = i64::try_from(self.clock.now()).unwrap_or(i64::MAX);
                let (credential, _credential_use) =
                    service.credential_for_use(now_ms, 120_000).await?;
                let secret = crate::SecretValue::new(credential.access_token.as_bytes().to_vec())
                    .map_err(|_| {
                    ApplicationError::Integrity("OAuth credential is invalid".into())
                })?;
                let model = factory.create(secret).map_err(map_model_error)?;
                tokio::time::timeout(MODEL_DISCOVERY_TIMEOUT, model.list_models())
                    .await
                    .map_err(|_| ApplicationError::DeadlineExceeded)?
                    .map_err(map_model_error)?
            }
        };
        validate_catalog(models)
    }
}

fn catalog_snapshot(
    preference: ChatModelPreference,
    models: Vec<ChatModelCatalogEntry>,
) -> ChatModelCatalog {
    let selected_model_ready = canonical_text_model_id(&preference.selected_model_id, &models)
        .is_ok_and(|canonical| {
            preference.revision == 0 || canonical == preference.selected_model_id
        });
    let default_model_ready = canonical_text_model_id(DEFAULT_XAI_CHAT_MODEL_ID, &models).is_ok();
    ChatModelCatalog {
        models,
        preference,
        default_model_id: DEFAULT_XAI_CHAT_MODEL_ID.into(),
        selected_model_ready,
        default_model_ready,
    }
}

pub(crate) fn validate_catalog(
    models: Vec<ModelDescriptor>,
) -> Result<Vec<ChatModelCatalogEntry>, ApplicationError> {
    if models.len() > MAX_DISCOVERED_MODELS {
        return Err(ApplicationError::Unavailable(
            "xAI returned too many model descriptors".into(),
        ));
    }
    let mut identifiers = HashSet::with_capacity(models.len());
    for model in &models {
        validate_identifier(&model.id, MAX_MODEL_ID_BYTES, "model identifier")?;
        if !identifiers.insert(model.id.clone()) {
            return Err(ApplicationError::Unavailable(
                "xAI returned duplicate model identifiers".into(),
            ));
        }
    }
    let mut advertised_identifiers = identifiers;
    let mut entries = Vec::with_capacity(models.len());
    for model in models {
        if model.aliases.len() > MAX_MODEL_ALIASES
            || model.input_modalities.len() > MAX_MODEL_MODALITIES
            || model.output_modalities.len() > MAX_MODEL_MODALITIES
        {
            return Err(ApplicationError::Unavailable(
                "xAI returned an oversized model descriptor".into(),
            ));
        }
        for alias in &model.aliases {
            validate_identifier(alias, MAX_MODEL_ID_BYTES, "model alias")?;
            if !advertised_identifiers.insert(alias.clone()) {
                return Err(ApplicationError::Unavailable(
                    "xAI returned an ambiguous model alias".into(),
                ));
            }
        }
        for modality in model
            .input_modalities
            .iter()
            .chain(&model.output_modalities)
        {
            validate_identifier(modality, MAX_MODALITY_BYTES, "model modality")?;
        }
        let text_conversation_ready = supports_text_conversation(&model);
        entries.push(ChatModelCatalogEntry {
            id: model.id,
            aliases: model.aliases,
            input_modalities: model.input_modalities,
            output_modalities: model.output_modalities,
            text_conversation_ready,
        });
    }
    Ok(entries)
}

pub(crate) fn canonical_text_model_id(
    requested: &str,
    models: &[ChatModelCatalogEntry],
) -> Result<String, ApplicationError> {
    if let Some(model) = models.iter().find(|model| model.id == requested) {
        return require_text_model(model);
    }
    let mut aliases = models
        .iter()
        .filter(|model| model.aliases.iter().any(|alias| alias == requested));
    let Some(model) = aliases.next() else {
        return Err(ApplicationError::Unavailable(
            "the selected xAI model is unavailable to this API key".into(),
        ));
    };
    if aliases.next().is_some() {
        return Err(ApplicationError::Unavailable(
            "xAI returned an ambiguous model alias".into(),
        ));
    }
    require_text_model(model)
}

fn require_text_model(model: &ChatModelCatalogEntry) -> Result<String, ApplicationError> {
    if !model.text_conversation_ready {
        return Err(ApplicationError::Unavailable(
            "the selected xAI model does not advertise text conversation".into(),
        ));
    }
    Ok(model.id.clone())
}

fn supports_text_conversation(model: &ModelDescriptor) -> bool {
    // Official Imagine media models must never become the durable Chat selection
    // even when the provider omits modality lists (empty = no text contradiction).
    if is_imagine_media_model_id(&model.id)
        || model
            .aliases
            .iter()
            .any(|alias| is_imagine_media_model_id(alias))
    {
        return false;
    }
    (model.input_modalities.is_empty()
        || model.input_modalities.iter().any(|value| value == "text"))
        && (model.output_modalities.is_empty()
            || model.output_modalities.iter().any(|value| value == "text"))
}

/// Canonical and alias identifiers for official Grok Imagine media models.
fn is_imagine_media_model_id(id: &str) -> bool {
    let normalized = id.trim().to_ascii_lowercase();
    normalized.starts_with("grok-imagine-")
}

fn validate_identifier(
    value: &str,
    maximum_bytes: usize,
    field: &str,
) -> Result<(), ApplicationError> {
    if value.is_empty()
        || value.len() > maximum_bytes
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(ApplicationError::InvalidInput(format!(
            "{field} is invalid"
        )));
    }
    Ok(())
}

fn map_model_error(error: ModelError) -> ApplicationError {
    match error.kind {
        ModelErrorKind::Authentication => ApplicationError::Unauthorized(error.message),
        ModelErrorKind::Forbidden => ApplicationError::Unavailable(
            "the configured xAI key cannot resolve model capabilities".into(),
        ),
        ModelErrorKind::InvalidRequest => ApplicationError::InvalidInput(error.message),
        ModelErrorKind::RateLimited | ModelErrorKind::Unavailable | ModelErrorKind::Protocol => {
            ApplicationError::Unavailable(error.message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str, aliases: &[&str]) -> ModelDescriptor {
        ModelDescriptor {
            id: id.into(),
            aliases: aliases.iter().map(|value| (*value).into()).collect(),
            input_modalities: vec!["text".into()],
            output_modalities: vec!["text".into()],
        }
    }

    #[test]
    fn canonicalizes_unique_aliases_and_rejects_ambiguous_aliases() {
        let models = validate_catalog(vec![
            model("grok-a", &["grok-current"]),
            model("grok-b", &["grok-preview"]),
        ])
        .expect("catalog");
        assert_eq!(
            canonical_text_model_id("grok-current", &models).expect("alias"),
            "grok-a"
        );

        assert!(matches!(
            validate_catalog(vec![
                model("grok-a", &["shared"]),
                model("grok-b", &["shared"]),
            ]),
            Err(ApplicationError::Unavailable(message)) if message.contains("ambiguous")
        ));
    }

    #[test]
    fn explicitly_non_text_models_are_not_ready() {
        let mut image = model("grok-image", &[]);
        image.output_modalities = vec!["image".into()];
        let models = validate_catalog(vec![image]).expect("catalog");
        assert!(!models[0].text_conversation_ready);
        assert!(canonical_text_model_id("grok-image", &models).is_err());
    }

    #[test]
    fn official_imagine_media_ids_are_never_text_conversation_ready() {
        let catalog = validate_catalog(vec![
            model("grok-4.3", &["grok-latest"]),
            // Empty modalities would otherwise fail-open as text-ready (ADR 0009).
            model("grok-imagine-image", &["grok-imagine-image-2026-03-02"]),
            model(
                "grok-imagine-video-1.5",
                &["grok-imagine-video-1.5-preview"],
            ),
            // Alias-only Imagine identity must also be excluded when used as id.
            model("other-media", &["grok-imagine-image-quality"]),
        ])
        .expect("catalog");

        let by_id: std::collections::HashMap<_, _> = catalog
            .iter()
            .map(|entry| (entry.id.as_str(), entry.text_conversation_ready))
            .collect();
        assert_eq!(by_id.get("grok-4.3"), Some(&true));
        assert_eq!(by_id.get("grok-imagine-image"), Some(&false));
        assert_eq!(by_id.get("grok-imagine-video-1.5"), Some(&false));
        assert_eq!(by_id.get("other-media"), Some(&false));

        assert_eq!(
            canonical_text_model_id("grok-latest", &catalog).expect("text alias"),
            "grok-4.3"
        );
        assert!(canonical_text_model_id("grok-imagine-image", &catalog).is_err());
        assert!(canonical_text_model_id("grok-imagine-image-2026-03-02", &catalog).is_err());
        assert!(canonical_text_model_id("grok-imagine-video-1.5-preview", &catalog).is_err());
    }

    #[test]
    fn rejects_alias_collisions_with_canonical_ids_and_duplicate_aliases() {
        assert!(matches!(
            validate_catalog(vec![
                model("grok-a", &["grok-b"]),
                model("grok-b", &[]),
            ]),
            Err(ApplicationError::Unavailable(message)) if message.contains("ambiguous")
        ));
        assert!(matches!(
            validate_catalog(vec![
                model("grok-a", &["shared"]),
                model("grok-b", &["shared"]),
            ]),
            Err(ApplicationError::Unavailable(message)) if message.contains("ambiguous")
        ));
        assert!(matches!(
            validate_catalog(vec![model("grok-a", &["same", "same"])]),
            Err(ApplicationError::Unavailable(message)) if message.contains("ambiguous")
        ));
    }
}
