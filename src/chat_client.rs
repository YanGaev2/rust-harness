use std::error::Error;
use std::fmt;
use std::io::{BufRead, BufReader};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value, json};

use crate::http::{http_agent, streaming_http_agent};
use crate::providers::{CachePolicy, ChatApiFormat, ProviderConfig};
use crate::request::{ChatMessage, RequestEnvelope};

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
    send_json(http_agent(timeout, proxy), url, accept, headers, body)
}

/// `post_json` for SSE endpoints: `timeout` bounds the connection and the
/// response-header wait, but never the stream body — see `streaming_http_agent`.
fn post_json_streaming(
    timeout: Duration,
    proxy: Option<&str>,
    url: &str,
    accept: &str,
    headers: &[(String, String)],
    body: &str,
) -> Result<ureq::http::Response<ureq::Body>, ChatClientError> {
    send_json(
        streaming_http_agent(timeout, proxy),
        url,
        accept,
        headers,
        body,
    )
}

fn send_json(
    agent: ureq::Agent,
    url: &str,
    accept: &str,
    headers: &[(String, String)],
    body: &str,
) -> Result<ureq::http::Response<ureq::Body>, ChatClientError> {
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
    let retry_after_seconds = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok());
    let body = response.body_mut().read_to_string().unwrap_or_default();
    let body = body.trim().chars().take(400).collect::<String>();
    Err(ChatClientError::Status {
        code,
        url: url.to_string(),
        body,
        retry_after_seconds,
    })
}

/// Read a successful response's whole body as UTF-8 text.
fn response_text(response: ureq::http::Response<ureq::Body>) -> Result<String, ChatClientError> {
    Ok(response.into_body().read_to_string()?)
}

/// One raw wire event: the exact request body as sent, the full unparsed body
/// of a blocking response, or a single SSE `data:` payload line of a stream
/// (including the final `[DONE]` marker). Bodies only — headers, and therefore
/// auth keys, never reach the tap.
#[derive(Debug, Clone, Copy)]
pub enum WireEvent<'a> {
    Request { url: &'a str, body: &'a str },
    Response { url: &'a str, body: &'a str },
    SseData { url: &'a str, data: &'a str },
}

/// Shared sink for [`WireEvent`]s — the raw-traffic insurance log behind
/// parsed traces. Must tolerate calls from a worker thread (the TUI runs the
/// agent off-thread).
#[derive(Clone)]
pub struct WireTap(Arc<dyn Fn(WireEvent<'_>) + Send + Sync>);

impl WireTap {
    pub fn new(callback: impl Fn(WireEvent<'_>) + Send + Sync + 'static) -> Self {
        Self(Arc::new(callback))
    }

    fn emit(&self, event: WireEvent<'_>) {
        (self.0)(event);
    }
}

impl fmt::Debug for WireTap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WireTap")
    }
}

#[derive(Debug, Clone)]
pub struct ProviderChatClient {
    timeout: Duration,
    cancel: Option<Arc<AtomicBool>>,
    wire_tap: Option<WireTap>,
}

impl ProviderChatClient {
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            cancel: None,
            wire_tap: None,
        }
    }

    /// Cooperative cancellation: SSE streaming stops between chunks once the
    /// flag flips. Blocking (non-stream) requests cannot be aborted mid-flight;
    /// callers observe the flag after the response instead.
    pub fn with_cancel_flag(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Mirror every request/response body (and raw SSE line) into `tap`.
    pub fn with_wire_tap(mut self, tap: WireTap) -> Self {
        self.wire_tap = Some(tap);
        self
    }

    fn compat_client(&self) -> OpenAiCompatibleChatClient {
        let mut client = OpenAiCompatibleChatClient::new(self.timeout);
        if let Some(tap) = &self.wire_tap {
            client = client.with_wire_tap(tap.clone());
        }
        client
    }

    fn responses_client(&self) -> OpenAiResponsesClient {
        let mut client = OpenAiResponsesClient::new(self.timeout);
        if let Some(tap) = &self.wire_tap {
            client = client.with_wire_tap(tap.clone());
        }
        client
    }

    fn anthropic_client(&self) -> AnthropicMessagesChatClient {
        let mut client = AnthropicMessagesChatClient::new(self.timeout);
        if let Some(tap) = &self.wire_tap {
            client = client.with_wire_tap(tap.clone());
        }
        client
    }

    pub fn send(
        &self,
        provider: &ProviderConfig,
        envelope: &RequestEnvelope,
    ) -> Result<ChatResponse, ChatClientError> {
        match provider.chat_api() {
            ChatApiFormat::OpenAiCompatible => self.compat_client().send(provider, envelope),
            ChatApiFormat::OpenAiResponses => self.responses_client().send(provider, envelope),
            ChatApiFormat::OpenAiCodexResponses => {
                self.responses_client().send_codex(provider, envelope)
            }
            ChatApiFormat::AnthropicMessages => self.anthropic_client().send(provider, envelope),
        }
    }

    /// Content-only streaming (`chat stream`): the full stream_chat pipeline
    /// with reasoning and tool-call deltas filtered out, so in-band errors,
    /// finish_reason, and EOF detection behave identically on both paths.
    pub fn stream_text<F>(
        &self,
        provider: &ProviderConfig,
        envelope: &RequestEnvelope,
        mut on_delta: F,
    ) -> Result<TokenUsage, ChatClientError>
    where
        F: FnMut(&str),
    {
        let response = self.stream_chat(provider, envelope, |delta| {
            if let StreamDelta::Content(chunk) = delta {
                on_delta(chunk);
            }
        })?;
        Ok(response.usage)
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
                let mut client = self.compat_client();
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
    wire_tap: Option<WireTap>,
}

impl OpenAiCompatibleChatClient {
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            cancel: None,
            wire_tap: None,
        }
    }

    pub fn with_cancel_flag(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = Some(cancel);
        self
    }

    pub fn with_wire_tap(mut self, tap: WireTap) -> Self {
        self.wire_tap = Some(tap);
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
        let body = openai_chat_body(provider, envelope).to_string();
        let response = exchange(
            self.timeout,
            provider,
            envelope,
            &url,
            &[],
            &body,
            self.wire_tap.as_ref(),
        )?;
        let raw: OpenAiChatCompletionResponse = serde_json::from_str(&response)?;
        raw.into_chat_response()
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
        let body = openai_stream_body(provider, envelope).to_string();
        if let Some(tap) = &self.wire_tap {
            tap.emit(WireEvent::Request {
                url: &url,
                body: &body,
            });
        }
        let response = post_json_streaming(
            self.timeout,
            provider.proxy(),
            &url,
            "text/event-stream",
            &provider_headers(provider, envelope),
            &body,
        )?;
        read_openai_stream_full(
            response.into_body().into_reader(),
            on_delta,
            self.cancel.as_deref(),
            self.wire_tap.as_ref(),
            &url,
        )
    }
}

#[derive(Debug, Clone)]
pub struct OpenAiResponsesClient {
    timeout: Duration,
    wire_tap: Option<WireTap>,
}

impl OpenAiResponsesClient {
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            wire_tap: None,
        }
    }

    pub fn with_wire_tap(mut self, tap: WireTap) -> Self {
        self.wire_tap = Some(tap);
        self
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
        let body = openai_responses_body(envelope).to_string();
        let response = exchange(
            self.timeout,
            provider,
            envelope,
            &url,
            &[],
            &body,
            self.wire_tap.as_ref(),
        )?;
        let raw: OpenAiResponsesResponse = serde_json::from_str(&response)?;
        Ok(raw.into_chat_response())
    }
}

#[derive(Debug, Clone)]
pub struct AnthropicMessagesChatClient {
    timeout: Duration,
    wire_tap: Option<WireTap>,
}

impl AnthropicMessagesChatClient {
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            wire_tap: None,
        }
    }

    pub fn with_wire_tap(mut self, tap: WireTap) -> Self {
        self.wire_tap = Some(tap);
        self
    }

    pub fn send(
        &self,
        provider: &ProviderConfig,
        envelope: &RequestEnvelope,
    ) -> Result<ChatResponse, ChatClientError> {
        let url = format!("{}/messages", provider.base_url().trim_end_matches('/'));
        let body = anthropic_messages_body(provider, envelope).to_string();
        let response = exchange(
            self.timeout,
            provider,
            envelope,
            &url,
            &[("anthropic-version".to_string(), "2023-06-01".to_string())],
            &body,
            self.wire_tap.as_ref(),
        )?;
        let raw: AnthropicMessagesResponse = serde_json::from_str(&response)?;
        raw.into_chat_response()
    }
}

/// The blocking request pipeline every adapter shares: tap the request,
/// POST, read the body, tap the response. `extra_headers` go first so the
/// Anthropic version header keeps its position ahead of auth.
fn exchange(
    timeout: Duration,
    provider: &ProviderConfig,
    envelope: &RequestEnvelope,
    url: &str,
    extra_headers: &[(String, String)],
    body: &str,
    tap: Option<&WireTap>,
) -> Result<String, ChatClientError> {
    if let Some(tap) = tap {
        tap.emit(WireEvent::Request { url, body });
    }
    let mut headers = extra_headers.to_vec();
    headers.extend(provider_headers(provider, envelope));
    let response = post_json(
        timeout,
        provider.proxy(),
        url,
        "application/json",
        &headers,
        body,
    )?;
    let response = response_text(response)?;
    if let Some(tap) = tap {
        tap.emit(WireEvent::Response {
            url,
            body: &response,
        });
    }
    Ok(response)
}

/// Final assembly shared by every adapter: pairing the usage with its cache
/// report here means no call site can forget one half.
fn chat_response(
    content: Option<String>,
    reasoning: Option<String>,
    tool_calls: Vec<ChatToolCall>,
    usage: TokenUsage,
) -> ChatResponse {
    let cache = usage.cache_report();
    ChatResponse {
        content,
        reasoning,
        tool_calls,
        usage,
        cache,
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
        /// `Retry-After` в секундах, если провайдер его прислал числом
        /// (HTTP-date формы игнорируются). Питает классификатор failover:
        /// короткое ожидание — повторить на месте, длинное — переключиться.
        retry_after_seconds: Option<u64>,
    },
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for ChatClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(err) => write!(f, "chat request failed: {err}"),
            Self::Status {
                code,
                url,
                body,
                retry_after_seconds,
            } => {
                write!(f, "chat request failed: {url}: status {code}")?;
                if !body.is_empty() {
                    write!(f, ": {body}")?;
                }
                if let Some(seconds) = retry_after_seconds {
                    write!(f, ", retry after {seconds}s")?;
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
        Ok(chat_response(content, reasoning, tool_calls, usage))
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
        chat_response(
            (!text_blocks.is_empty()).then(|| text_blocks.join("")),
            (!reasoning_blocks.is_empty()).then(|| reasoning_blocks.join("\n")),
            tool_calls,
            usage,
        )
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
        Ok(chat_response(
            (!text_blocks.is_empty()).then(|| text_blocks.join("")),
            (!reasoning_blocks.is_empty()).then(|| reasoning_blocks.join("\n")),
            tool_calls,
            usage,
        ))
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
    messages.extend(
        envelope
            .messages()
            .iter()
            .map(|message| openai_message_body(message, envelope.retain_thinking())),
    );

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

    let mut body = json!({
        "model": envelope.model(),
        "messages": messages,
        "tools": tools,
    });
    // Provider-specific fields (OpenRouter routing, vendor switches) ride
    // along at the top level; core fields win on key collision so a config
    // typo cannot silently replace the model or messages.
    if let (Value::Object(object), Some(Value::Object(extra))) = (&mut body, provider.extra_body())
    {
        for (key, value) in extra {
            object.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    body
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

fn openai_message_body(message: &crate::request::ChatMessage, retain_thinking: bool) -> Value {
    let mut object = Map::new();
    object.insert("role".to_string(), json!(message.role()));

    // `reasoning_content` is the de-facto OpenAI-compatible field for
    // replayed chain-of-thought (DeepSeek / Kimi / MiMo thinking modes
    // require it back on every assistant turn; probe 2026-08-04).
    if retain_thinking
        && message.role() == "assistant"
        && let Some(reasoning) = message.reasoning()
    {
        object.insert("reasoning_content".to_string(), json!(reasoning));
    }

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
                .map(|message| anthropic_message_body(message, envelope.retain_thinking()))
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

fn anthropic_message_body(message: &ChatMessage, retain_thinking: bool) -> Value {
    // Unsigned thinking block synthesised from the stored reasoning, first in
    // the content array (the Kimi/DeepSeek/MiMo anthropic-endpoint contract;
    // real Anthropic thinking-with-tools needs signed blocks we do not store).
    let retained_thinking_block = || {
        retain_thinking
            .then(|| message.reasoning())
            .flatten()
            .map(|reasoning| json!({"type": "thinking", "thinking": reasoning}))
    };

    if !message.tool_calls().is_empty() {
        let mut blocks = Vec::new();
        blocks.extend(retained_thinking_block());
        blocks.extend(message.tool_calls().iter().map(|call| {
            json!({
                "type": "tool_use",
                "id": call.id(),
                "name": api_tool_name(call.name()),
                "input": call.arguments(),
            })
        }));
        return json!({
            "role": "assistant",
            "content": blocks,
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

    if message.role() == "assistant"
        && let Some(thinking_block) = retained_thinking_block()
    {
        return json!({
            "role": "assistant",
            "content": [
                thinking_block,
                {"type": "text", "text": message.content()},
            ],
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
    // The shared pipeline (json_repair) parses valid JSON on its first
    // attempt and only then repairs: control chars in strings, trailing
    // commas (string-literal aware), missing closers. The quote swap stays
    // local — it only makes sense for fully single-quoted payloads.
    let mut candidates = vec![trimmed.to_string()];
    if stripped != trimmed {
        candidates.push(stripped.to_string());
    }
    for base in candidates.clone() {
        if base.contains('\'') && !base.contains('"') {
            candidates.push(base.replace('\'', "\""));
        }
    }

    for candidate in candidates {
        if let Some(value) = crate::json_repair::repair_json_text(&candidate) {
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

#[derive(Debug, Deserialize)]
struct OpenAiStreamChunk {
    #[serde(default)]
    choices: Vec<OpenAiStreamChoice>,
    #[serde(default)]
    usage: Option<TokenUsage>,
    /// In-band abort: some backends (baseten) end a stream with
    /// `{"error":{...}}` + [DONE]. Without this field the event used to parse
    /// as an empty chunk and the cut turn looked like a normal answer.
    #[serde(default)]
    error: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamChoice {
    delta: OpenAiStreamDelta,
    /// Marks normal completion; providers may close right after it without a
    /// [DONE] marker. Its absence at EOF marks a truncated stream.
    #[serde(default)]
    finish_reason: Option<String>,
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
    wire_tap: Option<&WireTap>,
    url: &str,
) -> Result<ChatResponse, ChatClientError>
where
    R: std::io::Read,
    F: FnMut(StreamDelta),
{
    let mut usage = TokenUsage::default();
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut tools: Vec<StreamToolAcc> = Vec::new();
    // Set by [DONE] or a finish_reason; EOF without either means the stream
    // was cut mid-answer and must surface as an error, not a shorter answer.
    let mut completed = false;
    let reader = BufReader::new(reader);

    for line in reader.lines() {
        // Cooperative cancel between chunks: stop reading and hand back what
        // has accumulated so far; the caller decides how to report the abort.
        if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            completed = true;
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
        // Tapped before parsing (and before the [DONE] break) so the raw log
        // is a faithful record of the stream even when parsing fails.
        if let Some(tap) = wire_tap {
            tap.emit(WireEvent::SseData { url, data });
        }
        if data == "[DONE]" {
            completed = true;
            break;
        }

        let event: OpenAiStreamChunk = serde_json::from_str(data)?;
        if let Some(error) = event.error {
            return Err(sse_error_event(url, &error));
        }
        if let Some(chunk_usage) = event.usage {
            usage = chunk_usage;
        }

        for choice in event.choices {
            if choice.finish_reason.is_some() {
                completed = true;
            }
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

    if !completed {
        return Err(ChatClientError::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "stream ended without completion (no [DONE] or finish_reason)",
        )));
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

    Ok(chat_response(
        (!content.is_empty()).then_some(content),
        (!reasoning.is_empty()).then_some(reasoning),
        tool_calls,
        usage,
    ))
}

/// Map an in-band SSE `{"error":{...}}` event onto the same shape as an HTTP
/// error reply, so the failover classifier sees one taxonomy: the numeric
/// `code` field when it looks like a status, else 502 (upstream failure).
fn sse_error_event(url: &str, error: &serde_json::Value) -> ChatClientError {
    let code = error
        .get("code")
        .and_then(serde_json::Value::as_u64)
        .and_then(|code| u16::try_from(code).ok())
        .filter(|code| (100..=599).contains(code))
        .unwrap_or(502);
    ChatClientError::Status {
        code,
        url: url.to_string(),
        body: error.to_string(),
        retry_after_seconds: None,
    }
}
