use std::error::Error;
use std::fmt;
use std::io::{BufRead, BufReader};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value, json};

use crate::providers::{CachePolicy, ChatApiFormat, ProviderConfig};
use crate::request::{ChatMessage, RequestEnvelope};

/// Build the blocking HTTP agent. Non-2xx statuses come back as regular
/// responses (`http_status_as_error(false)`) so callers can surface the
/// provider's own error body instead of a bare status code.
///
/// Proxying is strictly opt-in via the harness config: `None`/"none" go
/// direct (ambient HTTP_PROXY/HTTPS_PROXY are deliberately ignored so the
/// environment can never silently reroute traffic), `"env"` opts into the
/// environment variables, anything else is a proxy URL.
fn http_agent(timeout: Duration, proxy: Option<&str>) -> ureq::Agent {
    let proxy = match proxy.map(str::trim) {
        None | Some("") | Some("none") => None,
        Some("env") => ureq::Proxy::try_from_env(),
        Some(url) => ureq::Proxy::new(url).ok(),
    };
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .proxy(proxy)
        .build()
        .into()
}

/// Cache and auth headers a provider wants on every chat request.
fn provider_headers(
    provider: &ProviderConfig,
    envelope: &RequestEnvelope,
) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    if let Some(header) = provider.cache_header(&envelope.cache_prefix_key()) {
        headers.push(header);
    }
    if let Some(header) = provider.auth_header() {
        headers.push(header);
    }
    headers
}

/// POST a JSON body; a non-2xx reply becomes [`ChatClientError::Status`]
/// carrying up to 400 chars of the response body.
fn post_json(
    timeout: Duration,
    proxy: Option<&str>,
    url: &str,
    accept: &str,
    headers: &[(String, String)],
    body: &str,
) -> Result<ureq::http::Response<ureq::Body>, ChatClientError> {
    let agent = http_agent(timeout, proxy);
    let mut request = agent
        .post(url)
        .header("Accept", accept)
        .header("Content-Type", "application/json");
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let mut response = request.send(body)?;
    let code = response.status().as_u16();
    if (200..300).contains(&code) {
        return Ok(response);
    }
    let body = response.body_mut().read_to_string().unwrap_or_default();
    let body = body.trim().chars().take(400).collect::<String>();
    Err(ChatClientError::Status {
        code,
        url: url.to_string(),
        body,
    })
}

/// Read a successful response's whole body as UTF-8 text.
fn response_text(response: ureq::http::Response<ureq::Body>) -> Result<String, ChatClientError> {
    Ok(response.into_body().read_to_string()?)
}

#[derive(Debug, Clone)]
pub struct ProviderChatClient {
    timeout: Duration,
    cancel: Option<Arc<AtomicBool>>,
}

impl ProviderChatClient {
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            cancel: None,
        }
    }

    /// Cooperative cancellation: SSE streaming stops between chunks once the
    /// flag flips. Blocking (non-stream) requests cannot be aborted mid-flight;
    /// callers observe the flag after the response instead.
    pub fn with_cancel_flag(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = Some(cancel);
        self
    }

    pub fn send(
        &self,
        provider: &ProviderConfig,
        envelope: &RequestEnvelope,
    ) -> Result<ChatResponse, ChatClientError> {
        match provider.chat_api() {
            ChatApiFormat::OpenAiCompatible => {
                OpenAiCompatibleChatClient::new(self.timeout).send(provider, envelope)
            }
            ChatApiFormat::OpenAiResponses => {
                OpenAiResponsesClient::new(self.timeout).send(provider, envelope)
            }
            ChatApiFormat::OpenAiCodexResponses => {
                OpenAiResponsesClient::new(self.timeout).send_codex(provider, envelope)
            }
            ChatApiFormat::AnthropicMessages => {
                AnthropicMessagesChatClient::new(self.timeout).send(provider, envelope)
            }
        }
    }

    pub fn stream_text<F>(
        &self,
        provider: &ProviderConfig,
        envelope: &RequestEnvelope,
        mut on_delta: F,
    ) -> Result<TokenUsage, ChatClientError>
    where
        F: FnMut(&str),
    {
        match provider.chat_api() {
            ChatApiFormat::OpenAiCompatible => OpenAiCompatibleChatClient::new(self.timeout)
                .stream_text(provider, envelope, on_delta),
            ChatApiFormat::OpenAiResponses => {
                let response = OpenAiResponsesClient::new(self.timeout).send(provider, envelope)?;
                if let Some(content) = response.content.as_deref() {
                    on_delta(content);
                }
                Ok(response.usage)
            }
            ChatApiFormat::OpenAiCodexResponses => {
                let response =
                    OpenAiResponsesClient::new(self.timeout).send_codex(provider, envelope)?;
                if let Some(content) = response.content.as_deref() {
                    on_delta(content);
                }
                Ok(response.usage)
            }
            ChatApiFormat::AnthropicMessages => {
                let response =
                    AnthropicMessagesChatClient::new(self.timeout).send(provider, envelope)?;
                if let Some(content) = response.content.as_deref() {
                    on_delta(content);
                }
                Ok(response.usage)
            }
        }
    }

    /// Stream a full turn (content + reasoning + tool calls). OpenAI-compatible
    /// providers stream natively over SSE; other formats fall back to a single
    /// blocking request whose reasoning and content are replayed as one delta
    /// each, so callers get an identical `ChatResponse` either way.
    pub fn stream_chat<F>(
        &self,
        provider: &ProviderConfig,
        envelope: &RequestEnvelope,
        mut on_delta: F,
    ) -> Result<ChatResponse, ChatClientError>
    where
        F: FnMut(StreamDelta),
    {
        match provider.chat_api() {
            ChatApiFormat::OpenAiCompatible => {
                let mut client = OpenAiCompatibleChatClient::new(self.timeout);
                if let Some(cancel) = &self.cancel {
                    client = client.with_cancel_flag(cancel.clone());
                }
                client.stream_chat(provider, envelope, on_delta)
            }
            _ => {
                let response = self.send(provider, envelope)?;
                if let Some(reasoning) = response.reasoning.as_deref().filter(|t| !t.is_empty()) {
                    on_delta(StreamDelta::Reasoning(reasoning));
                }
                if let Some(content) = response.content.as_deref().filter(|t| !t.is_empty()) {
                    on_delta(StreamDelta::Content(content));
                }
                Ok(response)
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct OpenAiCompatibleChatClient {
    timeout: Duration,
    cancel: Option<Arc<AtomicBool>>,
}

impl OpenAiCompatibleChatClient {
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            cancel: None,
        }
    }

    pub fn with_cancel_flag(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = Some(cancel);
        self
    }

    pub fn send(
        &self,
        provider: &ProviderConfig,
        envelope: &RequestEnvelope,
    ) -> Result<ChatResponse, ChatClientError> {
        let url = format!(
            "{}/chat/completions",
            provider.base_url().trim_end_matches('/')
        );
        let body = openai_chat_body(provider, envelope);
        let response = post_json(
            self.timeout,
            provider.proxy(),
            &url,
            "application/json",
            &provider_headers(provider, envelope),
            &body.to_string(),
        )?;
        let response = response_text(response)?;
        let raw: OpenAiChatCompletionResponse = serde_json::from_str(&response)?;
        raw.into_chat_response()
    }

    pub fn stream_text<F>(
        &self,
        provider: &ProviderConfig,
        envelope: &RequestEnvelope,
        on_delta: F,
    ) -> Result<TokenUsage, ChatClientError>
    where
        F: FnMut(&str),
    {
        let url = format!(
            "{}/chat/completions",
            provider.base_url().trim_end_matches('/')
        );
        let body = openai_stream_body(provider, envelope);
        let response = post_json(
            self.timeout,
            provider.proxy(),
            &url,
            "text/event-stream",
            &provider_headers(provider, envelope),
            &body.to_string(),
        )?;
        read_openai_stream(response.into_body().into_reader(), on_delta)
    }

    /// Stream a full turn (content, reasoning, and tool calls), emitting each
    /// text/reasoning fragment to `on_delta` as it arrives.
    pub fn stream_chat<F>(
        &self,
        provider: &ProviderConfig,
        envelope: &RequestEnvelope,
        on_delta: F,
    ) -> Result<ChatResponse, ChatClientError>
    where
        F: FnMut(StreamDelta),
    {
        let url = format!(
            "{}/chat/completions",
            provider.base_url().trim_end_matches('/')
        );
        let body = openai_stream_body(provider, envelope);
        let response = post_json(
            self.timeout,
            provider.proxy(),
            &url,
            "text/event-stream",
            &provider_headers(provider, envelope),
            &body.to_string(),
        )?;
        read_openai_stream_full(
            response.into_body().into_reader(),
            on_delta,
            self.cancel.as_deref(),
        )
    }
}

#[derive(Debug, Clone)]
pub struct OpenAiResponsesClient {
    timeout: Duration,
}

impl OpenAiResponsesClient {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }

    pub fn send(
        &self,
        provider: &ProviderConfig,
        envelope: &RequestEnvelope,
    ) -> Result<ChatResponse, ChatClientError> {
        self.send_to_endpoint(provider, envelope, "responses")
    }

    pub fn send_codex(
        &self,
        provider: &ProviderConfig,
        envelope: &RequestEnvelope,
    ) -> Result<ChatResponse, ChatClientError> {
        self.send_to_endpoint(provider, envelope, "codex/responses")
    }

    fn send_to_endpoint(
        &self,
        provider: &ProviderConfig,
        envelope: &RequestEnvelope,
        endpoint: &str,
    ) -> Result<ChatResponse, ChatClientError> {
        let url = format!(
            "{}/{}",
            provider.base_url().trim_end_matches('/'),
            endpoint.trim_start_matches('/')
        );
        let body = openai_responses_body(envelope);
        let response = post_json(
            self.timeout,
            provider.proxy(),
            &url,
            "application/json",
            &provider_headers(provider, envelope),
            &body.to_string(),
        )?;
        let response = response_text(response)?;
        let raw: OpenAiResponsesResponse = serde_json::from_str(&response)?;
        Ok(raw.into_chat_response())
    }
}

#[derive(Debug, Clone)]
pub struct AnthropicMessagesChatClient {
    timeout: Duration,
}

impl AnthropicMessagesChatClient {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }

    pub fn send(
        &self,
        provider: &ProviderConfig,
        envelope: &RequestEnvelope,
    ) -> Result<ChatResponse, ChatClientError> {
        let url = format!("{}/messages", provider.base_url().trim_end_matches('/'));
        let body = anthropic_messages_body(provider, envelope);
        let mut headers = vec![("anthropic-version".to_string(), "2023-06-01".to_string())];
        headers.extend(provider_headers(provider, envelope));
        let response = post_json(
            self.timeout,
            provider.proxy(),
            &url,
            "application/json",
            &headers,
            &body.to_string(),
        )?;
        let response = response_text(response)?;
        let raw: AnthropicMessagesResponse = serde_json::from_str(&response)?;
        raw.into_chat_response()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChatResponse {
    pub content: Option<String>,
    /// Chain-of-thought / reasoning text when the provider exposes it
    /// (`reasoning_content` for DeepSeek, `reasoning` items for the Responses
    /// API, `thinking` blocks for Anthropic). `None` for models that don't.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ChatToolCall>,
    pub usage: TokenUsage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache: Option<CacheUsageReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChatToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheUsageReport {
    pub hit_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub miss_tokens: Option<u64>,
    pub cacheable_prompt_tokens: u64,
    pub hit_ratio_percent: u64,
    pub saved_prompt_tokens: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TokenUsage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub prompt_cache_hit_tokens: Option<u64>,
    pub prompt_cache_miss_tokens: Option<u64>,
}

impl TokenUsage {
    pub fn cache_hit_ratio(&self) -> Option<f64> {
        if let (Some(hit), Some(miss)) =
            (self.prompt_cache_hit_tokens, self.prompt_cache_miss_tokens)
        {
            let total = hit + miss;
            return (total > 0).then_some(hit as f64 / total as f64);
        }

        match (self.cached_tokens, self.prompt_tokens) {
            (Some(cached), Some(prompt)) if prompt > 0 => Some(cached as f64 / prompt as f64),
            _ => None,
        }
    }

    pub fn cache_report(&self) -> Option<CacheUsageReport> {
        if let (Some(hit), Some(miss)) =
            (self.prompt_cache_hit_tokens, self.prompt_cache_miss_tokens)
        {
            let cacheable_prompt_tokens = hit + miss;
            if cacheable_prompt_tokens == 0 {
                return None;
            }

            return Some(CacheUsageReport {
                hit_tokens: hit,
                miss_tokens: Some(miss),
                cacheable_prompt_tokens,
                hit_ratio_percent: hit.saturating_mul(100) / cacheable_prompt_tokens,
                saved_prompt_tokens: hit,
            });
        }

        match (self.cached_tokens, self.prompt_tokens) {
            (Some(cached), Some(prompt)) if prompt > 0 => {
                let hit = cached.min(prompt);
                Some(CacheUsageReport {
                    hit_tokens: hit,
                    miss_tokens: Some(prompt - hit),
                    cacheable_prompt_tokens: prompt,
                    hit_ratio_percent: hit.saturating_mul(100) / prompt,
                    saved_prompt_tokens: hit,
                })
            }
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for TokenUsage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawTokenUsage {
            prompt_tokens: Option<u64>,
            completion_tokens: Option<u64>,
            total_tokens: Option<u64>,
            cached_tokens: Option<u64>,
            prompt_cache_hit_tokens: Option<u64>,
            prompt_cache_miss_tokens: Option<u64>,
            prompt_tokens_details: Option<PromptTokensDetails>,
        }

        #[derive(Deserialize)]
        struct PromptTokensDetails {
            cached_tokens: Option<u64>,
        }

        let raw = RawTokenUsage::deserialize(deserializer)?;
        Ok(Self {
            prompt_tokens: raw.prompt_tokens,
            completion_tokens: raw.completion_tokens,
            total_tokens: raw.total_tokens,
            cached_tokens: raw.cached_tokens.or_else(|| {
                raw.prompt_tokens_details
                    .and_then(|details| details.cached_tokens)
            }),
            prompt_cache_hit_tokens: raw.prompt_cache_hit_tokens,
            prompt_cache_miss_tokens: raw.prompt_cache_miss_tokens,
        })
    }
}

#[derive(Debug)]
pub enum ChatClientError {
    Http(Box<ureq::Error>),
    /// Non-2xx reply, with the provider's own error body preserved — a bare
    /// "status code 403" is undiagnosable (moderation? credits? auth?).
    Status {
        code: u16,
        url: String,
        body: String,
    },
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for ChatClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(err) => write!(f, "chat request failed: {err}"),
            Self::Status { code, url, body } => {
                write!(f, "chat request failed: {url}: status {code}")?;
                if !body.is_empty() {
                    write!(f, ": {body}")?;
                }
                Ok(())
            }
            Self::Io(err) => write!(f, "chat response read failed: {err}"),
            Self::Json(err) => write!(f, "invalid chat response: {err}"),
        }
    }
}

impl Error for ChatClientError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Http(err) => Some(err.as_ref()),
            Self::Status { .. } => None,
            Self::Io(err) => Some(err),
            Self::Json(err) => Some(err),
        }
    }
}

impl From<ureq::Error> for ChatClientError {
    fn from(value: ureq::Error) -> Self {
        // Non-2xx never reaches here: the agent is built with
        // `http_status_as_error(false)` and `post_json` converts those to
        // `Status` with the response body attached.
        Self::Http(Box::new(value))
    }
}

impl From<serde_json::Error> for ChatClientError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

#[derive(Debug, Deserialize)]
struct OpenAiChatCompletionResponse {
    choices: Vec<OpenAiChoice>,
    #[serde(default)]
    usage: Option<TokenUsage>,
}

impl OpenAiChatCompletionResponse {
    fn into_chat_response(self) -> Result<ChatResponse, ChatClientError> {
        let mut content = None;
        let mut reasoning = None;
        let mut tool_calls = Vec::new();

        for choice in self.choices {
            if content.is_none() {
                content = choice.message.content;
            }
            // DeepSeek exposes chain-of-thought as `reasoning_content`; some
            // OpenAI-compatible gateways use `reasoning`. Take whichever is set.
            if reasoning.is_none() {
                reasoning = choice
                    .message
                    .reasoning_content
                    .or(choice.message.reasoning)
                    .filter(|text| !text.trim().is_empty());
            }

            for call in choice.message.tool_calls.unwrap_or_default() {
                let arguments = parse_tool_arguments_lossy(&call.function.arguments);
                tool_calls.push(ChatToolCall {
                    id: call.id,
                    name: call.function.name,
                    arguments,
                });
            }
        }

        let usage = self.usage.unwrap_or_default();
        let cache = usage.cache_report();
        Ok(ChatResponse {
            content,
            reasoning,
            tool_calls,
            usage,
            cache,
        })
    }
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
}

#[derive(Debug, Deserialize)]
struct OpenAiMessage {
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAiToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OpenAiToolCall {
    id: String,
    function: OpenAiToolFunction,
}

#[derive(Debug, Deserialize)]
struct OpenAiToolFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct OpenAiResponsesResponse {
    #[serde(default)]
    output: Vec<Value>,
    #[serde(default)]
    usage: Option<OpenAiResponsesUsage>,
}

impl OpenAiResponsesResponse {
    fn into_chat_response(self) -> ChatResponse {
        let mut text_blocks = Vec::new();
        let mut reasoning_blocks = Vec::new();
        let mut tool_calls = Vec::new();

        for item in self.output {
            match item.get("type").and_then(Value::as_str) {
                Some("message") => collect_responses_message_text(&item, &mut text_blocks),
                Some("reasoning") => collect_responses_reasoning(&item, &mut reasoning_blocks),
                Some("function_call") => {
                    let id = item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let name = item
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let arguments = item
                        .get("arguments")
                        .and_then(Value::as_str)
                        .map(parse_tool_arguments_lossy)
                        .unwrap_or_else(|| json!({}));
                    tool_calls.push(ChatToolCall {
                        id,
                        name,
                        arguments,
                    });
                }
                _ => {}
            }
        }

        let usage: TokenUsage = self.usage.map(Into::into).unwrap_or_default();
        let cache = usage.cache_report();
        ChatResponse {
            content: (!text_blocks.is_empty()).then(|| text_blocks.join("")),
            reasoning: (!reasoning_blocks.is_empty()).then(|| reasoning_blocks.join("\n")),
            tool_calls,
            usage,
            cache,
        }
    }
}

#[derive(Debug, Deserialize)]
struct OpenAiResponsesUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    total_tokens: Option<u64>,
    input_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Debug, Deserialize)]
struct PromptTokensDetails {
    cached_tokens: Option<u64>,
}

impl From<OpenAiResponsesUsage> for TokenUsage {
    fn from(value: OpenAiResponsesUsage) -> Self {
        Self {
            prompt_tokens: value.input_tokens,
            completion_tokens: value.output_tokens,
            total_tokens: value.total_tokens.or_else(|| {
                match (value.input_tokens, value.output_tokens) {
                    (Some(input), Some(output)) => Some(input + output),
                    _ => None,
                }
            }),
            cached_tokens: value
                .input_tokens_details
                .and_then(|details| details.cached_tokens),
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct AnthropicMessagesResponse {
    #[serde(default)]
    content: Vec<Value>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

impl AnthropicMessagesResponse {
    fn into_chat_response(self) -> Result<ChatResponse, ChatClientError> {
        let mut text_blocks = Vec::new();
        let mut reasoning_blocks = Vec::new();
        let mut tool_calls = Vec::new();

        for block in self.content {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        text_blocks.push(text.to_string());
                    }
                }
                Some("thinking") => {
                    if let Some(text) = block.get("thinking").and_then(Value::as_str) {
                        reasoning_blocks.push(text.to_string());
                    }
                }
                Some("tool_use") => {
                    let id = block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let arguments = block.get("input").cloned().unwrap_or_else(|| json!({}));
                    tool_calls.push(ChatToolCall {
                        id,
                        name,
                        arguments,
                    });
                }
                _ => {}
            }
        }

        let usage: TokenUsage = self.usage.map(Into::into).unwrap_or_default();
        let cache = usage.cache_report();
        Ok(ChatResponse {
            content: (!text_blocks.is_empty()).then(|| text_blocks.join("")),
            reasoning: (!reasoning_blocks.is_empty()).then(|| reasoning_blocks.join("\n")),
            tool_calls,
            usage,
            cache,
        })
    }
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

impl From<AnthropicUsage> for TokenUsage {
    fn from(value: AnthropicUsage) -> Self {
        Self {
            prompt_tokens: value.input_tokens,
            completion_tokens: value.output_tokens,
            total_tokens: match (value.input_tokens, value.output_tokens) {
                (Some(input), Some(output)) => Some(input + output),
                _ => None,
            },
            cached_tokens: None,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
        }
    }
}

fn openai_chat_body(provider: &ProviderConfig, envelope: &RequestEnvelope) -> Value {
    let mut messages = Vec::new();
    if !envelope.system_prompt().trim().is_empty() {
        let content = match provider.cache_policy() {
            CachePolicy::BodyCacheControl { ttl } => {
                cacheable_openai_text_content(envelope.system_prompt(), ttl.as_deref())
            }
            _ => json!(envelope.system_prompt()),
        };
        messages.push(json!({
            "role": "system",
            "content": content,
        }));
    }
    messages.extend(envelope.messages().iter().map(openai_message_body));

    let tools = envelope
        .tools()
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": api_tool_name(tool.name()),
                    "description": tool.description(),
                    "parameters": tool_parameters(tool),
                },
            })
        })
        .collect::<Vec<_>>();

    json!({
        "model": envelope.model(),
        "messages": messages,
        "tools": tools,
    })
}

fn openai_stream_body(provider: &ProviderConfig, envelope: &RequestEnvelope) -> Value {
    let mut body = openai_chat_body(provider, envelope);
    if let Value::Object(object) = &mut body {
        object.insert("stream".to_string(), json!(true));
        object.insert(
            "stream_options".to_string(),
            json!({ "include_usage": true }),
        );
    }
    body
}

fn openai_responses_body(envelope: &RequestEnvelope) -> Value {
    let mut object = Map::new();
    object.insert("model".to_string(), json!(envelope.model()));
    if !envelope.system_prompt().trim().is_empty() {
        object.insert("instructions".to_string(), json!(envelope.system_prompt()));
    }
    object.insert(
        "input".to_string(),
        Value::Array(
            envelope
                .messages()
                .iter()
                .flat_map(openai_responses_input_items)
                .collect(),
        ),
    );

    if !envelope.tools().is_empty() {
        object.insert(
            "tools".to_string(),
            Value::Array(
                envelope
                    .tools()
                    .iter()
                    .map(|tool| {
                        json!({
                            "type": "function",
                            "name": api_tool_name(tool.name()),
                            "description": tool.description(),
                            "parameters": tool_parameters(tool),
                        })
                    })
                    .collect(),
            ),
        );
    }

    Value::Object(object)
}

fn openai_responses_input_items(message: &ChatMessage) -> Vec<Value> {
    if !message.tool_calls().is_empty() {
        return message
            .tool_calls()
            .iter()
            .map(|call| {
                json!({
                    "type": "function_call",
                    "call_id": call.id(),
                    "name": api_tool_name(call.name()),
                    "arguments": call.arguments().to_string(),
                })
            })
            .collect();
    }

    if let Some(tool_call_id) = message.tool_call_id() {
        return vec![json!({
            "type": "function_call_output",
            "call_id": tool_call_id,
            "output": message.content(),
        })];
    }

    vec![json!({
        "role": message.role(),
        "content": message.content(),
    })]
}

fn cacheable_openai_text_content(text: &str, ttl: Option<&str>) -> Value {
    let mut cache_control = Map::new();
    cache_control.insert("type".to_string(), json!("ephemeral"));
    if let Some(ttl) = ttl {
        cache_control.insert("ttl".to_string(), json!(ttl));
    }

    json!([{
        "type": "text",
        "text": text,
        "cache_control": Value::Object(cache_control),
    }])
}

fn openai_message_body(message: &crate::request::ChatMessage) -> Value {
    let mut object = Map::new();
    object.insert("role".to_string(), json!(message.role()));

    if !message.tool_calls().is_empty() {
        object.insert("content".to_string(), Value::Null);
        object.insert(
            "tool_calls".to_string(),
            Value::Array(
                message
                    .tool_calls()
                    .iter()
                    .map(|call| {
                        json!({
                            "id": call.id(),
                            "type": "function",
                            "function": {
                                "name": call.name(),
                                "arguments": call.arguments().to_string(),
                            }
                        })
                    })
                    .collect(),
            ),
        );
    } else {
        object.insert("content".to_string(), json!(message.content()));
    }

    if let Some(tool_call_id) = message.tool_call_id() {
        object.insert("tool_call_id".to_string(), json!(tool_call_id));
    }

    Value::Object(object)
}

fn anthropic_messages_body(provider: &ProviderConfig, envelope: &RequestEnvelope) -> Value {
    let mut object = Map::new();
    object.insert("model".to_string(), json!(envelope.model()));
    object.insert("max_tokens".to_string(), json!(4096));

    if let CachePolicy::AnthropicAutomatic { ttl } = provider.cache_policy() {
        let mut cache_control = Map::new();
        cache_control.insert("type".to_string(), json!("ephemeral"));
        if let Some(ttl) = ttl {
            cache_control.insert("ttl".to_string(), json!(ttl));
        }
        object.insert("cache_control".to_string(), Value::Object(cache_control));
    }

    if !envelope.system_prompt().trim().is_empty() {
        object.insert("system".to_string(), json!(envelope.system_prompt()));
    }

    object.insert(
        "messages".to_string(),
        Value::Array(
            envelope
                .messages()
                .iter()
                .map(anthropic_message_body)
                .collect(),
        ),
    );

    if !envelope.tools().is_empty() {
        object.insert(
            "tools".to_string(),
            Value::Array(
                envelope
                    .tools()
                    .iter()
                    .map(|tool| {
                        json!({
                            "name": api_tool_name(tool.name()),
                            "description": tool.description(),
                            "input_schema": tool_parameters(tool),
                        })
                    })
                    .collect(),
            ),
        );
    }

    Value::Object(object)
}

fn anthropic_message_body(message: &ChatMessage) -> Value {
    if !message.tool_calls().is_empty() {
        return json!({
            "role": "assistant",
            "content": message.tool_calls().iter().map(|call| {
                json!({
                    "type": "tool_use",
                    "id": call.id(),
                    "name": api_tool_name(call.name()),
                    "input": call.arguments(),
                })
            }).collect::<Vec<_>>(),
        });
    }

    if let Some(tool_call_id) = message.tool_call_id() {
        return json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": tool_call_id,
                "content": message.content(),
            }],
        });
    }

    json!({
        "role": message.role(),
        "content": message.content(),
    })
}

/// The declared argument schema, or the accept-anything stub for specs
/// that never set one (older call sites, tests).
fn tool_parameters(tool: &crate::request::ToolSpec) -> Value {
    tool.parameters().cloned().unwrap_or_else(|| {
        json!({
            "type": "object",
            "additionalProperties": true,
        })
    })
}

fn api_tool_name(name: &str) -> String {
    let name = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .take(64)
        .collect::<String>();

    if name.is_empty() {
        "tool".to_string()
    } else {
        name
    }
}

fn collect_responses_message_text(item: &Value, text_blocks: &mut Vec<String>) {
    let Some(content) = item.get("content") else {
        return;
    };

    if let Some(text) = content.as_str() {
        text_blocks.push(text.to_string());
        return;
    }

    let Some(parts) = content.as_array() else {
        return;
    };

    for part in parts {
        if let Some("output_text" | "text") = part.get("type").and_then(Value::as_str)
            && let Some(text) = part.get("text").and_then(Value::as_str)
        {
            text_blocks.push(text.to_string());
        }
    }
}

/// Pull reasoning text out of a Responses-API `reasoning` output item. The model
/// reports it as a `summary` array of `{type: "summary_text", text}` entries (and
/// occasionally a parallel `content` array); collect text from either.
fn collect_responses_reasoning(item: &Value, reasoning_blocks: &mut Vec<String>) {
    for key in ["summary", "content"] {
        let Some(parts) = item.get(key).and_then(Value::as_array) else {
            continue;
        };
        for part in parts {
            if let Some(text) = part.get("text").and_then(Value::as_str)
                && !text.trim().is_empty()
            {
                reasoning_blocks.push(text.to_string());
            }
        }
    }
}

fn parse_tool_arguments_lossy(raw: &str) -> Value {
    let trimmed = raw.trim();
    let stripped = strip_json_code_fence(trimmed);
    let mut candidates = vec![trimmed.to_string()];

    if stripped != trimmed {
        candidates.push(stripped.to_string());
    }

    for base in candidates.clone() {
        let without_trailing_commas = remove_trailing_commas(&base);
        if without_trailing_commas != base {
            candidates.push(without_trailing_commas.clone());
        }

        if without_trailing_commas.contains('\'') && !without_trailing_commas.contains('"') {
            candidates.push(without_trailing_commas.replace('\'', "\""));
        }
    }

    for candidate in candidates {
        if let Ok(value) = serde_json::from_str::<Value>(&candidate) {
            return if value.is_object() {
                value
            } else {
                json!({ "_raw_arguments": value })
            };
        }
    }

    json!({ "_raw_arguments": raw })
}

fn strip_json_code_fence(input: &str) -> &str {
    let Some(rest) = input.strip_prefix("```") else {
        return input;
    };

    let rest = rest.trim_start();
    let rest = rest.strip_prefix("json").unwrap_or(rest).trim_start();

    if let Some(end) = rest.rfind("```") {
        rest[..end].trim()
    } else {
        input
    }
}

fn remove_trailing_commas(input: &str) -> String {
    let characters = input.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(input.len());

    for (index, character) in characters.iter().enumerate() {
        if *character == ',' {
            let next_non_whitespace = characters
                .iter()
                .skip(index + 1)
                .copied()
                .find(|candidate| !candidate.is_whitespace());
            if matches!(next_non_whitespace, Some('}' | ']')) {
                continue;
            }
        }

        output.push(*character);
    }

    output
}

fn read_openai_stream<R, F>(reader: R, mut on_delta: F) -> Result<TokenUsage, ChatClientError>
where
    R: std::io::Read,
    F: FnMut(&str),
{
    let mut usage = TokenUsage::default();
    let reader = BufReader::new(reader);

    for line in reader.lines() {
        let line = line.map_err(ChatClientError::Io)?;
        let Some(data) = line.trim().strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            break;
        }

        let event: OpenAiStreamChunk = serde_json::from_str(data)?;
        if let Some(chunk_usage) = event.usage {
            usage = chunk_usage;
        }

        for choice in event.choices {
            if let Some(content) = choice.delta.content {
                on_delta(&content);
            }
        }
    }

    Ok(usage)
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamChunk {
    #[serde(default)]
    choices: Vec<OpenAiStreamChoice>,
    #[serde(default)]
    usage: Option<TokenUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamChoice {
    delta: OpenAiStreamDelta,
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamDelta {
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAiStreamToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamToolCall {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<OpenAiStreamToolFunction>,
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamToolFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Default)]
struct StreamToolAcc {
    id: String,
    name: String,
    arguments: String,
}

/// A chunk of streamed output: assistant text or chain-of-thought, delivered as
/// it arrives so the UI can render tokens live.
pub enum StreamDelta<'a> {
    Content(&'a str),
    Reasoning(&'a str),
}

/// Read an OpenAI-compatible SSE stream into a full `ChatResponse` while emitting
/// each content/reasoning fragment to `on_delta`. Tool-call argument fragments are
/// accumulated by index across chunks and parsed once the stream ends.
fn read_openai_stream_full<R, F>(
    reader: R,
    mut on_delta: F,
    cancel: Option<&AtomicBool>,
) -> Result<ChatResponse, ChatClientError>
where
    R: std::io::Read,
    F: FnMut(StreamDelta),
{
    let mut usage = TokenUsage::default();
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut tools: Vec<StreamToolAcc> = Vec::new();
    let reader = BufReader::new(reader);

    for line in reader.lines() {
        // Cooperative cancel between chunks: stop reading and hand back what
        // has accumulated so far; the caller decides how to report the abort.
        if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            break;
        }
        let line = line.map_err(ChatClientError::Io)?;
        let Some(data) = line.trim().strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            break;
        }

        let event: OpenAiStreamChunk = serde_json::from_str(data)?;
        if let Some(chunk_usage) = event.usage {
            usage = chunk_usage;
        }

        for choice in event.choices {
            let delta = choice.delta;
            if let Some(chunk) = delta.content.filter(|text| !text.is_empty()) {
                on_delta(StreamDelta::Content(&chunk));
                content.push_str(&chunk);
            }
            if let Some(chunk) = delta
                .reasoning_content
                .or(delta.reasoning)
                .filter(|text| !text.is_empty())
            {
                on_delta(StreamDelta::Reasoning(&chunk));
                reasoning.push_str(&chunk);
            }
            for call in delta.tool_calls.unwrap_or_default() {
                let slot = call.index;
                while tools.len() <= slot {
                    tools.push(StreamToolAcc::default());
                }
                let acc = &mut tools[slot];
                if let Some(id) = call.id.filter(|id| !id.is_empty()) {
                    acc.id = id;
                }
                if let Some(function) = call.function {
                    if let Some(name) = function.name.filter(|name| !name.is_empty()) {
                        acc.name.push_str(&name);
                    }
                    if let Some(arguments) = function.arguments {
                        acc.arguments.push_str(&arguments);
                    }
                }
            }
        }
    }

    let tool_calls = tools
        .into_iter()
        .filter(|tool| !tool.name.is_empty() || !tool.arguments.is_empty())
        .map(|tool| ChatToolCall {
            id: tool.id,
            name: tool.name,
            arguments: parse_tool_arguments_lossy(&tool.arguments),
        })
        .collect();

    let cache = usage.cache_report();
    Ok(ChatResponse {
        content: (!content.is_empty()).then_some(content),
        reasoning: (!reasoning.is_empty()).then_some(reasoning),
        tool_calls,
        usage,
        cache,
    })
}
