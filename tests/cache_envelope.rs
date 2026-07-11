use harness_cli::prompt::DEFAULT_SYSTEM_PROMPT;
use harness_cli::request::{CacheMode, ChatMessage, MessageToolCall, RequestEnvelope, ToolSpec};
use serde_json::json;

#[test]
fn provider_prefix_cache_key_is_stable_across_user_turns() {
    let tools = vec![ToolSpec::new(
        "file.write",
        "Writes text into the workspace with path normalization and rollback metadata.",
    )];

    let first = RequestEnvelope::new("deepseek", "deepseek-v4-pro")
        .with_system_prompt(DEFAULT_SYSTEM_PROMPT)
        .with_cache_mode(CacheMode::ProviderPrefix)
        .with_tools(tools.clone())
        .with_messages(vec![ChatMessage::user("Inspect src/main.rs")]);

    let second = RequestEnvelope::new("deepseek", "deepseek-v4-pro")
        .with_system_prompt(DEFAULT_SYSTEM_PROMPT)
        .with_cache_mode(CacheMode::ProviderPrefix)
        .with_tools(tools)
        .with_messages(vec![ChatMessage::user("Now inspect src/lib.rs")]);

    assert_eq!(first.cache_prefix_key(), second.cache_prefix_key());
    assert_ne!(first.full_request_key(), second.full_request_key());
}

#[test]
fn chat_message_survives_serde_round_trip() {
    // Session resume rebuilds Vec<ChatMessage> from JSONL, so every message
    // variant must deserialize back to an identical value.
    let messages = vec![
        ChatMessage::user("почини тест"),
        ChatMessage::assistant_tool_calls(vec![MessageToolCall::new(
            "call_1",
            "file.read",
            json!({"path": "src/main.rs"}),
        )]),
        ChatMessage::tool_result("call_1", "{\"ok\":true}"),
    ];

    for message in messages {
        let encoded = serde_json::to_string(&message).expect("serialize");
        let decoded: ChatMessage = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(message, decoded);
    }
}
