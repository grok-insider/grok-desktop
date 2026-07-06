//! Typed adapter for official xAI REST APIs.

mod sse;

use std::{collections::HashSet, fmt, sync::Arc, time::Duration};

use async_trait::async_trait;
use futures_util::StreamExt;
use grok_application::{
    Citation, ContentPart, ConversationEvent, ConversationMessage, ConversationModel,
    ConversationModelFactory, ConversationRequest, ConversationRole, ConversationStream,
    GeneratedAsset, ImageRequest, MediaGenerator, ModelDescriptor, ModelError, ModelErrorKind,
    ModelFailureCertainty, SecretValue, ServerTool, Usage, XaiApiKeyValidation,
    XaiApiKeyValidationError, XaiApiKeyValidator,
};
use reqwest::{StatusCode, header};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::sse::SseDecoder;

const OFFICIAL_BASE_URL: &str = "https://api.x.ai";
const MAX_JSON_BYTES: usize = 4 * 1024 * 1024;
const MAX_PROMPT_BYTES: usize = 2 * 1024 * 1024;
const MAX_CONVERSATION_BYTES: usize = 2 * 1024 * 1024;
const MAX_STREAM_EVENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_DISCOVERED_MODELS: usize = 256;
const MAX_MODEL_ID_BYTES: usize = 512;
const MAX_MODEL_ALIASES: usize = 64;
const MAX_MODEL_MODALITIES: usize = 16;
const MAX_MODALITY_BYTES: usize = 64;
// Leave enough time for daemon cancellation and a typed IPC deadline response.
const XAI_API_KEY_VALIDATION_TIMEOUT: Duration = Duration::from_secs(8);

/// Explicit product-selected chat model. Availability is validated per key.
pub const DEFAULT_XAI_CONVERSATION_MODEL: &str = "grok-4.3";

/// Validated API credential whose debug output is always redacted.
#[derive(Clone)]
struct ApiKey(Arc<Zeroizing<String>>);

impl ApiKey {
    fn new(value: String) -> Result<Self, ModelError> {
        if value.is_empty()
            || value.len() > 4096
            || value.chars().any(char::is_control)
            || value.trim() != value
        {
            return Err(invalid("xAI API key has an invalid format"));
        }
        Ok(Self(Arc::new(Zeroizing::new(value))))
    }

    fn expose(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ApiKey([REDACTED])")
    }
}

/// Official xAI adapter shared by conversation and media use cases.
#[derive(Clone)]
pub struct XaiClient {
    http: reqwest::Client,
    api_key: ApiKey,
    authorization: header::HeaderValue,
    base_url: Arc<str>,
}

/// Validates BYOK credentials only against the fixed official xAI origin.
#[derive(Clone)]
pub struct OfficialXaiApiKeyValidator {
    test_base_url: Option<Arc<str>>,
    validation_timeout: Duration,
}

/// Factory restricted to the fixed official xAI origin.
#[derive(Debug, Clone, Copy, Default)]
pub struct OfficialXaiConversationModelFactory;

impl Default for OfficialXaiApiKeyValidator {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for OfficialXaiApiKeyValidator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OfficialXaiApiKeyValidator")
            .finish_non_exhaustive()
    }
}

impl OfficialXaiApiKeyValidator {
    /// Creates a validator restricted to `https://api.x.ai`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            test_base_url: None,
            validation_timeout: XAI_API_KEY_VALIDATION_TIMEOUT,
        }
    }

    #[cfg(test)]
    fn for_test(base_url: &str) -> Self {
        Self {
            test_base_url: Some(Arc::from(base_url)),
            validation_timeout: XAI_API_KEY_VALIDATION_TIMEOUT,
        }
    }

    #[cfg(test)]
    fn for_test_with_timeout(base_url: &str, validation_timeout: Duration) -> Self {
        Self {
            test_base_url: Some(Arc::from(base_url)),
            validation_timeout,
        }
    }
}

#[async_trait]
impl XaiApiKeyValidator for OfficialXaiApiKeyValidator {
    async fn validate(
        &self,
        api_key: &SecretValue,
    ) -> Result<XaiApiKeyValidation, XaiApiKeyValidationError> {
        let key = String::from_utf8(api_key.expose_secret().to_vec())
            .map_err(|_| XaiApiKeyValidationError::InvalidFormat)?;
        let client = if let Some(base_url) = &self.test_base_url {
            XaiClient::with_base_url(key, base_url)
        } else {
            XaiClient::new(key)
        }
        .map_err(|error| map_credential_validation(&error))?;
        let result = tokio::time::timeout(self.validation_timeout, client.list_models())
            .await
            .map_err(|_| XaiApiKeyValidationError::Unavailable)?;
        match result {
            Ok(models) => Ok(if models.iter().any(supports_text_conversation) {
                XaiApiKeyValidation::CapabilitiesResolved
            } else {
                XaiApiKeyValidation::CapabilitiesUnresolved
            }),
            Err(error) if error.kind == ModelErrorKind::Forbidden => {
                Ok(XaiApiKeyValidation::CapabilitiesUnresolved)
            }
            Err(error) => Err(map_credential_validation(&error)),
        }
    }
}

fn supports_text_conversation(model: &ModelDescriptor) -> bool {
    (model.input_modalities.is_empty()
        || model.input_modalities.iter().any(|value| value == "text"))
        && (model.output_modalities.is_empty()
            || model.output_modalities.iter().any(|value| value == "text"))
}

impl ConversationModelFactory for OfficialXaiConversationModelFactory {
    fn create(&self, api_key: SecretValue) -> Result<Arc<dyn ConversationModel>, ModelError> {
        let value = String::from_utf8(api_key.expose_secret().to_vec())
            .map_err(|_| invalid("xAI API key has an invalid format"))?;
        Ok(Arc::new(XaiClient::new(value)?))
    }
}

impl fmt::Debug for XaiClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("XaiClient")
            .field("api_key", &self.api_key)
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl XaiClient {
    /// Creates a client restricted to the official xAI origin.
    ///
    /// # Errors
    ///
    /// Returns an invalid-request error when the key format is unsafe, or a
    /// protocol error when the hardened HTTP client cannot be constructed.
    pub fn new(api_key: String) -> Result<Self, ModelError> {
        Self::with_base_url(api_key, OFFICIAL_BASE_URL)
    }

    fn with_base_url(api_key: String, base_url: &str) -> Result<Self, ModelError> {
        let api_key = ApiKey::new(api_key)?;
        let authorization = {
            let value = Zeroizing::new(format!("Bearer {}", api_key.expose()));
            let mut header = header::HeaderValue::from_str(value.as_str())
                .map_err(|_| invalid("xAI API key has an invalid format"))?;
            header.set_sensitive(true);
            header
        };
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_hours(1))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("grok-desktop/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| protocol("failed to construct the xAI HTTP client"))?;
        Ok(Self {
            http,
            api_key,
            authorization,
            base_url: Arc::from(base_url.trim_end_matches('/')),
        })
    }

    #[cfg(test)]
    fn for_test(api_key: &str, base_url: &str) -> Self {
        Self::with_base_url(api_key.into(), base_url).expect("test client")
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.base_url))
            .header(header::AUTHORIZATION, self.authorization.clone())
            .header(header::ACCEPT, "application/json")
    }

    async fn checked_json<T: for<'de> Deserialize<'de>>(
        response: reqwest::Response,
    ) -> Result<T, ModelError> {
        let status = response.status();
        if !status.is_success() {
            return Err(http_status(status));
        }
        if response
            .content_length()
            .is_some_and(|size| usize::try_from(size).map_or(true, |size| size > MAX_JSON_BYTES))
        {
            return Err(protocol("xAI response exceeded the configured size limit"));
        }
        let capacity = response
            .content_length()
            .and_then(|size| usize::try_from(size).ok())
            .unwrap_or_default()
            .min(MAX_JSON_BYTES);
        let mut bytes = Vec::with_capacity(capacity);
        let mut chunks = response.bytes_stream();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|error| map_transport(&error))?;
            if bytes.len().saturating_add(chunk.len()) > MAX_JSON_BYTES {
                return Err(protocol("xAI response exceeded the configured size limit"));
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| protocol("xAI returned malformed JSON"))
    }
}

#[async_trait]
impl ConversationModel for XaiClient {
    async fn list_models(&self) -> Result<Vec<ModelDescriptor>, ModelError> {
        let response = self
            .request(reqwest::Method::GET, "/v1/models")
            .send()
            .await
            .map_err(|error| map_transport(&error))?;
        let models: ModelList = Self::checked_json(response).await?;
        let values = if models.data.is_empty() {
            models.models
        } else {
            models.data
        };
        validate_model_catalog(values)
    }

    async fn stream(&self, request: ConversationRequest) -> Result<ConversationStream, ModelError> {
        let body = response_request(request)?;
        let response = self
            .request(reqwest::Method::POST, "/v1/responses")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "text/event-stream")
            .json(&body)
            .send()
            .await
            .map_err(|error| map_transport(&error))?;
        if !response.status().is_success() {
            return Err(http_status(response.status()));
        }

        let zero_data_retention = response
            .headers()
            .get("x-zero-data-retention")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| match value {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            });

        Ok(conversation_event_stream(response, zero_data_retention))
    }
}

fn conversation_event_stream(
    response: reqwest::Response,
    zero_data_retention: Option<bool>,
) -> ConversationStream {
    Box::pin(async_stream::stream! {
        if let Some(zero_data_retention) = zero_data_retention {
            yield Ok(ConversationEvent::RetentionObserved { zero_data_retention });
        }
        let mut source = response.bytes_stream();
        let mut decoder = SseDecoder::new(MAX_STREAM_EVENT_BYTES);
        let mut terminal = None;
        while let Some(chunk) = source.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    yield Err(if terminal.is_some() {
                        protocol("xAI stream transport failed after a terminal event")
                    } else {
                        map_transport(&error)
                    });
                    return;
                }
            };
            let Ok(events) = decoder.push(&chunk) else {
                yield Err(protocol("xAI stream event exceeded the configured size limit"));
                return;
            };
            let mut boundary = false;
            for data in events {
                if boundary {
                    yield Err(protocol("xAI stream contained data after its end marker"));
                    return;
                }
                if data == "[DONE]" {
                    boundary = true;
                    continue;
                }
                if terminal.is_some() {
                    yield Err(protocol("xAI stream contained data after a terminal event"));
                    return;
                }
                match parse_stream_event(&data) {
                    Ok(events) => {
                        if events
                            .iter()
                            .any(|event| matches!(event, ConversationEvent::Completed { .. }))
                        {
                            terminal = Some(PendingTerminal::Success(events));
                        } else {
                            for event in events {
                                yield Ok(event);
                            }
                        }
                    }
                    Err(error) if error.certainty == ModelFailureCertainty::KnownFailure => {
                        terminal = Some(PendingTerminal::Failure(error));
                    }
                    Err(error) => {
                        yield Err(error);
                        return;
                    }
                }
            }
            if boundary {
                if decoder.has_pending_data() {
                    yield Err(protocol("xAI stream contained bytes after its end marker"));
                    return;
                }
                match terminal_boundary(terminal.take()) {
                    Ok(events) => {
                        for event in events {
                            yield Ok(event);
                        }
                    }
                    Err(error) => yield Err(error),
                }
                return;
            }
        }
        if decoder.has_pending_data() {
            yield Err(protocol("xAI stream ended with an incomplete event"));
            return;
        }
        match terminal_boundary(terminal) {
            Ok(events) => {
                for event in events {
                    yield Ok(event);
                }
            }
            Err(error) => yield Err(error),
        }
    })
}

enum PendingTerminal {
    Success(Vec<ConversationEvent>),
    Failure(ModelError),
}

fn terminal_boundary(
    terminal: Option<PendingTerminal>,
) -> Result<Vec<ConversationEvent>, ModelError> {
    match terminal {
        Some(PendingTerminal::Success(events)) => Ok(events),
        Some(PendingTerminal::Failure(error)) => Err(error),
        None => Err(protocol("xAI stream ended without a terminal response")),
    }
}

#[async_trait]
impl MediaGenerator for XaiClient {
    async fn generate_images(
        &self,
        request: ImageRequest,
    ) -> Result<Vec<GeneratedAsset>, ModelError> {
        validate_identifier("image model", &request.model)?;
        validate_prompt(&request.prompt)?;
        if !(1..=8).contains(&request.count) {
            return Err(invalid("image count must be between 1 and 8"));
        }
        let response = self
            .request(reqwest::Method::POST, "/v1/images/generations")
            .header(header::CONTENT_TYPE, "application/json")
            .json(&json!({
                "model": request.model,
                "prompt": request.prompt,
                "n": request.count,
                "response_format": "url"
            }))
            .send()
            .await
            .map_err(|error| map_transport(&error))?;
        let response: ImageResponse = Self::checked_json(response).await?;
        if response.data.is_empty() {
            return Err(protocol("xAI returned no generated images"));
        }
        let cost = response.usage.cost_in_usd_ticks;
        response
            .data
            .into_iter()
            .map(|asset| {
                validate_https_url(&asset.url)?;
                if !asset.mime_type.starts_with("image/") {
                    return Err(protocol("xAI returned an unsupported image media type"));
                }
                Ok(GeneratedAsset {
                    url: asset.url,
                    media_type: asset.mime_type,
                    revised_prompt: non_empty(asset.revised_prompt),
                    cost_in_usd_ticks: cost,
                })
            })
            .collect()
    }
}

fn response_request(request: ConversationRequest) -> Result<Value, ModelError> {
    validate_identifier("model", &request.model)?;
    if request.messages.is_empty() || request.messages.len() > 1_000 {
        return Err(invalid(
            "conversation must contain between 1 and 1000 messages",
        ));
    }
    if request.continuation.as_deref().is_some_and(|value| {
        value.is_empty() || value.len() > 512 || value.chars().any(char::is_control)
    }) {
        return Err(invalid("continuation identifier has an invalid format"));
    }
    let aggregate_bytes = request.messages.iter().try_fold(0usize, |total, message| {
        message.content.iter().try_fold(total, |total, part| {
            let length = match part {
                ContentPart::Text(value)
                | ContentPart::FileReference(value)
                | ContentPart::ImageUrl(value) => value.len(),
            };
            total.checked_add(length)
        })
    });
    if aggregate_bytes.is_none_or(|bytes| bytes > MAX_CONVERSATION_BYTES) {
        return Err(invalid(
            "conversation exceeds the configured aggregate size limit",
        ));
    }
    let input = request
        .messages
        .into_iter()
        .map(message_value)
        .collect::<Result<Vec<_>, _>>()?;
    let tools: Vec<Value> = request
        .tools
        .into_iter()
        .map(|tool| match tool {
            ServerTool::WebSearch => json!({ "type": "web_search" }),
            ServerTool::XSearch => json!({ "type": "x_search" }),
        })
        .collect();
    let mut body = json!({
        "model": request.model,
        "input": input,
        "stream": true,
        "store": request.store,
        "tools": tools
    });
    if let Some(continuation) = request.continuation {
        body["previous_response_id"] = Value::String(continuation);
    }
    Ok(body)
}

fn message_value(message: ConversationMessage) -> Result<Value, ModelError> {
    if message.content.is_empty() || message.content.len() > 100 {
        return Err(invalid(
            "message content must contain between 1 and 100 parts",
        ));
    }
    let role = match message.role {
        ConversationRole::System => "system",
        ConversationRole::User => "user",
        ConversationRole::Assistant => "assistant",
    };
    let content = message
        .content
        .into_iter()
        .map(|part| match part {
            ContentPart::Text(text) => {
                validate_prompt(&text)?;
                Ok(json!({ "type": "input_text", "text": text }))
            }
            ContentPart::FileReference(file_id) => {
                validate_identifier("file identifier", &file_id)?;
                Ok(json!({ "type": "input_file", "file_id": file_id }))
            }
            ContentPart::ImageUrl(url) => {
                validate_https_url(&url)?;
                Ok(json!({ "type": "input_image", "image_url": url }))
            }
        })
        .collect::<Result<Vec<_>, ModelError>>()?;
    Ok(json!({ "role": role, "content": content }))
}

fn parse_stream_event(data: &str) -> Result<Vec<ConversationEvent>, ModelError> {
    if data == "[DONE]" {
        return Ok(Vec::new());
    }
    let value: Value = serde_json::from_str(data)
        .map_err(|_| protocol("xAI returned a malformed stream event"))?;
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .filter(|kind| !kind.is_empty())
        .ok_or_else(|| protocol("xAI returned a malformed stream event"))?;
    match kind {
        "response.created" | "response.in_progress" => Ok(vec![ConversationEvent::Started {
            continuation: response_id(&value),
        }]),
        "response.output_text.delta" => value
            .get("delta")
            .and_then(Value::as_str)
            .map(|delta| vec![ConversationEvent::TextDelta(delta.into())])
            .ok_or_else(|| protocol("xAI text delta did not contain text")),
        "response.output_text.annotation.added" => Ok(citation_events(citations_from_annotation(
            value.get("annotation"),
        )?)),
        "response.output_text.done" => Ok(citation_events(citations_from_annotations(
            value.get("annotations"),
        )?)),
        "response.completed" => completed_events(&value),
        "response.incomplete" => incomplete_events(&value),
        "response.done" => match embedded_response_status(&value)? {
            "completed" => completed_events(&value),
            "incomplete" => incomplete_events(&value),
            _ => Err(protocol("xAI returned a contradictory terminal event")),
        },
        "response.failed" => failed_event(&value),
        "error" => error_event(&value),
        // Unknown future events and reasoning deltas are not user-visible output.
        _ => Ok(Vec::new()),
    }
}

fn completed_events(value: &Value) -> Result<Vec<ConversationEvent>, ModelError> {
    let response = embedded_response(value)?;
    require_response_object(response)?;
    require_response_status(response, "completed")?;
    reject_non_null(response, "error")?;
    reject_non_null(response, "incomplete_details")?;
    if !response.get("output").is_some_and(Value::is_array) {
        return Err(protocol("xAI returned a malformed completed response"));
    }
    let continuation = required_response_id(response)?;
    let usage = required_usage(response)?;
    let mut events = citation_events(citations_from_completed_response(response)?);
    events.push(ConversationEvent::Usage(usage));
    events.push(ConversationEvent::Completed {
        continuation: Some(continuation),
    });
    Ok(events)
}

fn incomplete_events(value: &Value) -> Result<Vec<ConversationEvent>, ModelError> {
    let response = embedded_response(value)?;
    validate_optional_response_object(response)?;
    validate_optional_response_status(response, "incomplete")?;
    reject_non_null(response, "error")?;
    validate_optional_usage(response)?;
    let reason = incomplete_reason(response)?;
    let error = match reason {
        Some("max_output_tokens" | "max_prompt_tokens") => ModelError {
            kind: ModelErrorKind::InvalidRequest,
            message: "xAI stopped the response after reaching a token limit".into(),
            retryable: false,
            certainty: ModelFailureCertainty::KnownFailure,
        },
        Some("max_time_limit") => ModelError {
            kind: ModelErrorKind::Unavailable,
            message: "xAI stopped the response after reaching its time limit".into(),
            retryable: true,
            certainty: ModelFailureCertainty::KnownFailure,
        },
        _ => ModelError {
            kind: ModelErrorKind::Protocol,
            message: "xAI returned an unsupported incomplete response reason".into(),
            retryable: false,
            certainty: ModelFailureCertainty::KnownFailure,
        },
    };
    Err(error)
}

fn failed_event(value: &Value) -> Result<Vec<ConversationEvent>, ModelError> {
    let response = embedded_response(value)?;
    validate_optional_response_object(response)?;
    validate_optional_response_status(response, "failed")?;
    validate_optional_usage(response)?;
    validate_optional_incomplete_details(response)?;
    if let Some(error) = response.get("error").filter(|error| !error.is_null()) {
        validate_provider_error(error)?;
    }
    Err(known_provider_failure())
}

fn error_event(value: &Value) -> Result<Vec<ConversationEvent>, ModelError> {
    match value.get("error") {
        Some(error) if !error.is_null() => {
            validate_provider_error(error)?;
            if value.get("message").is_some() {
                validate_provider_error(value)?;
            }
        }
        _ => validate_provider_error(value)?,
    }
    Err(known_provider_failure())
}

fn known_provider_failure() -> ModelError {
    ModelError {
        kind: ModelErrorKind::Unavailable,
        message: "xAI could not complete the response".into(),
        retryable: false,
        certainty: ModelFailureCertainty::KnownFailure,
    }
}

fn embedded_response_status(value: &Value) -> Result<&str, ModelError> {
    let response = embedded_response(value)?;
    validate_optional_response_object(response)?;
    response
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| protocol("xAI returned a malformed terminal event"))
}

fn embedded_response(value: &Value) -> Result<&Value, ModelError> {
    value
        .get("response")
        .filter(|response| response.is_object())
        .ok_or_else(|| protocol("xAI returned a malformed terminal event"))
}

fn require_response_object(response: &Value) -> Result<(), ModelError> {
    if response.get("object").and_then(Value::as_str) == Some("response") {
        Ok(())
    } else {
        Err(protocol("xAI returned a malformed terminal event"))
    }
}

fn validate_optional_response_object(response: &Value) -> Result<(), ModelError> {
    match response.get("object") {
        None => Ok(()),
        Some(Value::String(object)) if object == "response" => Ok(()),
        Some(_) => Err(protocol("xAI returned a malformed terminal event")),
    }
}

fn require_response_status(response: &Value, expected: &str) -> Result<(), ModelError> {
    if response.get("status").and_then(Value::as_str) == Some(expected) {
        Ok(())
    } else {
        Err(protocol("xAI returned a contradictory terminal event"))
    }
}

fn validate_optional_response_status(response: &Value, expected: &str) -> Result<(), ModelError> {
    match response.get("status") {
        None => Ok(()),
        Some(Value::String(status)) if status == expected => Ok(()),
        Some(_) => Err(protocol("xAI returned a contradictory terminal event")),
    }
}

fn reject_non_null(response: &Value, field: &str) -> Result<(), ModelError> {
    if response.get(field).is_some_and(|value| !value.is_null()) {
        Err(protocol("xAI returned a contradictory terminal event"))
    } else {
        Ok(())
    }
}

fn required_response_id(response: &Value) -> Result<String, ModelError> {
    let identifier = response
        .get("id")
        .and_then(Value::as_str)
        .filter(|identifier| {
            !identifier.is_empty()
                && identifier.len() <= 512
                && identifier.trim() == *identifier
                && !identifier.chars().any(char::is_control)
        })
        .ok_or_else(|| protocol("xAI returned an invalid response identifier"))?;
    Ok(identifier.to_owned())
}

fn required_usage(response: &Value) -> Result<Usage, ModelError> {
    let usage = response
        .get("usage")
        .filter(|usage| usage.is_object())
        .ok_or_else(|| protocol("xAI completed a response without valid usage accounting"))?;
    usage_value(usage)
}

fn validate_optional_usage(response: &Value) -> Result<(), ModelError> {
    match response.get("usage") {
        None | Some(Value::Null) => Ok(()),
        Some(usage) if usage.is_object() => usage_value(usage).map(|_| ()),
        Some(_) => Err(protocol("xAI returned malformed usage accounting")),
    }
}

fn usage_value(usage: &Value) -> Result<Usage, ModelError> {
    let required_unsigned = |field| {
        usage
            .get(field)
            .and_then(Value::as_u64)
            .ok_or_else(|| protocol("xAI returned malformed usage accounting"))
    };
    Ok(Usage {
        input_tokens: required_unsigned("input_tokens")?,
        output_tokens: required_unsigned("output_tokens")?,
        cost_in_usd_ticks: required_unsigned("cost_in_usd_ticks")?,
    })
}

fn incomplete_reason(response: &Value) -> Result<Option<&str>, ModelError> {
    match response.get("incomplete_details") {
        None | Some(Value::Null) => Ok(None),
        Some(details) if details.is_object() => details
            .get("reason")
            .and_then(Value::as_str)
            .map(Some)
            .ok_or_else(|| protocol("xAI returned malformed incomplete response details")),
        Some(_) => Err(protocol(
            "xAI returned malformed incomplete response details",
        )),
    }
}

fn validate_optional_incomplete_details(response: &Value) -> Result<(), ModelError> {
    incomplete_reason(response).map(|_| ())
}

fn validate_provider_error(error: &Value) -> Result<(), ModelError> {
    if !error.is_object() {
        return Err(protocol("xAI returned a malformed error event"));
    }
    let Some(message) = error.get("message").and_then(Value::as_str) else {
        return Err(protocol("xAI returned a malformed error event"));
    };
    if message.is_empty() || message.len() > 8_192 {
        return Err(protocol("xAI returned a malformed error event"));
    }
    for field in ["type", "code", "param"] {
        match error.get(field) {
            None | Some(Value::Null) => {}
            Some(Value::String(value)) if value.len() <= 512 => {}
            Some(_) => return Err(protocol("xAI returned a malformed error event")),
        }
    }
    match error.get("sequence_number") {
        None | Some(Value::Null) => {}
        Some(sequence)
            if sequence
                .as_u64()
                .is_some_and(|sequence| sequence <= 9_007_199_254_740_991) => {}
        Some(_) => return Err(protocol("xAI returned a malformed error event")),
    }
    Ok(())
}

fn response_id(value: &Value) -> Option<String> {
    value
        .pointer("/response/id")
        .or_else(|| value.get("response_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 512)
        .map(str::to_owned)
}

fn citations_from_completed_response(response: &Value) -> Result<Vec<Citation>, ModelError> {
    let mut found = Vec::new();
    let output = response
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol("xAI returned a malformed completed response"))?;
    for item in output {
        if item.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let content = item
            .get("content")
            .and_then(Value::as_array)
            .ok_or_else(|| protocol("xAI returned malformed message output"))?;
        for part in content {
            if part.get("type").and_then(Value::as_str) == Some("output_text") {
                append_annotations(part.get("annotations"), &mut found)?;
            }
        }
    }
    Ok(found)
}

fn citations_from_annotation(annotation: Option<&Value>) -> Result<Vec<Citation>, ModelError> {
    let mut found = Vec::new();
    let annotation =
        annotation.ok_or_else(|| protocol("xAI returned a malformed citation annotation event"))?;
    append_annotation(annotation, &mut found)?;
    Ok(found)
}

fn citations_from_annotations(annotations: Option<&Value>) -> Result<Vec<Citation>, ModelError> {
    let mut found = Vec::new();
    append_annotations(annotations, &mut found)?;
    Ok(found)
}

fn append_annotations(
    annotations: Option<&Value>,
    found: &mut Vec<Citation>,
) -> Result<(), ModelError> {
    let annotations = match annotations {
        None | Some(Value::Null) => return Ok(()),
        Some(Value::Array(annotations)) => annotations,
        Some(_) => return Err(protocol("xAI returned malformed citation annotations")),
    };
    for annotation in annotations {
        append_annotation(annotation, found)?;
    }
    Ok(())
}

fn append_annotation(annotation: &Value, found: &mut Vec<Citation>) -> Result<(), ModelError> {
    let kind = annotation
        .get("type")
        .and_then(Value::as_str)
        .filter(|kind| !kind.is_empty())
        .ok_or_else(|| protocol("xAI returned a malformed citation annotation"))?;
    if kind != "url_citation" {
        return Ok(());
    }
    let url = annotation
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| protocol("xAI returned a malformed URL citation"))?;
    validate_https_url(url).map_err(|_| protocol("xAI returned a malformed URL citation"))?;
    let title = match annotation.get("title") {
        None | Some(Value::Null) => None,
        Some(Value::String(title)) if title.len() <= 500 => Some(title.to_owned()),
        Some(_) => return Err(protocol("xAI returned a malformed URL citation")),
    };
    if found.iter().any(|citation| citation.url == url) {
        return Ok(());
    }
    if found.len() >= 256 {
        return Err(protocol("xAI returned too many unique URL citations"));
    }
    found.push(Citation {
        title,
        url: url.to_owned(),
    });
    Ok(())
}

fn citation_events(citations: Vec<Citation>) -> Vec<ConversationEvent> {
    citations
        .into_iter()
        .map(ConversationEvent::Citation)
        .collect()
}

fn validate_identifier(kind: &str, value: &str) -> Result<(), ModelError> {
    if value.is_empty()
        || value.len() > 512
        || value.chars().any(char::is_control)
        || value.trim() != value
    {
        return Err(invalid(&format!("{kind} has an invalid format")));
    }
    Ok(())
}

fn validate_prompt(value: &str) -> Result<(), ModelError> {
    if value.is_empty() || value.len() > MAX_PROMPT_BYTES {
        return Err(invalid(
            "text content is empty or exceeds the configured size limit",
        ));
    }
    Ok(())
}

fn validate_https_url(value: &str) -> Result<(), ModelError> {
    let parsed = reqwest::Url::parse(value);
    if value.len() > 8192
        || !value.starts_with("https://")
        || value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
        || value.contains('@')
        || !parsed.is_ok_and(|url| {
            url.scheme() == "https"
                && url.host_str().is_some_and(|host| !host.is_empty())
                && url.username().is_empty()
                && url.password().is_none()
        })
    {
        return Err(invalid(
            "provider media URL must be a credential-free HTTPS URL",
        ));
    }
    Ok(())
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}

fn invalid(message: &str) -> ModelError {
    ModelError {
        kind: ModelErrorKind::InvalidRequest,
        message: message.into(),
        retryable: false,
        certainty: ModelFailureCertainty::KnownFailure,
    }
}

fn protocol(message: &str) -> ModelError {
    ModelError {
        kind: ModelErrorKind::Protocol,
        message: message.into(),
        retryable: false,
        certainty: ModelFailureCertainty::OutcomeUnknown,
    }
}

fn map_transport(error: &reqwest::Error) -> ModelError {
    ModelError {
        kind: ModelErrorKind::Unavailable,
        message: if error.is_timeout() {
            "xAI request timed out"
        } else {
            "xAI request failed"
        }
        .into(),
        retryable: error.is_timeout() || error.is_connect(),
        certainty: ModelFailureCertainty::OutcomeUnknown,
    }
}

fn map_credential_validation(error: &ModelError) -> XaiApiKeyValidationError {
    match error.kind {
        ModelErrorKind::InvalidRequest => XaiApiKeyValidationError::InvalidFormat,
        ModelErrorKind::Authentication => XaiApiKeyValidationError::Rejected,
        ModelErrorKind::Forbidden
        | ModelErrorKind::RateLimited
        | ModelErrorKind::Unavailable
        | ModelErrorKind::Protocol => XaiApiKeyValidationError::Unavailable,
    }
}

fn http_status(status: StatusCode) -> ModelError {
    let (kind, retryable, message) = match status.as_u16() {
        401 => (
            ModelErrorKind::Authentication,
            false,
            "xAI rejected the configured API credential",
        ),
        403 => (
            ModelErrorKind::Forbidden,
            false,
            "the xAI API key does not permit this endpoint or model",
        ),
        400 | 404 | 409 | 422 => (
            ModelErrorKind::InvalidRequest,
            false,
            "xAI rejected the request",
        ),
        429 => (
            ModelErrorKind::RateLimited,
            true,
            "xAI rate limited the request",
        ),
        500..=599 => (
            ModelErrorKind::Unavailable,
            true,
            "xAI is temporarily unavailable",
        ),
        _ => (
            ModelErrorKind::Protocol,
            false,
            "xAI returned an unexpected HTTP status",
        ),
    };
    ModelError {
        kind,
        message: message.into(),
        retryable,
        certainty: ModelFailureCertainty::KnownFailure,
    }
}

#[derive(Debug, Deserialize)]
struct ModelList {
    #[serde(default)]
    data: Vec<RawModel>,
    #[serde(default)]
    models: Vec<RawModel>,
}

#[derive(Debug, Deserialize)]
struct RawModel {
    id: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    input_modalities: Vec<String>,
    #[serde(default)]
    output_modalities: Vec<String>,
}

fn validate_model_catalog(values: Vec<RawModel>) -> Result<Vec<ModelDescriptor>, ModelError> {
    if values.len() > MAX_DISCOVERED_MODELS {
        return Err(protocol("xAI returned too many model descriptors"));
    }
    let mut identifiers = HashSet::with_capacity(values.len());
    for model in &values {
        validate_catalog_string(&model.id, MAX_MODEL_ID_BYTES, "model identifier")?;
        if !identifiers.insert(model.id.clone()) {
            return Err(protocol("xAI returned duplicate model identifiers"));
        }
    }
    let mut advertised_identifiers = identifiers;
    let mut models = Vec::with_capacity(values.len());
    for model in values {
        if model.aliases.len() > MAX_MODEL_ALIASES {
            return Err(protocol("xAI returned too many aliases for a model"));
        }
        if model.input_modalities.len() > MAX_MODEL_MODALITIES
            || model.output_modalities.len() > MAX_MODEL_MODALITIES
        {
            return Err(protocol("xAI returned too many modalities for a model"));
        }
        for alias in &model.aliases {
            validate_catalog_string(alias, MAX_MODEL_ID_BYTES, "model alias")?;
            if !advertised_identifiers.insert(alias.clone()) {
                return Err(protocol("xAI returned an ambiguous model alias"));
            }
        }
        for modality in model
            .input_modalities
            .iter()
            .chain(&model.output_modalities)
        {
            validate_catalog_string(modality, MAX_MODALITY_BYTES, "model modality")?;
        }
        models.push(ModelDescriptor {
            id: model.id,
            aliases: model.aliases,
            input_modalities: model.input_modalities,
            output_modalities: model.output_modalities,
        });
    }
    Ok(models)
}

fn validate_catalog_string(
    value: &str,
    maximum_bytes: usize,
    field: &str,
) -> Result<(), ModelError> {
    if value.is_empty()
        || value.len() > maximum_bytes
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(protocol(&format!("xAI returned an invalid {field}")));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct ImageResponse {
    data: Vec<RawImage>,
    usage: RawUsage,
}

#[derive(Debug, Deserialize)]
struct RawImage {
    url: String,
    #[serde(default = "default_image_media_type")]
    mime_type: String,
    revised_prompt: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
struct RawUsage {
    cost_in_usd_ticks: u64,
}

fn default_image_media_type() -> String {
    "image/jpeg".into()
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use futures_util::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    async fn server(responses: Vec<&'static str>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        let mut responses: VecDeque<&'static str> = responses.into();
        tokio::spawn(async move {
            while let Some(response) = responses.pop_front() {
                let (mut stream, _) = listener.accept().await.expect("accept");
                let mut request = vec![0; 16 * 1024];
                let _ = stream.read(&mut request).await.expect("read");
                stream.write_all(response.as_bytes()).await.expect("write");
                stream.shutdown().await.expect("shutdown");
            }
        });
        format!("http://{address}")
    }

    async fn stalled_server() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            tokio::time::sleep(Duration::from_secs(1)).await;
            drop(stream);
        });
        format!("http://{address}")
    }

    fn http(body: &str, content_type: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn conversation_request() -> ConversationRequest {
        ConversationRequest {
            model: "grok-test".into(),
            messages: vec![ConversationMessage {
                role: ConversationRole::User,
                content: vec![ContentPart::Text("Hello".into())],
            }],
            continuation: None,
            tools: Vec::new(),
            store: false,
        }
    }

    async fn stream_results(body: String) -> Vec<Result<ConversationEvent, ModelError>> {
        stream_results_from_http(http(&body, "text/event-stream")).await
    }

    async fn stream_results_from_http(
        response: String,
    ) -> Vec<Result<ConversationEvent, ModelError>> {
        let base = server(vec![Box::leak(response.into_boxed_str())]).await;
        let client = XaiClient::for_test("test-key", &base);
        client
            .stream(conversation_request())
            .await
            .expect("stream response")
            .collect()
            .await
    }

    fn completed_data() -> String {
        json!({
            "type": "response.completed",
            "response": {
                "object": "response",
                "id": "resp-1",
                "status": "completed",
                "usage": {
                    "input_tokens": 1,
                    "output_tokens": 1,
                    "cost_in_usd_ticks": 0
                },
                "output": []
            }
        })
        .to_string()
    }

    fn incomplete_data() -> String {
        json!({
            "type": "response.incomplete",
            "response": {
                "incomplete_details": { "reason": "max_output_tokens" }
            }
        })
        .to_string()
    }

    fn failed_data() -> String {
        json!({
            "type": "response.failed",
            "response": {
                "error": { "code": "failed", "message": "provider detail" }
            }
        })
        .to_string()
    }

    fn sse(data: &str) -> String {
        format!("data: {data}\n\n")
    }

    #[test]
    fn credentials_are_always_redacted() {
        let key = ApiKey::new("secret-value".into()).expect("key");
        assert_eq!(format!("{key:?}"), "ApiKey([REDACTED])");
        let client =
            XaiClient::with_base_url("secret-value".into(), "https://api.x.ai").expect("client");
        assert!(client.authorization.is_sensitive());
        assert!(!format!("{client:?}").contains("secret-value"));
    }

    #[test]
    fn request_validation_rejects_non_https_media_and_empty_text() {
        let error = response_request(ConversationRequest {
            model: "grok-test".into(),
            messages: vec![ConversationMessage {
                role: ConversationRole::User,
                content: vec![ContentPart::ImageUrl(
                    "http://example.test/image.png".into(),
                )],
            }],
            continuation: None,
            tools: Vec::new(),
            store: false,
        })
        .expect_err("reject URL");
        assert_eq!(error.kind, ModelErrorKind::InvalidRequest);

        for invalid in [
            "https://",
            "https:// example.test/image.png",
            "https://#fragment",
            "https://example.test:99999/image.png",
            "https://user@example.test/image.png",
        ] {
            assert!(validate_https_url(invalid).is_err(), "{invalid}");
        }
        assert!(validate_https_url("https://example.test/image.png").is_ok());
    }

    #[test]
    fn completed_event_requires_accounting_and_extracts_exact_ordered_citations() {
        let events = parse_stream_event(
            r#"{
                "type":"response.completed",
                "response":{
                    "object":"response",
                    "id":"resp-1",
                    "status":"completed",
                    "usage":{"input_tokens":3,"output_tokens":5,"cost_in_usd_ticks":7},
                    "output":[
                        {
                            "type":"message",
                            "content":[
                                {
                                    "type":"output_text",
                                    "annotations":[
                                        {"type":"url_citation","url":"https://example.test/b","title":"B"},
                                        {"type":"url_citation","url":"https://example.test/a","title":"A"},
                                        {"type":"url_citation","url":"https://example.test/b"},
                                        {"type":"file_citation","url":"https://example.test/not-a-url-citation"}
                                    ]
                                },
                                {
                                    "type":"refusal",
                                    "annotations":[
                                        {"type":"url_citation","url":"https://example.test/ignored-content"}
                                    ]
                                }
                            ]
                        },
                        {
                            "type":"web_search_call",
                            "content":[{
                                "type":"output_text",
                                "annotations":[
                                    {"type":"url_citation","url":"https://example.test/ignored-output"}
                                ]
                            }]
                        }
                    ],
                    "nested":{"type":"url_citation","url":"https://example.test/ignored-recursive"},
                    "error":null,
                    "incomplete_details":null
                }
            }"#,
        )
        .expect("event");
        assert_eq!(
            events,
            vec![
                ConversationEvent::Citation(Citation {
                    title: Some("B".into()),
                    url: "https://example.test/b".into(),
                }),
                ConversationEvent::Citation(Citation {
                    title: Some("A".into()),
                    url: "https://example.test/a".into(),
                }),
                ConversationEvent::Usage(Usage {
                    input_tokens: 3,
                    output_tokens: 5,
                    cost_in_usd_ticks: 7,
                }),
                ConversationEvent::Completed {
                    continuation: Some("resp-1".into()),
                },
            ]
        );
    }

    #[test]
    fn completed_event_rejects_malformed_identity_status_and_usage_as_uncertain() {
        let valid = json!({
            "type": "response.completed",
            "response": {
                "object": "response",
                "id": "resp-1",
                "status": "completed",
                "usage": {
                    "input_tokens": 3,
                    "output_tokens": 5,
                    "cost_in_usd_ticks": 7
                },
                "output": []
            }
        });
        let mut malformed = Vec::new();

        let mut value = valid.clone();
        value.as_object_mut().expect("event").remove("response");
        malformed.push(value);

        for (pointer, replacement) in [
            ("/response/object", Value::Null),
            ("/response/object", json!("not-a-response")),
            ("/response/id", Value::Null),
            ("/response/id", json!("")),
            ("/response/id", json!(" response-id ")),
            ("/response/id", json!("x".repeat(513))),
            ("/response/status", Value::Null),
            ("/response/status", json!("incomplete")),
            ("/response/usage", Value::Null),
            ("/response/usage/input_tokens", Value::Null),
            ("/response/usage/input_tokens", json!(-1)),
            ("/response/usage/input_tokens", json!(1.5)),
            ("/response/usage/output_tokens", json!("5")),
            ("/response/usage/cost_in_usd_ticks", Value::Null),
        ] {
            let mut value = valid.clone();
            *value.pointer_mut(pointer).expect("fixture pointer") = replacement;
            malformed.push(value);
        }

        for value in malformed {
            let error = parse_stream_event(&value.to_string()).expect_err("malformed terminal");
            assert_eq!(error.kind, ModelErrorKind::Protocol);
            assert!(!error.retryable);
            assert_eq!(error.certainty, ModelFailureCertainty::OutcomeUnknown);
        }

        for field in ["error", "incomplete_details"] {
            let mut value = valid.clone();
            value["response"][field] = json!({ "unexpected": true });
            let error = parse_stream_event(&value.to_string()).expect_err("contradiction");
            assert_eq!(error.kind, ModelErrorKind::Protocol);
            assert_eq!(error.certainty, ModelFailureCertainty::OutcomeUnknown);
        }

        let mut missing_output = valid;
        missing_output["response"]
            .as_object_mut()
            .expect("response")
            .remove("output");
        let error =
            parse_stream_event(&missing_output.to_string()).expect_err("missing output array");
        assert_eq!(error.kind, ModelErrorKind::Protocol);
        assert_eq!(error.certainty, ModelFailureCertainty::OutcomeUnknown);
    }

    #[test]
    fn incomplete_reasons_have_local_authoritative_failure_classification() {
        let incomplete = |event_type: &str, reason: Option<&str>| {
            let mut response = json!({ "id": "resp-1" });
            if event_type == "response.done" {
                response["object"] = json!("response");
                response["status"] = json!("incomplete");
            }
            if let Some(reason) = reason {
                response["incomplete_details"] = json!({ "reason": reason });
            }
            json!({ "type": event_type, "response": response }).to_string()
        };

        for reason in ["max_output_tokens", "max_prompt_tokens"] {
            let error = parse_stream_event(&incomplete("response.incomplete", Some(reason)))
                .expect_err("token limit");
            assert_eq!(error.kind, ModelErrorKind::InvalidRequest);
            assert!(!error.retryable);
            assert_eq!(error.certainty, ModelFailureCertainty::KnownFailure);
            assert_eq!(
                error.message,
                "xAI stopped the response after reaching a token limit"
            );
        }

        let error = parse_stream_event(&incomplete("response.incomplete", Some("max_time_limit")))
            .expect_err("time limit");
        assert_eq!(error.kind, ModelErrorKind::Unavailable);
        assert!(error.retryable);
        assert_eq!(error.certainty, ModelFailureCertainty::KnownFailure);

        for reason in [None, Some("secret-provider-detail")] {
            let error = parse_stream_event(&incomplete("response.incomplete", reason))
                .expect_err("unknown reason");
            assert_eq!(error.kind, ModelErrorKind::Protocol);
            assert!(!error.retryable);
            assert_eq!(error.certainty, ModelFailureCertainty::KnownFailure);
            assert_eq!(
                error.message,
                "xAI returned an unsupported incomplete response reason"
            );
            assert!(!error.message.contains("secret-provider-detail"));
        }

        let done = parse_stream_event(&incomplete("response.done", Some("max_time_limit")))
            .expect_err("done incomplete");
        assert_eq!(done.kind, ModelErrorKind::Unavailable);
        assert!(done.retryable);
        assert_eq!(done.certainty, ModelFailureCertainty::KnownFailure);

        for (field, value) in [
            ("object", json!("not-a-response")),
            ("status", json!("completed")),
            ("error", json!({ "message": "contradiction" })),
            ("usage", json!({ "input_tokens": -1 })),
            ("incomplete_details", json!({ "reason": 42 })),
        ] {
            let mut event = json!({
                "type": "response.incomplete",
                "response": {
                    "incomplete_details": { "reason": "max_output_tokens" }
                }
            });
            event["response"][field] = value;
            let error = parse_stream_event(&event.to_string()).expect_err("malformed incomplete");
            assert_eq!(error.kind, ModelErrorKind::Protocol);
            assert_eq!(error.certainty, ModelFailureCertainty::OutcomeUnknown);
        }

        let valid_optional_usage = json!({
            "type": "response.incomplete",
            "response": {
                "object": "response",
                "status": "incomplete",
                "usage": {
                    "input_tokens": 2,
                    "output_tokens": 3,
                    "cost_in_usd_ticks": 4
                },
                "incomplete_details": null,
                "error": null
            }
        });
        let error = parse_stream_event(&valid_optional_usage.to_string())
            .expect_err("valid partial incomplete response");
        assert_eq!(error.kind, ModelErrorKind::Protocol);
        assert_eq!(error.certainty, ModelFailureCertainty::KnownFailure);
    }

    #[test]
    fn failure_events_require_documented_bounded_structures_and_sanitize_copy() {
        let failed = json!({
            "type": "response.failed",
            "response": {
                "object": "response",
                "status": "failed",
                "error": {
                    "code": "provider_code",
                    "message": "provider detail must not cross the adapter"
                },
                "incomplete_details": { "reason": "provider_reason" },
                "usage": {
                    "input_tokens": 1,
                    "output_tokens": 0,
                    "cost_in_usd_ticks": 0
                }
            }
        });
        let error = parse_stream_event(&failed.to_string()).expect_err("known failed response");
        assert_eq!(error.kind, ModelErrorKind::Unavailable);
        assert_eq!(error.certainty, ModelFailureCertainty::KnownFailure);
        assert_eq!(error.message, "xAI could not complete the response");
        assert!(!error.message.contains("provider detail"));

        let partial = parse_stream_event(r#"{"type":"response.failed","response":{}}"#)
            .expect_err("valid partial failed response");
        assert_eq!(partial.certainty, ModelFailureCertainty::KnownFailure);
        let missing_response = parse_stream_event(r#"{"type":"response.failed"}"#)
            .expect_err("missing response object");
        assert_eq!(missing_response.kind, ModelErrorKind::Protocol);
        assert_eq!(
            missing_response.certainty,
            ModelFailureCertainty::OutcomeUnknown
        );

        for (pointer, value) in [
            ("/response/object", json!("other")),
            ("/response/status", json!("completed")),
            ("/response/error/message", Value::Null),
            ("/response/error/code", json!("x".repeat(513))),
            ("/response/incomplete_details/reason", json!(42)),
            ("/response/usage/output_tokens", json!(-1)),
        ] {
            let mut malformed = failed.clone();
            *malformed.pointer_mut(pointer).expect("fixture pointer") = value;
            let error = parse_stream_event(&malformed.to_string()).expect_err("malformed failure");
            assert_eq!(error.kind, ModelErrorKind::Protocol);
            assert_eq!(error.certainty, ModelFailureCertainty::OutcomeUnknown);
        }

        for value in [
            json!({
                "type": "error",
                "message": "top-level provider detail",
                "code": "bad_request",
                "param": null,
                "sequence_number": 2
            }),
            json!({
                "type": "error",
                "status": 400,
                "error": {
                    "type": "invalid_request_error",
                    "code": "bad_request",
                    "message": "nested provider detail",
                    "param": "input"
                }
            }),
        ] {
            let error = parse_stream_event(&value.to_string()).expect_err("valid error event");
            assert_eq!(error.kind, ModelErrorKind::Unavailable);
            assert_eq!(error.certainty, ModelFailureCertainty::KnownFailure);
            assert_eq!(error.message, "xAI could not complete the response");
        }

        for value in [
            json!({ "type": "error" }),
            json!({ "type": "error", "message": 42 }),
            json!({ "type": "error", "message": "ok", "sequence_number": -1 }),
            json!({ "type": "error", "message": "ok", "param": ["input"] }),
            json!({ "type": "error", "error": { "message": null } }),
        ] {
            let error = parse_stream_event(&value.to_string()).expect_err("malformed error event");
            assert_eq!(error.kind, ModelErrorKind::Protocol);
            assert_eq!(error.certainty, ModelFailureCertainty::OutcomeUnknown);
        }
    }

    #[test]
    fn done_dispatches_only_strict_embedded_terminal_statuses() {
        let completed = json!({
            "type": "response.done",
            "response": {
                "object": "response",
                "id": "resp-done",
                "status": "completed",
                "usage": {
                    "input_tokens": 1,
                    "output_tokens": 2,
                    "cost_in_usd_ticks": 3
                },
                "output": []
            }
        });
        assert_eq!(
            parse_stream_event(&completed.to_string()).expect("done completion"),
            vec![
                ConversationEvent::Usage(Usage {
                    input_tokens: 1,
                    output_tokens: 2,
                    cost_in_usd_ticks: 3,
                }),
                ConversationEvent::Completed {
                    continuation: Some("resp-done".into()),
                },
            ]
        );

        for status in [Value::Null, json!("queued"), json!(42)] {
            let mut contradictory = completed.clone();
            contradictory["response"]["status"] = status;
            let error = parse_stream_event(&contradictory.to_string())
                .expect_err("contradictory done event");
            assert_eq!(error.kind, ModelErrorKind::Protocol);
            assert_eq!(error.certainty, ModelFailureCertainty::OutcomeUnknown);
        }

        let mut contradictory = completed;
        contradictory["type"] = json!("response.incomplete");
        let error = parse_stream_event(&contradictory.to_string())
            .expect_err("contradictory incomplete event");
        assert_eq!(error.kind, ModelErrorKind::Protocol);
        assert_eq!(error.certainty, ModelFailureCertainty::OutcomeUnknown);
    }

    #[test]
    fn citations_use_only_the_exact_streaming_annotation_paths() {
        let added = parse_stream_event(
            r#"{"type":"response.output_text.annotation.added","annotation":{"type":"url_citation","url":"https://example.test/added","title":"Added"},"nested":{"type":"url_citation","url":"https://example.test/ignored"}}"#,
        )
        .expect("annotation added");
        assert_eq!(
            added,
            vec![ConversationEvent::Citation(Citation {
                title: Some("Added".into()),
                url: "https://example.test/added".into(),
            })]
        );

        let done = parse_stream_event(
            r#"{"type":"response.output_text.done","annotations":[{"type":"url_citation","url":"https://example.test/z"},{"type":"url_citation","url":"https://example.test/a"},{"type":"url_citation","url":"https://example.test/z"}],"nested":{"annotations":[{"type":"url_citation","url":"https://example.test/ignored"}]}}"#,
        )
        .expect("text done");
        assert_eq!(
            done,
            vec![
                ConversationEvent::Citation(Citation {
                    title: None,
                    url: "https://example.test/z".into(),
                }),
                ConversationEvent::Citation(Citation {
                    title: None,
                    url: "https://example.test/a".into(),
                }),
            ]
        );
    }

    #[test]
    fn citation_bounds_count_unique_valid_urls_and_fail_closed_on_malformed_entries() {
        let mut annotations = (0..256)
            .map(|index| {
                json!({
                    "type": "url_citation",
                    "url": format!("https://example.test/{index}")
                })
            })
            .collect::<Vec<_>>();
        annotations.extend([
            json!({ "type": "url_citation", "url": "https://example.test/0" }),
            json!({ "type": "url_citation", "url": "https://example.test/255" }),
        ]);
        let bounded = json!({
            "type": "response.output_text.done",
            "annotations": annotations
        });
        assert_eq!(
            parse_stream_event(&bounded.to_string())
                .expect("256 unique citations plus repeats")
                .len(),
            256
        );

        let mut over = bounded;
        over["annotations"]
            .as_array_mut()
            .expect("annotations")
            .push(json!({
                "type": "url_citation",
                "url": "https://example.test/overflow"
            }));
        let error = parse_stream_event(&over.to_string()).expect_err("257th unique citation");
        assert_eq!(error.kind, ModelErrorKind::Protocol);
        assert_eq!(error.certainty, ModelFailureCertainty::OutcomeUnknown);

        for annotation in [
            json!({ "type": "url_citation" }),
            json!({ "type": "url_citation", "url": "http://example.test/unsafe" }),
            json!({
                "type": "url_citation",
                "url": "https://example.test/source",
                "title": 42
            }),
            json!({
                "type": "url_citation",
                "url": "https://example.test/source",
                "title": "x".repeat(501)
            }),
            json!({ "url": "https://example.test/source" }),
        ] {
            let event = json!({
                "type": "response.output_text.annotation.added",
                "annotation": annotation
            });
            let error = parse_stream_event(&event.to_string()).expect_err("malformed citation");
            assert_eq!(error.kind, ModelErrorKind::Protocol);
            assert_eq!(error.certainty, ModelFailureCertainty::OutcomeUnknown);
        }

        let unknown = parse_stream_event(
            r#"{"type":"response.output_text.annotation.added","annotation":{"type":"file_citation","file_id":"file-1"}}"#,
        )
        .expect("unknown annotation kind");
        assert!(unknown.is_empty());
    }

    #[test]
    fn malformed_event_types_fail_while_unknown_and_reasoning_events_are_ignored() {
        for data in [
            r"{}",
            r#"{"type":null}"#,
            r#"{"type":42}"#,
            r#"{"type":""}"#,
        ] {
            let error = parse_stream_event(data).expect_err("malformed event type");
            assert_eq!(error.kind, ModelErrorKind::Protocol);
            assert_eq!(error.certainty, ModelFailureCertainty::OutcomeUnknown);
        }
        for data in [
            r#"{"type":"response.future_event","payload":"ignored"}"#,
            r#"{"type":"response.reasoning_text.delta","delta":"private reasoning"}"#,
            r#"{"type":"response.reasoning_summary_text.delta","delta":"private summary"}"#,
        ] {
            assert!(parse_stream_event(data).expect("ignored event").is_empty());
        }
    }

    #[tokio::test]
    async fn lists_models_and_generates_images_from_typed_responses() {
        let models = r#"{"data":[{"id":"grok-test","input_modalities":["text"],"output_modalities":["text"]}]}"#;
        let image = r#"{"data":[{"url":"https://images.example.test/a.jpg","mime_type":"image/jpeg","revised_prompt":"revised"}],"usage":{"cost_in_usd_ticks":42}}"#;
        let base = server(vec![
            Box::leak(http(models, "application/json").into_boxed_str()),
            Box::leak(http(image, "application/json").into_boxed_str()),
        ])
        .await;
        let client = XaiClient::for_test("test-key", &base);
        let listed = client.list_models().await.expect("models");
        assert_eq!(listed[0].id, "grok-test");
        let assets = client
            .generate_images(ImageRequest {
                model: "grok-image-test".into(),
                prompt: "A test image".into(),
                count: 1,
            })
            .await
            .expect("image");
        assert_eq!(assets[0].cost_in_usd_ticks, 42);
    }

    #[tokio::test]
    async fn image_generation_requires_explicit_cost_accounting() {
        let without_usage =
            r#"{"data":[{"url":"https://images.example.test/a.jpg","mime_type":"image/jpeg"}]}"#;
        let without_cost = r#"{"data":[{"url":"https://images.example.test/a.jpg","mime_type":"image/jpeg"}],"usage":{}}"#;
        let base = server(vec![
            Box::leak(http(without_usage, "application/json").into_boxed_str()),
            Box::leak(http(without_cost, "application/json").into_boxed_str()),
        ])
        .await;
        let client = XaiClient::for_test("test-key", &base);
        for prompt in ["missing usage", "missing cost"] {
            let error = client
                .generate_images(ImageRequest {
                    model: "grok-image-test".into(),
                    prompt: prompt.into(),
                    count: 1,
                })
                .await
                .expect_err("missing image cost accounting");
            assert_eq!(error.kind, ModelErrorKind::Protocol);
            assert_eq!(error.certainty, ModelFailureCertainty::OutcomeUnknown);
        }
    }

    #[test]
    fn model_catalog_is_structurally_bounded_and_unambiguous() {
        let model = |id: String| RawModel {
            id,
            aliases: vec!["grok-latest".into()],
            input_modalities: vec!["text".into()],
            output_modalities: vec!["text".into()],
        };
        let valid = validate_model_catalog(vec![model("grok-test".into())]).expect("catalog");
        assert_eq!(valid[0].id, "grok-test");

        let too_many = (0..=MAX_DISCOVERED_MODELS)
            .map(|index| model(format!("grok-{index}")))
            .collect();
        assert_eq!(
            validate_model_catalog(too_many)
                .expect_err("model count")
                .message,
            "xAI returned too many model descriptors"
        );
        assert_eq!(
            validate_model_catalog(vec![model("grok-test".into()), model("grok-test".into())])
                .expect_err("duplicate")
                .message,
            "xAI returned duplicate model identifiers"
        );
        assert_eq!(
            validate_model_catalog(vec![model("x".repeat(MAX_MODEL_ID_BYTES + 1))])
                .expect_err("identifier")
                .message,
            "xAI returned an invalid model identifier"
        );

        let mut aliases = model("grok-aliases".into());
        aliases.aliases = vec!["alias".into(); MAX_MODEL_ALIASES + 1];
        assert_eq!(
            validate_model_catalog(vec![aliases])
                .expect_err("aliases")
                .message,
            "xAI returned too many aliases for a model"
        );
        let mut modalities = model("grok-modalities".into());
        modalities.input_modalities = vec!["text".into(); MAX_MODEL_MODALITIES + 1];
        assert_eq!(
            validate_model_catalog(vec![modalities])
                .expect_err("modalities")
                .message,
            "xAI returned too many modalities for a model"
        );

        assert_eq!(
            validate_model_catalog(vec![
                model("grok-a".into()),
                RawModel {
                    id: "grok-latest".into(),
                    aliases: Vec::new(),
                    input_modalities: vec!["text".into()],
                    output_modalities: vec!["text".into()],
                },
            ])
            .expect_err("alias/canonical collision")
            .message,
            "xAI returned an ambiguous model alias"
        );
        assert_eq!(
            validate_model_catalog(vec![RawModel {
                id: "grok-a".into(),
                aliases: vec!["shared".into(), "shared".into()],
                input_modalities: vec!["text".into()],
                output_modalities: vec!["text".into()],
            }])
            .expect_err("duplicate alias")
            .message,
            "xAI returned an ambiguous model alias"
        );
    }

    #[tokio::test]
    async fn rejects_chunked_json_as_soon_as_the_body_exceeds_the_limit() {
        let body = "x".repeat(MAX_JSON_BYTES + 1);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
             Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:X}\r\n{}\r\n0\r\n\r\n",
            body.len(),
            body,
        );
        let base = server(vec![Box::leak(response.into_boxed_str())]).await;
        let client = XaiClient::for_test("test-key", &base);
        let error = client.list_models().await.expect_err("oversize response");
        assert_eq!(error.kind, ModelErrorKind::Protocol);
        assert_eq!(
            error.message,
            "xAI response exceeded the configured size limit"
        );
    }

    #[tokio::test]
    async fn official_validator_reports_acceptance_and_rejection_without_returning_the_key() {
        let accepted = server(vec![Box::leak(
            http(r#"{"data":[{"id":"grok-test"}]}"#, "application/json").into_boxed_str(),
        )])
        .await;
        let secret = SecretValue::new(b"test-key".to_vec()).expect("secret");
        OfficialXaiApiKeyValidator::for_test(&accepted)
            .validate(&secret)
            .await
            .expect("accepted key");

        let rejected = server(vec![
            "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ])
        .await;
        assert_eq!(
            OfficialXaiApiKeyValidator::for_test(&rejected)
                .validate(&secret)
                .await,
            Err(XaiApiKeyValidationError::Rejected)
        );
    }

    #[tokio::test]
    async fn official_validator_accepts_text_catalog_without_the_compiled_default() {
        let alternative = server(vec![Box::leak(
            http(
                r#"{"data":[{"id":"grok-alternative","input_modalities":["text"],"output_modalities":["text"]}]}"#,
                "application/json",
            )
            .into_boxed_str(),
        )])
        .await;
        let secret = SecretValue::new(b"test-key".to_vec()).expect("secret");
        assert_eq!(
            OfficialXaiApiKeyValidator::for_test(&alternative)
                .validate(&secret)
                .await,
            Ok(XaiApiKeyValidation::CapabilitiesResolved)
        );

        let image_only = server(vec![Box::leak(
            http(
                r#"{"data":[{"id":"grok-image","input_modalities":["text"],"output_modalities":["image"]}]}"#,
                "application/json",
            )
            .into_boxed_str(),
        )])
        .await;
        assert_eq!(
            OfficialXaiApiKeyValidator::for_test(&image_only)
                .validate(&secret)
                .await,
            Ok(XaiApiKeyValidation::CapabilitiesUnresolved)
        );
    }

    #[tokio::test]
    async fn official_validator_bounds_a_stalled_provider_probe() {
        let stalled = stalled_server().await;
        let secret = SecretValue::new(b"test-key".to_vec()).expect("secret");
        let result =
            OfficialXaiApiKeyValidator::for_test_with_timeout(&stalled, Duration::from_millis(25))
                .validate(&secret)
                .await;

        assert_eq!(result, Err(XaiApiKeyValidationError::Unavailable));
    }

    #[tokio::test]
    async fn streams_fragment_safe_response_events() {
        let body = concat!(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp-1\"}}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"object\":\"response\",\"id\":\"resp-1\",\"status\":\"completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"cost_in_usd_ticks\":0},\"output\":[]}}\n\n"
        );
        let base = server(vec![Box::leak(
            http(body, "text/event-stream").into_boxed_str(),
        )])
        .await;
        let client = XaiClient::for_test("test-key", &base);
        let mut events = client
            .stream(ConversationRequest {
                model: "grok-test".into(),
                messages: vec![ConversationMessage {
                    role: ConversationRole::User,
                    content: vec![ContentPart::Text("Hello".into())],
                }],
                continuation: None,
                tools: vec![ServerTool::WebSearch],
                store: false,
            })
            .await
            .expect("stream");
        let mut collected = Vec::new();
        while let Some(event) = events.next().await {
            collected.push(event.expect("event"));
        }
        assert!(collected.contains(&ConversationEvent::TextDelta("Hello".into())));
        assert!(matches!(
            collected.last(),
            Some(ConversationEvent::Completed { .. })
        ));
    }

    #[tokio::test]
    async fn terminal_failures_are_released_only_at_clean_eof_or_done() {
        for terminal in [incomplete_data(), failed_data()] {
            for boundary in ["", "data: [DONE]\n\n"] {
                let results = stream_results(format!("{}{boundary}", sse(&terminal))).await;
                assert_eq!(results.len(), 1);
                let error = results[0].as_ref().expect_err("known terminal failure");
                assert_eq!(error.certainty, ModelFailureCertainty::KnownFailure);
            }
        }
    }

    #[tokio::test]
    async fn data_after_incomplete_or_failed_terminal_is_outcome_unknown() {
        let delta = r#"{"type":"response.output_text.delta","delta":"late"}"#;
        for body in [
            format!("{}{}", sse(&incomplete_data()), sse(delta)),
            format!("{}{}", sse(&incomplete_data()), sse(&completed_data())),
            format!("{}{}", sse(&failed_data()), sse(&completed_data())),
        ] {
            let results = stream_results(body).await;
            assert_eq!(results.len(), 1);
            let error = results[0]
                .as_ref()
                .expect_err("contradictory terminal tail");
            assert_eq!(error.kind, ModelErrorKind::Protocol);
            assert_eq!(error.certainty, ModelFailureCertainty::OutcomeUnknown);
        }
    }

    #[tokio::test]
    async fn completed_terminal_is_buffered_and_any_same_batch_tail_is_outcome_unknown() {
        for boundary in ["", "data: [DONE]\n\n"] {
            let results = stream_results(format!("{}{boundary}", sse(&completed_data()))).await;
            assert_eq!(
                results,
                vec![
                    Ok(ConversationEvent::Usage(Usage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cost_in_usd_ticks: 0,
                    })),
                    Ok(ConversationEvent::Completed {
                        continuation: Some("resp-1".into()),
                    }),
                ]
            );
        }

        for tail in [
            sse(r#"{"type":"response.future_event"}"#),
            sse(r#"{"type":"response.output_text.delta","delta":"late"}"#),
            format!(
                "{}{}",
                sse("[DONE]"),
                sse(r#"{"type":"response.future_event"}"#)
            ),
        ] {
            let results = stream_results(format!("{}{}", sse(&completed_data()), tail)).await;
            assert_eq!(results.len(), 1);
            let error = results[0].as_ref().expect_err("post-completion data");
            assert_eq!(error.kind, ModelErrorKind::Protocol);
            assert_eq!(error.certainty, ModelFailureCertainty::OutcomeUnknown);
        }
    }

    #[tokio::test]
    async fn boundary_without_terminal_and_terminal_with_decoder_residue_are_unknown() {
        for body in [String::new(), sse("[DONE]")] {
            let results = stream_results(body).await;
            assert_eq!(results.len(), 1);
            let error = results[0].as_ref().expect_err("missing terminal");
            assert_eq!(error.kind, ModelErrorKind::Protocol);
            assert_eq!(error.certainty, ModelFailureCertainty::OutcomeUnknown);
        }

        let results = stream_results(format!(
            "{}data: {{\"type\":\"response.future_event\"}}",
            sse(&incomplete_data())
        ))
        .await;
        assert_eq!(results.len(), 1);
        let error = results[0].as_ref().expect_err("decoder residue");
        assert_eq!(error.kind, ModelErrorKind::Protocol);
        assert_eq!(error.certainty, ModelFailureCertainty::OutcomeUnknown);
    }

    #[tokio::test]
    async fn transport_failure_after_terminal_cannot_release_a_known_outcome() {
        let body = sse(&failed_data());
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len() + 16
        );
        let results = stream_results_from_http(response).await;
        assert_eq!(results.len(), 1);
        let error = results[0].as_ref().expect_err("truncated terminal stream");
        assert_eq!(error.kind, ModelErrorKind::Protocol);
        assert_eq!(error.certainty, ModelFailureCertainty::OutcomeUnknown);
    }
}
