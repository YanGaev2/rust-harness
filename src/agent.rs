use std::error::Error;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;

use crate::chat_client::{ChatClientError, ChatToolCall, ProviderChatClient, StreamDelta, WireTap};
use crate::failover::{FailureAction, classify};
use crate::guardrails::{GuardrailController, GuardrailDecision};
use crate::prompt::agent_system_prompt;
use crate::providers::ProviderConfig;
use crate::request::{CacheMode, ChatMessage, MessageToolCall, RequestEnvelope};
use crate::runtime::{RuntimeError, ToolBatchResult, ToolCall, ToolRuntime, ToolScheduler};

#[derive(Debug, Clone)]
pub struct AgentRunner {
    provider: ProviderConfig,
    model: String,
    workspace: PathBuf,
    timeout: Duration,
    max_tool_rounds: usize,
    max_tool_concurrency: Option<usize>,
    tool_batch_timeout: Option<Duration>,
    stream: bool,
    retain_thinking: bool,
    history: Vec<ChatMessage>,
    cancel: Option<Arc<AtomicBool>>,
    wire_tap: Option<WireTap>,
    /// Цепочка (провайдер, модель) на случай quota/5xx у текущего бэкенда;
    /// порядок = приоритет. Пустая цепочка = поведение как раньше.
    fallback: Vec<(ProviderConfig, String)>,
}

impl AgentRunner {
    pub fn new(
        provider: ProviderConfig,
        model: impl Into<String>,
        workspace: impl Into<PathBuf>,
    ) -> Self {
        Self {
            provider,
            model: model.into(),
            workspace: workspace.into(),
            timeout: Duration::from_secs(60),
            max_tool_rounds: 4,
            max_tool_concurrency: None,
            tool_batch_timeout: None,
            stream: false,
            retain_thinking: false,
            history: Vec::new(),
            cancel: None,
            wire_tap: None,
            fallback: Vec::new(),
        }
    }

    /// Mirror raw provider traffic (request/response bodies, SSE lines) into
    /// `tap` — feeds the session's wire log so parser bugs cannot lose data.
    pub fn with_wire_tap(mut self, wire_tap: WireTap) -> Self {
        self.wire_tap = Some(wire_tap);
        self
    }

    /// Fallback-цепочка: при классифицированной как «переключаемой» ошибке
    /// (quota, 5xx, overloaded — см. failover.rs) запрос уходит к следующей
    /// паре провайдер/модель. Auth и context-overflow не переключают.
    pub fn with_fallback(mut self, fallback: Vec<(ProviderConfig, String)>) -> Self {
        self.fallback = fallback;
        self
    }

    /// Cooperative cancellation (Esc/Ctrl+C in the UI): the flag is checked
    /// between SSE chunks, after each provider response, and before each new
    /// round. A cancelled run returns `AgentError::Cancelled` carrying its
    /// partial trace, so the interruption is still persisted for analysis.
    pub fn with_cancel_flag(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = Some(cancel);
        self
    }

    fn is_cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::SeqCst))
    }

    fn cancelled(mut trace: AgentTrace, messages: Vec<ChatMessage>) -> AgentError {
        trace.events.push(AgentTraceEvent::Error {
            message: "interrupted by user".to_string(),
        });
        AgentError::Cancelled {
            trace: Box::new(trace),
            messages,
        }
    }

    /// Prior conversation messages replayed before the new user message, so a
    /// resumed or multi-turn chat keeps its context. History only appends to the
    /// tail of `messages`, leaving the cache prefix (system prompt + tools) stable.
    pub fn with_history(mut self, history: Vec<ChatMessage>) -> Self {
        self.history = history;
        self
    }

    /// Stream responses over SSE so thinking and answer tokens arrive live
    /// (OpenAI-compatible providers; others transparently fall back to blocking).
    pub fn with_streaming(mut self, stream: bool) -> Self {
        self.stream = stream;
        self
    }

    /// Replay stored assistant reasoning back to the provider (session-wide).
    /// Off by default: thinking then lives only in traces. DeepSeek V4 / Kimi /
    /// MiMo thinking modes require the echo-back and want this on.
    pub fn with_retain_thinking(mut self, retain_thinking: bool) -> Self {
        self.retain_thinking = retain_thinking;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_max_tool_rounds(mut self, max_tool_rounds: usize) -> Self {
        self.max_tool_rounds = max_tool_rounds;
        self
    }

    pub fn with_max_tool_concurrency(mut self, max_tool_concurrency: usize) -> Self {
        self.max_tool_concurrency = Some(max_tool_concurrency.max(1));
        self
    }

    pub fn with_tool_batch_timeout(mut self, timeout: Duration) -> Self {
        self.tool_batch_timeout = Some(timeout);
        self
    }

    pub fn run(&self, user_message: impl Into<String>) -> Result<AgentRunResult, AgentError> {
        self.run_with_events(user_message, |_| {})
    }

    pub fn run_with_events<F>(
        &self,
        user_message: impl Into<String>,
        mut on_event: F,
    ) -> Result<AgentRunResult, AgentError>
    where
        F: FnMut(AgentEvent),
    {
        let mut client = ProviderChatClient::new(self.timeout);
        if let Some(cancel) = &self.cancel {
            client = client.with_cancel_flag(cancel.clone());
        }
        if let Some(tap) = &self.wire_tap {
            client = client.with_wire_tap(tap.clone());
        }
        // One probe per process; prompt, tool description, and execution all
        // agree on the same interpreter (the model writes whichever dialect
        // it is told about — telling it wrong is worse than not telling).
        let shell = crate::platform::ShellProfile::detected();
        let mut runtime = ToolRuntime::new(&self.workspace)
            .with_shell_timeout(self.timeout)
            .with_shell_profile(shell.cloned())
            // Web tools follow the session provider's egress policy (the
            // resolved proxy already folds in the config-wide default).
            .with_web_proxy(self.provider.proxy().map(str::to_string));
        // Esc/Ctrl+C must reach a shell command that is already running, not
        // only the checks between rounds — the same flag goes to the tools.
        if let Some(cancel) = &self.cancel {
            runtime = runtime.with_cancel_flag(cancel.clone());
        }
        // Built once per run: constant within the session, so the cache prefix
        // over the system prompt stays stable across rounds.
        let system_prompt = agent_system_prompt(&self.workspace, shell);
        let user_message = user_message.into();
        let mut messages = self.history.clone();
        messages.push(ChatMessage::user(user_message.clone()));
        let mut tool_results = Vec::new();
        let mut tool_rounds = 0;
        // Per-run: детект повторяющихся бесплодных вызовов (см. guardrails.rs).
        let mut guardrails = GuardrailController::new();
        // Активный бэкенд: начинается с primary, продвигается по fallback-цепочке
        // на переключаемых ошибках. Индекс не сбрасывается в рамках одного run.
        let mut current_provider = self.provider.clone();
        let mut current_model = self.model.clone();
        let mut fallback_index = 0usize;
        let mut trace = AgentTrace::new(
            self.provider.name().to_string(),
            self.model.clone(),
            self.workspace.display().to_string(),
            user_message,
        );

        loop {
            if self.is_cancelled() {
                return Err(Self::cancelled(trace, messages.clone()));
            }
            // Streamed thinking/answer fragments, accumulated. Non-empty means
            // deltas already reached the user (so the full text must not be
            // re-emitted), and when the stream dies mid-answer the buffers ride
            // out on the error so the turn survives in the conversation.
            let mut partial_reasoning = String::new();
            let mut partial_content = String::new();
            // До двух повторов на месте при коротком Retry-After; счётчик
            // обнуляется при переключении на следующий бэкенд цепочки.
            let mut retries_in_place = 0u32;
            let response = loop {
                // Envelope пересобирается на каждую попытку: после переключения
                // provider/model меняют и cache-prefix, и тело запроса.
                let envelope = RequestEnvelope::new(current_provider.name(), &current_model)
                    .with_system_prompt(&system_prompt)
                    .with_cache_mode(CacheMode::ProviderPrefix)
                    .with_tools(ToolRuntime::tool_specs_with_shell(shell))
                    .with_messages(messages.clone())
                    .with_retain_thinking(self.retain_thinking);
                let attempt = if self.stream {
                    client.stream_chat(&current_provider, &envelope, |delta| match delta {
                        StreamDelta::Reasoning(chunk) if !chunk.is_empty() => {
                            partial_reasoning.push_str(chunk);
                            on_event(AgentEvent::Thinking(chunk.to_string()));
                        }
                        StreamDelta::Content(chunk) if !chunk.is_empty() => {
                            partial_content.push_str(chunk);
                            on_event(AgentEvent::FinalContentDelta(chunk.to_string()));
                        }
                        _ => {}
                    })
                } else {
                    client.send(&current_provider, &envelope)
                };
                let err = match attempt {
                    Ok(response) => break response,
                    Err(err) => err,
                };

                let classified = classify(&err);
                // Once deltas have reached the user, a silent retry or provider
                // switch would duplicate the visible text and re-burn the whole
                // generation (hermes rule: no silent replay after output).
                // Fall through to the error exit, which keeps the partial turn.
                let streamed_already = !partial_reasoning.is_empty() || !partial_content.is_empty();
                let wants_switch = if streamed_already {
                    false
                } else {
                    match classified.action {
                        FailureAction::RetrySameProvider { after_seconds } => {
                            if retries_in_place < 2 {
                                retries_in_place += 1;
                                std::thread::sleep(Duration::from_secs(after_seconds.min(10)));
                                if self.is_cancelled() {
                                    return Err(Self::cancelled(trace, messages.clone()));
                                }
                                continue;
                            }
                            // Ретраи не помогли — трактуем как переключаемую.
                            true
                        }
                        FailureAction::SwitchProvider => true,
                        FailureAction::Fail => false,
                    }
                };
                if wants_switch {
                    // Пропускаем записи, совпадающие с только что упавшим
                    // бэкендом: падать в него же — зациклить отказ.
                    let next = loop {
                        let Some((provider, model)) = self.fallback.get(fallback_index) else {
                            break None;
                        };
                        fallback_index += 1;
                        if provider.name() == current_provider.name() && *model == current_model {
                            continue;
                        }
                        break Some((provider.clone(), model.clone()));
                    };
                    if let Some((next_provider, next_model)) = next {
                        let reason = format!("{:?}", classified.reason);
                        on_event(AgentEvent::ProviderSwitched {
                            from_provider: current_provider.name().to_string(),
                            from_model: current_model.clone(),
                            to_provider: next_provider.name().to_string(),
                            to_model: next_model.clone(),
                            reason: reason.clone(),
                        });
                        trace.events.push(AgentTraceEvent::ProviderSwitched {
                            from: format!("{}/{current_model}", current_provider.name()),
                            to: format!("{}/{next_model}", next_provider.name()),
                            reason,
                        });
                        current_provider = next_provider;
                        current_model = next_model;
                        retries_in_place = 0;
                        continue;
                    }
                }
                // Terminal failure: keep the conversation (and any partial
                // thinking/text) on the error so the turn is not lost — the
                // next user message continues from here instead of a reset.
                record_partial_turn(&mut trace, &partial_reasoning, &partial_content);
                trace.events.push(AgentTraceEvent::Error {
                    message: err.to_string(),
                });
                return Err(AgentError::Chat {
                    source: err,
                    trace: Box::new(trace),
                    messages: with_partial_turn(
                        messages.clone(),
                        &partial_reasoning,
                        &partial_content,
                    ),
                });
            };

            // Esc during a streamed answer: the client already stopped reading
            // between chunks; drop the partial response from this round's flow
            // but keep it in the carried conversation for a later "continue".
            if self.is_cancelled() {
                record_partial_turn(&mut trace, &partial_reasoning, &partial_content);
                return Err(Self::cancelled(
                    trace,
                    with_partial_turn(messages.clone(), &partial_reasoning, &partial_content),
                ));
            }

            trace.record_usage(&response.usage, current_provider.pricing(&current_model));
            on_event(AgentEvent::UsageUpdated(trace.usage));

            // Kept beyond the trace: the reasoning also rides on the assistant
            // history message, so retention can replay it in later requests.
            let reasoning = response
                .reasoning
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string);
            if let Some(reasoning) = reasoning.as_deref() {
                if partial_reasoning.is_empty() {
                    on_event(AgentEvent::Thinking(reasoning.to_string()));
                }
                trace.events.push(AgentTraceEvent::Thinking {
                    content: reasoning.to_string(),
                });
            }

            if response.tool_calls.is_empty() {
                if let Some(content) = response.content.as_deref()
                    && partial_content.is_empty()
                {
                    on_event(AgentEvent::FinalContentDelta(content.to_string()));
                }
                trace.events.push(AgentTraceEvent::FinalContent {
                    content: response.content.clone(),
                });
                if let Some(content) = response.content.as_deref() {
                    let mut message = ChatMessage::assistant(content);
                    if let Some(reasoning) = reasoning {
                        message = message.with_reasoning(reasoning);
                    }
                    messages.push(message);
                }
                return Ok(AgentRunResult {
                    final_content: response.content,
                    tool_results,
                    tool_rounds,
                    trace,
                    messages,
                });
            }

            let round = tool_rounds + 1;
            trace.events.push(AgentTraceEvent::ModelToolCalls {
                round,
                calls: response
                    .tool_calls
                    .iter()
                    .map(AgentTraceToolCall::from)
                    .collect(),
            });

            if tool_rounds >= self.max_tool_rounds {
                let message = format!("maximum tool rounds exceeded: {}", self.max_tool_rounds);
                trace.events.push(AgentTraceEvent::Error {
                    message: message.clone(),
                });
                return Err(AgentError::MaxToolRoundsExceeded {
                    max: self.max_tool_rounds,
                    trace: Box::new(trace),
                    messages: messages.clone(),
                });
            }

            on_event(AgentEvent::ToolRoundStarted {
                round,
                tool_calls: response.tool_calls.len(),
            });
            for call in &response.tool_calls {
                on_event(AgentEvent::ToolCallStarted {
                    round,
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                });
            }

            let mut assistant_turn = ChatMessage::assistant_tool_calls(
                response.tool_calls.iter().map(message_tool_call).collect(),
            );
            if let Some(reasoning) = reasoning {
                assistant_turn = assistant_turn.with_reasoning(reasoning);
            }
            messages.push(assistant_turn);

            let mut scheduler = ToolScheduler::new(runtime.clone());
            if let Some(max_tool_concurrency) = self.max_tool_concurrency {
                scheduler = scheduler.with_max_concurrency(max_tool_concurrency);
            }
            if let Some(tool_batch_timeout) = self.tool_batch_timeout {
                scheduler = scheduler.with_timeout(tool_batch_timeout);
            }

            // id -> (wire-имя, аргументы): нужно, чтобы после исполнения
            // сообщить guardrail'у исход именно того вызова.
            let call_index: std::collections::HashMap<String, (String, serde_json::Value)> =
                response
                    .tool_calls
                    .iter()
                    .map(|call| (call.id.clone(), (call.name.clone(), call.arguments.clone())))
                    .collect();

            let mut to_execute = Vec::new();
            let mut synthetic = Vec::new();
            let mut warn_notes: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for call in response.tool_calls.into_iter() {
                match guardrails.check(&call.name, &call.arguments) {
                    GuardrailDecision::Block(text) => synthetic.push(ToolBatchResult {
                        id: call.id.clone(),
                        tool_name: call.name.clone(),
                        ok: false,
                        repaired: false,
                        content: text,
                        metadata: serde_json::json!({"guardrail": "blocked"}),
                        error: None,
                        hint: None,
                    }),
                    GuardrailDecision::Warn(text) => {
                        warn_notes.insert(call.id.clone(), text);
                        to_execute.push(ToolCall::new(call.id, call.name, call.arguments));
                    }
                    GuardrailDecision::Allow => {
                        to_execute.push(ToolCall::new(call.id, call.name, call.arguments));
                    }
                }
            }

            let mut batch_results = scheduler.execute_batch(to_execute);
            for result in &mut batch_results {
                if let Some((name, args)) = call_index.get(&result.id) {
                    guardrails.record(name, args, result.ok, &result.content);
                }
                if let Some(note) = warn_notes.remove(&result.id) {
                    match result.metadata.as_object_mut() {
                        Some(map) => {
                            map.insert("guardrail".into(), serde_json::Value::String(note));
                        }
                        // metadata упавших вызовов бывает Null — не терять метку.
                        None => result.metadata = serde_json::json!({"guardrail": note}),
                    }
                }
            }
            batch_results.extend(synthetic);

            for result in batch_results {
                let tool_call_id = result.id.clone();
                let content = serde_json::to_string(&result)?;
                messages.push(ChatMessage::tool_result(tool_call_id, content));
                on_event(AgentEvent::ToolResult(result.clone()));
                trace.events.push(AgentTraceEvent::ToolResult {
                    round,
                    result: result.clone(),
                });
                tool_results.push(result);
            }

            tool_rounds += 1;
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    /// The model's chain-of-thought for this turn, when the provider exposes it.
    Thinking(String),
    ToolRoundStarted {
        round: usize,
        tool_calls: usize,
    },
    /// A single tool call the model wants to run, emitted before execution so the
    /// UI can show the call (name + arguments) immediately, then update it when
    /// the matching `ToolResult` arrives.
    ToolCallStarted {
        round: usize,
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    ToolResult(ToolBatchResult),
    FinalContentDelta(String),
    /// Cumulative session usage after each model request, so UIs can show
    /// live token/cost counters without re-parsing the trace.
    UsageUpdated(UsageTotals),
    /// The active backend changed mid-run (fallback chain rescued a
    /// quota/5xx failure); UIs surface it so the user knows which model
    /// actually produced the answer.
    ProviderSwitched {
        from_provider: String,
        from_model: String,
        to_provider: String,
        to_model: String,
        reason: String,
    },
}

/// Token totals across all requests of a run, cache-aware: `cached_tokens`
/// is the part of `prompt_tokens` billed at the provider's cached rate.
/// `context_tokens` is not a sum: it is the latest request's prompt plus
/// completion — the conversation the next request carries, i.e. how full
/// the context window is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct UsageTotals {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub cached_tokens: u64,
    pub completion_tokens: u64,
    pub context_tokens: u64,
}

impl UsageTotals {
    /// Fold another total in (e.g. a finished run into a session counter).
    pub fn combined(self, other: Self) -> Self {
        Self {
            requests: self.requests + other.requests,
            prompt_tokens: self.prompt_tokens + other.prompt_tokens,
            cached_tokens: self.cached_tokens + other.cached_tokens,
            completion_tokens: self.completion_tokens + other.completion_tokens,
            context_tokens: if other.requests > 0 {
                other.context_tokens
            } else {
                self.context_tokens
            },
        }
    }

    fn add(&mut self, usage: &crate::chat_client::TokenUsage) {
        self.requests += 1;
        self.prompt_tokens += usage.prompt_tokens.unwrap_or(0);
        self.completion_tokens += usage.completion_tokens.unwrap_or(0);
        self.context_tokens =
            usage.prompt_tokens.unwrap_or(0) + usage.completion_tokens.unwrap_or(0);
        // cache_report unifies both wire schemes (DeepSeek hit/miss fields
        // and OpenAI-style cached_tokens).
        if let Some(report) = usage.cache_report() {
            self.cached_tokens += report.hit_tokens;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentRunResult {
    pub final_content: Option<String>,
    pub tool_results: Vec<ToolBatchResult>,
    pub tool_rounds: usize,
    pub trace: AgentTrace,
    /// The full conversation after this run (history + new user message + this
    /// run's tool exchanges + final answer): feed it back via `with_history`.
    pub messages: Vec<ChatMessage>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentTrace {
    pub provider: String,
    pub model: String,
    pub workspace: String,
    pub user_message: String,
    pub usage: UsageTotals,
    /// Estimate from the built-in dated price list; absent for models we
    /// have no verified pricing for. Never a substitute for the invoice.
    pub estimated_cost_usd: Option<f64>,
    pub pricing_as_of: Option<String>,
    pub events: Vec<AgentTraceEvent>,
}

impl AgentTrace {
    fn new(provider: String, model: String, workspace: String, user_message: String) -> Self {
        Self {
            provider,
            model,
            workspace,
            user_message,
            usage: UsageTotals::default(),
            estimated_cost_usd: None,
            pricing_as_of: None,
            events: Vec::new(),
        }
    }

    fn record_usage(
        &mut self,
        usage: &crate::chat_client::TokenUsage,
        pricing: Option<crate::providers::ModelPricing>,
    ) {
        self.usage.add(usage);
        if let Some(pricing) = pricing {
            self.estimated_cost_usd = Some(pricing.estimate_usd(
                self.usage.prompt_tokens,
                self.usage.cached_tokens,
                self.usage.completion_tokens,
            ));
            self.pricing_as_of = Some(pricing.as_of);
        }
    }

    pub fn tool_errors(&self) -> Vec<AgentToolError> {
        self.events
            .iter()
            .filter_map(|event| {
                let AgentTraceEvent::ToolResult { result, .. } = event else {
                    return None;
                };
                if result.ok {
                    return None;
                }
                Some(AgentToolError {
                    id: result.id.clone(),
                    tool_name: result.tool_name.clone(),
                    error: result.error.clone().unwrap_or_default(),
                    metadata: result.metadata.clone(),
                })
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentTraceEvent {
    Thinking {
        content: String,
    },
    ModelToolCalls {
        round: usize,
        calls: Vec<AgentTraceToolCall>,
    },
    ToolResult {
        round: usize,
        result: ToolBatchResult,
    },
    ProviderSwitched {
        from: String,
        to: String,
        reason: String,
    },
    FinalContent {
        content: Option<String>,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentTraceToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

impl From<&ChatToolCall> for AgentTraceToolCall {
    fn from(value: &ChatToolCall) -> Self {
        Self {
            id: value.id.clone(),
            name: value.name.clone(),
            arguments: value.arguments.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentToolError {
    pub id: String,
    pub tool_name: String,
    pub error: String,
    pub metadata: serde_json::Value,
}

#[derive(Debug)]
pub enum AgentError {
    /// Provider request failed. Carries the conversation up to the failure —
    /// history + the user turn (+ a partial assistant message with any text
    /// and thinking that streamed before the cut) — so the exchange survives
    /// the error instead of resetting to the last complete turn.
    Chat {
        source: ChatClientError,
        trace: Box<AgentTrace>,
        messages: Vec<ChatMessage>,
    },
    Runtime(RuntimeError),
    Json(serde_json::Error),
    MaxToolRoundsExceeded {
        max: usize,
        trace: Box<AgentTrace>,
        messages: Vec<ChatMessage>,
    },
    /// The user interrupted the run (Esc/Ctrl+C); the partial trace is kept.
    Cancelled {
        trace: Box<AgentTrace>,
        messages: Vec<ChatMessage>,
    },
}

impl AgentError {
    pub fn trace(&self) -> Option<&AgentTrace> {
        match self {
            Self::Chat { trace, .. }
            | Self::MaxToolRoundsExceeded { trace, .. }
            | Self::Cancelled { trace, .. } => Some(trace),
            _ => None,
        }
    }

    /// The conversation as of the failure, when the run got far enough to
    /// have one. `None` for internal errors (Runtime/Json) where the message
    /// list may hold a dangling tool-call turn.
    pub fn messages(&self) -> Option<&[ChatMessage]> {
        match self {
            Self::Chat { messages, .. }
            | Self::MaxToolRoundsExceeded { messages, .. }
            | Self::Cancelled { messages, .. } => Some(messages),
            Self::Runtime(_) | Self::Json(_) => None,
        }
    }
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Chat { source, .. } => write!(f, "{source}"),
            Self::Runtime(err) => write!(f, "{err}"),
            Self::Json(err) => write!(f, "agent message serialization failed: {err}"),
            Self::MaxToolRoundsExceeded { max, .. } => {
                write!(f, "maximum tool rounds exceeded: {max}")
            }
            Self::Cancelled { .. } => write!(f, "interrupted by user"),
        }
    }
}

impl Error for AgentError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Chat { source, .. } => Some(source),
            Self::Runtime(err) => Some(err),
            Self::Json(err) => Some(err),
            Self::MaxToolRoundsExceeded { .. } | Self::Cancelled { .. } => None,
        }
    }
}

impl From<RuntimeError> for AgentError {
    fn from(value: RuntimeError) -> Self {
        Self::Runtime(value)
    }
}

impl From<serde_json::Error> for AgentError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

fn message_tool_call(call: &ChatToolCall) -> MessageToolCall {
    MessageToolCall::new(call.id.clone(), call.name.clone(), call.arguments.clone())
}

/// Append the interrupted assistant turn (streamed text and/or thinking) to a
/// conversation snapshot. A no-op when nothing streamed — no empty turns.
fn with_partial_turn(
    mut messages: Vec<ChatMessage>,
    reasoning: &str,
    content: &str,
) -> Vec<ChatMessage> {
    if !reasoning.is_empty() || !content.is_empty() {
        let mut partial = ChatMessage::assistant(content);
        if !reasoning.is_empty() {
            partial = partial.with_reasoning(reasoning);
        }
        messages.push(partial);
    }
    messages
}

/// Mirror the interrupted fragments into the trace, so the cut turn is
/// persisted (session/raw analysis) even though the run failed.
fn record_partial_turn(trace: &mut AgentTrace, reasoning: &str, content: &str) {
    if !reasoning.is_empty() {
        trace.events.push(AgentTraceEvent::Thinking {
            content: reasoning.to_string(),
        });
    }
    if !content.is_empty() {
        trace.events.push(AgentTraceEvent::FinalContent {
            content: Some(content.to_string()),
        });
    }
}
