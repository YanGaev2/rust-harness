use std::error::Error;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;

use crate::chat_client::{ChatClientError, ChatToolCall, ProviderChatClient, StreamDelta};
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
    history: Vec<ChatMessage>,
    cancel: Option<Arc<AtomicBool>>,
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
            history: Vec::new(),
            cancel: None,
        }
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

    fn cancelled(mut trace: AgentTrace) -> AgentError {
        trace.events.push(AgentTraceEvent::Error {
            message: "interrupted by user".to_string(),
        });
        AgentError::Cancelled {
            trace: Box::new(trace),
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
        let runtime = ToolRuntime::new(&self.workspace).with_shell_timeout(self.timeout);
        // Built once per run: constant within the session, so the cache prefix
        // over the system prompt stays stable across rounds.
        let system_prompt = agent_system_prompt(&self.workspace);
        let user_message = user_message.into();
        let mut messages = self.history.clone();
        messages.push(ChatMessage::user(user_message.clone()));
        let mut tool_results = Vec::new();
        let mut tool_rounds = 0;
        let mut trace = AgentTrace::new(
            self.provider.name().to_string(),
            self.model.clone(),
            self.workspace.display().to_string(),
            user_message,
        );

        loop {
            if self.is_cancelled() {
                return Err(Self::cancelled(trace));
            }
            let envelope = RequestEnvelope::new(self.provider.name(), &self.model)
                .with_system_prompt(&system_prompt)
                .with_cache_mode(CacheMode::ProviderPrefix)
                .with_tools(ToolRuntime::tool_specs())
                .with_messages(messages.clone());
            // When streaming, thinking and answer fragments are emitted live as
            // they arrive; the flags below stop us re-emitting the full text again.
            let mut reasoning_streamed = false;
            let mut content_streamed = false;
            let response = if self.stream {
                client.stream_chat(&self.provider, &envelope, |delta| match delta {
                    StreamDelta::Reasoning(chunk) if !chunk.is_empty() => {
                        reasoning_streamed = true;
                        on_event(AgentEvent::Thinking(chunk.to_string()));
                    }
                    StreamDelta::Content(chunk) if !chunk.is_empty() => {
                        content_streamed = true;
                        on_event(AgentEvent::FinalContentDelta(chunk.to_string()));
                    }
                    _ => {}
                })?
            } else {
                client.send(&self.provider, &envelope)?
            };

            // Esc during a streamed answer: the client already stopped reading
            // between chunks; drop the partial response instead of acting on it.
            if self.is_cancelled() {
                return Err(Self::cancelled(trace));
            }

            if let Some(reasoning) = response
                .reasoning
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
            {
                if !reasoning_streamed {
                    on_event(AgentEvent::Thinking(reasoning.to_string()));
                }
                trace.events.push(AgentTraceEvent::Thinking {
                    content: reasoning.to_string(),
                });
            }

            if response.tool_calls.is_empty() {
                if let Some(content) = response.content.as_deref()
                    && !content_streamed
                {
                    on_event(AgentEvent::FinalContentDelta(content.to_string()));
                }
                trace.events.push(AgentTraceEvent::FinalContent {
                    content: response.content.clone(),
                });
                if let Some(content) = response.content.as_deref() {
                    messages.push(ChatMessage::assistant(content));
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

            messages.push(ChatMessage::assistant_tool_calls(
                response.tool_calls.iter().map(message_tool_call).collect(),
            ));

            let mut scheduler = ToolScheduler::new(runtime.clone());
            if let Some(max_tool_concurrency) = self.max_tool_concurrency {
                scheduler = scheduler.with_max_concurrency(max_tool_concurrency);
            }
            if let Some(tool_batch_timeout) = self.tool_batch_timeout {
                scheduler = scheduler.with_timeout(tool_batch_timeout);
            }

            let batch_results = scheduler.execute_batch(
                response
                    .tool_calls
                    .into_iter()
                    .map(|call| ToolCall::new(call.id, call.name, call.arguments))
                    .collect(),
            );

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
    pub events: Vec<AgentTraceEvent>,
}

impl AgentTrace {
    fn new(provider: String, model: String, workspace: String, user_message: String) -> Self {
        Self {
            provider,
            model,
            workspace,
            user_message,
            events: Vec::new(),
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
    Chat(ChatClientError),
    Runtime(RuntimeError),
    Json(serde_json::Error),
    MaxToolRoundsExceeded {
        max: usize,
        trace: Box<AgentTrace>,
    },
    /// The user interrupted the run (Esc/Ctrl+C); the partial trace is kept.
    Cancelled {
        trace: Box<AgentTrace>,
    },
}

impl AgentError {
    pub fn trace(&self) -> Option<&AgentTrace> {
        match self {
            Self::MaxToolRoundsExceeded { trace, .. } | Self::Cancelled { trace } => Some(trace),
            _ => None,
        }
    }
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Chat(err) => write!(f, "{err}"),
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
            Self::Chat(err) => Some(err),
            Self::Runtime(err) => Some(err),
            Self::Json(err) => Some(err),
            Self::MaxToolRoundsExceeded { .. } | Self::Cancelled { .. } => None,
        }
    }
}

impl From<ChatClientError> for AgentError {
    fn from(value: ChatClientError) -> Self {
        Self::Chat(value)
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
