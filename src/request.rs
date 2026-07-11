use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum CacheMode {
    None,
    ProviderPrefix,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    role: String,
    content: String,
    tool_call_id: Option<String>,
    tool_calls: Vec<MessageToolCall>,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: content.into(),
            tool_call_id: None,
            tool_calls: Vec::new(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: content.into(),
            tool_call_id: None,
            tool_calls: Vec::new(),
        }
    }

    pub fn assistant_tool_calls(tool_calls: Vec<MessageToolCall>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: String::new(),
            tool_call_id: None,
            tool_calls,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".to_string(),
            content: content.into(),
            tool_call_id: Some(tool_call_id.into()),
            tool_calls: Vec::new(),
        }
    }

    pub fn role(&self) -> &str {
        &self.role
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn tool_call_id(&self) -> Option<&str> {
        self.tool_call_id.as_deref()
    }

    pub fn tool_calls(&self) -> &[MessageToolCall] {
        &self.tool_calls
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageToolCall {
    id: String,
    name: String,
    arguments: Value,
}

impl MessageToolCall {
    pub fn new(id: impl Into<String>, name: impl Into<String>, arguments: Value) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn arguments(&self) -> &Value {
        &self.arguments
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolSpec {
    name: String,
    description: String,
}

impl ToolSpec {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn description(&self) -> &str {
        &self.description
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RequestEnvelope {
    provider: String,
    model: String,
    system_prompt: String,
    cache_mode: CacheMode,
    tools: Vec<ToolSpec>,
    messages: Vec<ChatMessage>,
}

impl RequestEnvelope {
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            system_prompt: String::new(),
            cache_mode: CacheMode::None,
            tools: Vec::new(),
            messages: Vec::new(),
        }
    }

    pub fn with_system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.system_prompt = system_prompt.into();
        self
    }

    pub fn with_cache_mode(mut self, cache_mode: CacheMode) -> Self {
        self.cache_mode = cache_mode;
        self
    }

    pub fn with_tools(mut self, tools: Vec<ToolSpec>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_messages(mut self, messages: Vec<ChatMessage>) -> Self {
        self.messages = messages;
        self
    }

    pub fn cache_prefix_key(&self) -> String {
        let prefix = CachePrefix {
            provider: &self.provider,
            model: &self.model,
            system_prompt: &self.system_prompt,
            cache_mode: &self.cache_mode,
            tools: &self.tools,
        };
        hash_serializable(&prefix)
    }

    pub fn full_request_key(&self) -> String {
        hash_serializable(self)
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    pub fn tools(&self) -> &[ToolSpec] {
        &self.tools
    }

    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }
}

#[derive(Debug, Serialize)]
struct CachePrefix<'a> {
    provider: &'a str,
    model: &'a str,
    system_prompt: &'a str,
    cache_mode: &'a CacheMode,
    tools: &'a [ToolSpec],
}

fn hash_serializable(value: &impl Serialize) -> String {
    let bytes = serde_json::to_vec(value).expect("request cache payload must serialize");
    blake3::hash(&bytes).to_hex().to_string()
}
