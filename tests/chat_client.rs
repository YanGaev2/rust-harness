use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use harness_cli::chat_client::{
    AnthropicMessagesChatClient, OpenAiCompatibleChatClient, ProviderChatClient, StreamDelta,
};
use harness_cli::prompt::DEFAULT_SYSTEM_PROMPT;
use harness_cli::providers::{AuthScheme, CachePolicy, ChatApiFormat, ProviderConfig};
use harness_cli::request::{CacheMode, ChatMessage, RequestEnvelope, ToolSpec};
use serde_json::Value;

#[test]
fn openai_chat_body_carries_declared_tool_parameter_schemas() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let json: Value = serde_json::from_str(body).unwrap();

        // The declared schema replaces the accept-anything stub.
        let parameters = &json["tools"][0]["function"]["parameters"];
        assert_eq!(parameters["type"], "object");
        assert_eq!(
            parameters["properties"]["timeout"]["description"],
            "Timeout in seconds"
        );
        assert_eq!(parameters["required"][0], "command");

        let response_body = r#"{"choices": [{"message": {"role": "assistant", "content": "ok"}}]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        stream.write_all(response.as_bytes()).unwrap();
    });

    let provider = ProviderConfig::new("deepseek", format!("http://{addr}/v1"), "sk-test");
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "command": {"type": "string", "description": "The command line to run"},
            "timeout": {"type": "integer", "description": "Timeout in seconds"},
        },
        "required": ["command"],
    });
    let envelope = RequestEnvelope::new("deepseek", "deepseek-v4-pro")
        .with_system_prompt(DEFAULT_SYSTEM_PROMPT)
        .with_tools(vec![
            ToolSpec::new("run_shell_command", "Run a command").with_parameters(schema),
        ])
        .with_messages(vec![ChatMessage::user("count files")]);

    let response = OpenAiCompatibleChatClient::new(Duration::from_secs(2))
        .send(&provider, &envelope)
        .unwrap();
    server.join().unwrap();

    assert_eq!(response.content.as_deref(), Some("ok"));
}

#[test]
fn provider_extra_body_fields_are_merged_into_the_request() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let json: Value = serde_json::from_str(body).unwrap();

        // Provider-specific routing fields reach the wire...
        assert_eq!(json["provider"]["only"][0], "wandb");
        assert_eq!(json["provider"]["allow_fallbacks"], false);
        // ...but extra_body must not clobber core request fields.
        assert_eq!(json["model"], "qwen/qwen3.6-35b-a3b");

        let response_body = r#"{"choices": [{"message": {"role": "assistant", "content": "ok"}}]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        stream.write_all(response.as_bytes()).unwrap();
    });

    let provider = ProviderConfig::new("openrouter", format!("http://{addr}/v1"), "sk-test")
        .with_extra_body(serde_json::json!({
            "provider": {"only": ["wandb"], "allow_fallbacks": false},
            "model": "must-not-override",
        }));
    let envelope = RequestEnvelope::new("openrouter", "qwen/qwen3.6-35b-a3b")
        .with_system_prompt(DEFAULT_SYSTEM_PROMPT)
        .with_messages(vec![ChatMessage::user("ping")]);

    let response = OpenAiCompatibleChatClient::new(Duration::from_secs(2))
        .send(&provider, &envelope)
        .unwrap();
    server.join().unwrap();

    assert_eq!(response.content.as_deref(), Some("ok"));
}

#[test]
fn http_error_status_message_includes_the_response_body() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_http_request(&mut stream);
        let response_body = r#"{"error":{"message":"Key limit exceeded: add credits","code":403}}"#;
        let response = format!(
            "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        stream.write_all(response.as_bytes()).unwrap();
    });

    let provider = ProviderConfig::new("openrouter", format!("http://{addr}/v1"), "sk-test");
    let envelope = RequestEnvelope::new("openrouter", "openai/gpt-5.6-luna")
        .with_system_prompt(DEFAULT_SYSTEM_PROMPT)
        .with_messages(vec![ChatMessage::user("ping")]);

    let err = OpenAiCompatibleChatClient::new(Duration::from_secs(2))
        .send(&provider, &envelope)
        .unwrap_err();
    server.join().unwrap();

    let message = err.to_string();
    // A bare "status code 403" is undiagnosable; the provider's own error
    // text must reach the user.
    assert!(message.contains("403"), "message: {message}");
    assert!(
        message.contains("Key limit exceeded: add credits"),
        "message: {message}"
    );
}

#[test]
fn chat_client_posts_cache_aware_request_and_parses_tool_calls() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        let request_lower = request.to_ascii_lowercase();

        assert!(request.starts_with("POST /v1/chat/completions "));
        assert!(request_lower.contains("authorization: bearer sk-test"));
        assert!(request_lower.contains("x-harness-cache-key:"));

        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let json: Value = serde_json::from_str(body).unwrap();
        assert_eq!(json["model"], "deepseek-v4-pro");
        assert_eq!(json["messages"][0]["role"], "system");
        assert_eq!(json["messages"][1]["role"], "user");
        assert_eq!(json["tools"][0]["function"]["name"], "file_write");

        let response_body = r#"{
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {
                            "name": "write_file",
                            "arguments": "{\"file\":\"notes.txt\",\"text\":\"hi\"}"
                        }
                    }]
                }
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15,
                "prompt_cache_hit_tokens": 6,
                "prompt_cache_miss_tokens": 4
            }
        }"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        stream.write_all(response.as_bytes()).unwrap();
    });

    let provider = ProviderConfig::new("deepseek", format!("http://{addr}/v1"), "sk-test");
    let envelope = RequestEnvelope::new("deepseek", "deepseek-v4-pro")
        .with_system_prompt(DEFAULT_SYSTEM_PROMPT)
        .with_cache_mode(CacheMode::ProviderPrefix)
        .with_tools(vec![ToolSpec::new("file.write", "Write a file")])
        .with_messages(vec![ChatMessage::user("write notes.txt")]);
    let client = OpenAiCompatibleChatClient::new(Duration::from_secs(2));

    let response = client.send(&provider, &envelope).unwrap();
    server.join().unwrap();

    assert_eq!(response.usage.total_tokens, Some(15));
    assert_eq!(response.usage.prompt_cache_hit_tokens, Some(6));
    assert_eq!(response.usage.prompt_cache_miss_tokens, Some(4));
    assert_eq!(response.usage.cache_hit_ratio(), Some(0.6));
    let cache = response.cache.as_ref().unwrap();
    assert_eq!(cache.hit_tokens, 6);
    assert_eq!(cache.miss_tokens, Some(4));
    assert_eq!(cache.cacheable_prompt_tokens, 10);
    assert_eq!(cache.hit_ratio_percent, 60);
    assert_eq!(cache.saved_prompt_tokens, 6);
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].id, "call-1");
    assert_eq!(response.tool_calls[0].name, "write_file");
    assert_eq!(response.tool_calls[0].arguments["file"], "notes.txt");
}

#[test]
fn chat_client_parses_openai_cached_tokens_details() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        assert!(request.starts_with("POST /v1/chat/completions "));

        let response_body = r#"{
            "choices": [{"message": {"role": "assistant", "content": "ok"}}],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 7,
                "total_tokens": 107,
                "prompt_tokens_details": {"cached_tokens": 25}
            }
        }"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        stream.write_all(response.as_bytes()).unwrap();
    });

    let provider = ProviderConfig::new("openai", format!("http://{addr}/v1"), "sk-test");
    let envelope = RequestEnvelope::new("openai", "gpt-5")
        .with_system_prompt(DEFAULT_SYSTEM_PROMPT)
        .with_cache_mode(CacheMode::ProviderPrefix)
        .with_messages(vec![ChatMessage::user("hello")]);

    let response = OpenAiCompatibleChatClient::new(Duration::from_secs(2))
        .send(&provider, &envelope)
        .unwrap();
    server.join().unwrap();

    assert_eq!(response.usage.cached_tokens, Some(25));
    assert_eq!(response.usage.cache_hit_ratio(), Some(0.25));
    let cache = response.cache.as_ref().unwrap();
    assert_eq!(cache.hit_tokens, 25);
    assert_eq!(cache.miss_tokens, Some(75));
    assert_eq!(cache.cacheable_prompt_tokens, 100);
    assert_eq!(cache.hit_ratio_percent, 25);
    assert_eq!(cache.saved_prompt_tokens, 25);
}

#[test]
fn chat_client_streams_openai_compatible_content_deltas_and_usage() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        let request_lower = request.to_ascii_lowercase();
        assert!(request.starts_with("POST /v1/chat/completions "));
        assert!(request_lower.contains("authorization: bearer sk-stream"));

        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let json: Value = serde_json::from_str(body).unwrap();
        assert_eq!(json["model"], "gpt-stream");
        assert_eq!(json["stream"], true);
        assert_eq!(json["stream_options"]["include_usage"], true);

        respond_sse(
            &mut stream,
            &[
                r#"{"choices":[{"delta":{"role":"assistant","content":"hel"},"index":0}]}"#,
                r#"{"choices":[{"delta":{"content":"lo"},"index":0}]}"#,
                r#"{"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#,
                "[DONE]",
            ],
        );
    });

    let provider = ProviderConfig::new("openai", format!("http://{addr}/v1"), "sk-stream");
    let envelope = RequestEnvelope::new("openai", "gpt-stream")
        .with_messages(vec![ChatMessage::user("hello")]);
    let mut streamed = String::new();

    let usage = OpenAiCompatibleChatClient::new(Duration::from_secs(2))
        .stream_text(&provider, &envelope, |delta| streamed.push_str(delta))
        .unwrap();
    server.join().unwrap();

    assert_eq!(streamed, "hello");
    assert_eq!(usage.prompt_tokens, Some(3));
    assert_eq!(usage.completion_tokens, Some(2));
    assert_eq!(usage.total_tokens, Some(5));
}

#[test]
fn stream_chat_accumulates_reasoning_content_and_tool_calls() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_http_request(&mut stream);
        respond_sse(
            &mut stream,
            &[
                r#"{"choices":[{"delta":{"reasoning_content":"think "},"index":0}]}"#,
                r#"{"choices":[{"delta":{"reasoning_content":"hard"},"index":0}]}"#,
                r#"{"choices":[{"delta":{"content":"wri"},"index":0}]}"#,
                r#"{"choices":[{"delta":{"content":"ting"},"index":0}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"file_write","arguments":"{\"path\":"}}]},"index":0}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a.txt\"}"}}]},"index":0}]}"#,
                "[DONE]",
            ],
        );
    });

    let provider = ProviderConfig::new("local", format!("http://{addr}/v1"), "sk-key");
    let envelope = RequestEnvelope::new("local", "deepseek-v4-pro")
        .with_messages(vec![ChatMessage::user("write a file")]);

    let mut content = String::new();
    let mut reasoning = String::new();
    let response = OpenAiCompatibleChatClient::new(Duration::from_secs(2))
        .stream_chat(&provider, &envelope, |delta| match delta {
            StreamDelta::Content(chunk) => content.push_str(chunk),
            StreamDelta::Reasoning(chunk) => reasoning.push_str(chunk),
        })
        .unwrap();
    server.join().unwrap();

    // Fragments were delivered live...
    assert_eq!(content, "writing");
    assert_eq!(reasoning, "think hard");
    // ...and the assembled response carries the same text plus the parsed tool call.
    assert_eq!(response.content.as_deref(), Some("writing"));
    assert_eq!(response.reasoning.as_deref(), Some("think hard"));
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].id, "call-1");
    assert_eq!(response.tool_calls[0].name, "file_write");
    assert_eq!(response.tool_calls[0].arguments["path"], "a.txt");
}

#[test]
fn chat_client_repairs_common_malformed_tool_argument_json() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        assert!(request.starts_with("POST /v1/chat/completions "));

        let response_body = r#"{
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-repair",
                        "type": "function",
                        "function": {
                            "name": "write_file",
                            "arguments": "{'file':'notes.txt','text':'hi',}"
                        }
                    }]
                }
            }]
        }"#;
        respond_json(&mut stream, response_body);
    });

    let provider = ProviderConfig::new("openai", format!("http://{addr}/v1"), "sk-test");
    let envelope = RequestEnvelope::new("openai", "gpt-5")
        .with_messages(vec![ChatMessage::user("write notes")]);

    let response = OpenAiCompatibleChatClient::new(Duration::from_secs(2))
        .send(&provider, &envelope)
        .unwrap();
    server.join().unwrap();

    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].arguments["file"], "notes.txt");
    assert_eq!(response.tool_calls[0].arguments["text"], "hi");
}

#[test]
fn chat_client_preserves_raw_tool_arguments_when_json_repair_fails() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        assert!(request.starts_with("POST /v1/chat/completions "));

        let response_body = r#"{
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-raw",
                        "type": "function",
                        "function": {
                            "name": "write_file",
                            "arguments": "file=notes.txt text=hi"
                        }
                    }]
                }
            }]
        }"#;
        respond_json(&mut stream, response_body);
    });

    let provider = ProviderConfig::new("openai", format!("http://{addr}/v1"), "sk-test");
    let envelope = RequestEnvelope::new("openai", "gpt-5")
        .with_messages(vec![ChatMessage::user("write notes")]);

    let response = OpenAiCompatibleChatClient::new(Duration::from_secs(2))
        .send(&provider, &envelope)
        .unwrap();
    server.join().unwrap();

    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(
        response.tool_calls[0].arguments["_raw_arguments"],
        "file=notes.txt text=hi"
    );
}

#[test]
fn chat_client_does_not_send_cache_header_for_automatic_cache_policy() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        let request_lower = request.to_ascii_lowercase();

        assert!(request.starts_with("POST /v1/chat/completions "));
        assert!(!request_lower.contains("x-deepseek-cache-key:"));
        assert!(!request_lower.contains("x-harness-cache-key:"));

        let response_body = r#"{
            "choices": [{"message": {"role": "assistant", "content": "ok"}}],
            "usage": {
                "prompt_tokens": 12,
                "completion_tokens": 1,
                "total_tokens": 13,
                "prompt_cache_hit_tokens": 10,
                "prompt_cache_miss_tokens": 2
            }
        }"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        stream.write_all(response.as_bytes()).unwrap();
    });

    let provider = ProviderConfig::new("deepseek", format!("http://{addr}/v1"), "sk-test")
        .with_cache_policy(CachePolicy::Automatic {
            hit_tokens_field: "prompt_cache_hit_tokens".to_string(),
            miss_tokens_field: "prompt_cache_miss_tokens".to_string(),
        });
    let envelope = RequestEnvelope::new("deepseek", "deepseek-v4-pro")
        .with_system_prompt(DEFAULT_SYSTEM_PROMPT)
        .with_cache_mode(CacheMode::ProviderPrefix)
        .with_messages(vec![ChatMessage::user("hello")]);

    let response = OpenAiCompatibleChatClient::new(Duration::from_secs(2))
        .send(&provider, &envelope)
        .unwrap();
    server.join().unwrap();

    assert_eq!(response.usage.prompt_cache_hit_tokens, Some(10));
    assert_eq!(response.usage.prompt_cache_miss_tokens, Some(2));
}

#[test]
fn chat_client_uses_provider_auth_and_cache_headers() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        let request_lower = request.to_ascii_lowercase();

        assert!(request.starts_with("POST /v1/chat/completions "));
        assert!(request_lower.contains("x-api-key: sk-gateway"));
        assert!(request_lower.contains("x-provider-cache-key:"));
        assert!(!request_lower.contains("authorization: bearer sk-gateway"));

        let response_body = r#"{
            "choices": [{"message": {"role": "assistant", "content": "ok"}}]
        }"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        stream.write_all(response.as_bytes()).unwrap();
    });

    let provider = ProviderConfig::new("gateway", format!("http://{addr}/v1"), "sk-gateway")
        .with_auth_scheme(AuthScheme::Header {
            name: "x-api-key".to_string(),
        })
        .with_cache_policy(CachePolicy::Header {
            name: "x-provider-cache-key".to_string(),
        });
    let envelope = RequestEnvelope::new("gateway", "gateway-model")
        .with_system_prompt(DEFAULT_SYSTEM_PROMPT)
        .with_cache_mode(CacheMode::ProviderPrefix)
        .with_messages(vec![ChatMessage::user("hello")]);

    let response = OpenAiCompatibleChatClient::new(Duration::from_secs(2))
        .send(&provider, &envelope)
        .unwrap();
    server.join().unwrap();

    assert_eq!(response.content.as_deref(), Some("ok"));
}

#[test]
fn chat_client_parses_openai_reasoning_content() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_http_request(&mut stream);
        respond_json(
            &mut stream,
            r#"{
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "reasoning_content": "First I will inspect, then write.",
                        "content": "done"
                    }
                }]
            }"#,
        );
    });

    let provider = ProviderConfig::new("local", format!("http://{addr}/v1"), "sk-key");
    let envelope = RequestEnvelope::new("local", "deepseek-reasoner")
        .with_system_prompt(DEFAULT_SYSTEM_PROMPT)
        .with_messages(vec![ChatMessage::user("hello")]);

    let response = OpenAiCompatibleChatClient::new(Duration::from_secs(2))
        .send(&provider, &envelope)
        .unwrap();
    server.join().unwrap();

    assert_eq!(response.content.as_deref(), Some("done"));
    assert_eq!(
        response.reasoning.as_deref(),
        Some("First I will inspect, then write.")
    );
}

#[test]
fn openai_compatible_chat_adds_body_cache_control_without_cache_header() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        let request_lower = request.to_ascii_lowercase();

        assert!(request.starts_with("POST /v1/chat/completions "));
        assert!(!request_lower.contains("x-harness-cache-key:"));
        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let json: Value = serde_json::from_str(body).unwrap();
        assert_eq!(json["messages"][0]["role"], "system");
        assert_eq!(json["messages"][0]["content"][0]["type"], "text");
        assert_eq!(
            json["messages"][0]["content"][0]["text"],
            "cache this prefix"
        );
        assert_eq!(
            json["messages"][0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert_eq!(
            json["messages"][0]["content"][0]["cache_control"]["ttl"],
            "1h"
        );

        let response_body = r#"{
            "choices": [{"message": {"role": "assistant", "content": "ok"}}]
        }"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        stream.write_all(response.as_bytes()).unwrap();
    });

    let provider = ProviderConfig::new("cache-body", format!("http://{addr}/v1"), "sk-cache")
        .with_cache_policy(CachePolicy::BodyCacheControl {
            ttl: Some("1h".to_string()),
        });
    let envelope = RequestEnvelope::new("cache-body", "cache-model")
        .with_system_prompt("cache this prefix")
        .with_cache_mode(CacheMode::ProviderPrefix)
        .with_messages(vec![ChatMessage::user("hello")]);

    let response = OpenAiCompatibleChatClient::new(Duration::from_secs(2))
        .send(&provider, &envelope)
        .unwrap();
    server.join().unwrap();

    assert_eq!(response.content.as_deref(), Some("ok"));
}

#[test]
fn anthropic_messages_client_posts_native_payload_and_parses_tool_use() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        let request_lower = request.to_ascii_lowercase();

        assert!(request.starts_with("POST /v1/messages "));
        assert!(request_lower.contains("x-api-key: sk-anthropic"));
        assert!(request_lower.contains("anthropic-version: 2023-06-01"));
        assert!(!request_lower.contains("authorization: bearer sk-anthropic"));

        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let json: Value = serde_json::from_str(body).unwrap();
        assert_eq!(json["model"], "claude-sonnet-4.5");
        assert_eq!(json["system"], DEFAULT_SYSTEM_PROMPT);
        assert_eq!(json["messages"][0]["role"], "user");
        assert_eq!(json["messages"][0]["content"], "write notes.txt");
        assert_eq!(json["tools"][0]["name"], "file_write");
        assert_eq!(json["tools"][0]["input_schema"]["type"], "object");

        let response_body = r#"{
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "text", "text": "I will write it."},
                {
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "file_write",
                    "input": {"file": "notes.txt", "text": "hi"}
                }
            ],
            "usage": {"input_tokens": 14, "output_tokens": 6}
        }"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        stream.write_all(response.as_bytes()).unwrap();
    });

    let provider = ProviderConfig::new("claude", format!("http://{addr}/v1"), "sk-anthropic")
        .with_auth_scheme(AuthScheme::Header {
            name: "x-api-key".to_string(),
        });
    let envelope = RequestEnvelope::new("claude", "claude-sonnet-4.5")
        .with_system_prompt(DEFAULT_SYSTEM_PROMPT)
        .with_tools(vec![ToolSpec::new("file.write", "Write a file")])
        .with_messages(vec![ChatMessage::user("write notes.txt")]);

    let response = AnthropicMessagesChatClient::new(Duration::from_secs(2))
        .send(&provider, &envelope)
        .unwrap();
    server.join().unwrap();

    assert_eq!(response.content.as_deref(), Some("I will write it."));
    assert_eq!(response.usage.prompt_tokens, Some(14));
    assert_eq!(response.usage.completion_tokens, Some(6));
    assert_eq!(response.usage.total_tokens, Some(20));
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].id, "toolu_1");
    assert_eq!(response.tool_calls[0].name, "file_write");
    assert_eq!(response.tool_calls[0].arguments["file"], "notes.txt");
}

#[test]
fn anthropic_messages_client_adds_top_level_cache_control_for_prompt_caching() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        let request_lower = request.to_ascii_lowercase();

        assert!(request.starts_with("POST /v1/messages "));
        assert!(request_lower.contains("x-api-key: sk-anthropic"));
        assert!(!request_lower.contains("x-harness-cache-key:"));

        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let json: Value = serde_json::from_str(body).unwrap();
        assert_eq!(json["cache_control"]["type"], "ephemeral");
        assert!(json["cache_control"].get("ttl").is_none());
        respond_json(&mut stream, r#"{"content":[{"type":"text","text":"ok"}]}"#);
    });

    let provider = ProviderConfig::new("claude", format!("http://{addr}/v1"), "sk-anthropic")
        .with_auth_scheme(AuthScheme::Header {
            name: "x-api-key".to_string(),
        })
        .with_cache_policy(CachePolicy::AnthropicAutomatic { ttl: None });
    let envelope = RequestEnvelope::new("claude", "claude-sonnet-4.5")
        .with_system_prompt(DEFAULT_SYSTEM_PROMPT)
        .with_messages(vec![ChatMessage::user("hello")]);

    let response = AnthropicMessagesChatClient::new(Duration::from_secs(2))
        .send(&provider, &envelope)
        .unwrap();
    server.join().unwrap();

    assert_eq!(response.content.as_deref(), Some("ok"));
}

#[test]
fn provider_chat_client_routes_anthropic_format_from_provider_metadata() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        assert!(request.starts_with("POST /v1/messages "));
        respond_json(&mut stream, r#"{"content":[{"type":"text","text":"ok"}]}"#);
    });

    let provider = ProviderConfig::new("claude", format!("http://{addr}/v1"), "sk-anthropic")
        .with_auth_scheme(AuthScheme::Header {
            name: "x-api-key".to_string(),
        })
        .with_chat_api(ChatApiFormat::AnthropicMessages);
    let envelope = RequestEnvelope::new("claude", "claude-sonnet-4.5")
        .with_messages(vec![ChatMessage::user("hello")]);

    let response = ProviderChatClient::new(Duration::from_secs(2))
        .send(&provider, &envelope)
        .unwrap();
    server.join().unwrap();

    assert_eq!(response.content.as_deref(), Some("ok"));
}

#[test]
fn provider_chat_client_routes_openai_responses_format_and_parses_output_items() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        let request_lower = request.to_ascii_lowercase();

        assert!(request.starts_with("POST /v1/responses "));
        assert!(request_lower.contains("authorization: bearer sk-responses"));

        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let json: Value = serde_json::from_str(body).unwrap();
        assert_eq!(json["model"], "gpt-5.5");
        assert_eq!(json["instructions"], DEFAULT_SYSTEM_PROMPT);
        assert_eq!(json["input"][0]["role"], "user");
        assert_eq!(json["input"][0]["content"], "write notes.txt");
        assert_eq!(json["tools"][0]["type"], "function");
        assert_eq!(json["tools"][0]["name"], "file_write");
        assert_eq!(json["tools"][0]["parameters"]["type"], "object");

        respond_json(
            &mut stream,
            r#"{
                "id": "resp_1",
                "output": [
                    {
                        "type": "message",
                        "role": "assistant",
                        "content": [
                            {"type": "output_text", "text": "I will write it."}
                        ]
                    },
                    {
                        "type": "function_call",
                        "id": "fc_1",
                        "call_id": "call_1",
                        "name": "write_file",
                        "arguments": "{\"file\":\"notes.txt\",\"text\":\"hi\"}"
                    }
                ],
                "usage": {"input_tokens": 11, "output_tokens": 4, "total_tokens": 15}
            }"#,
        );
    });

    let provider = ProviderConfig::new("openai", format!("http://{addr}/v1"), "sk-responses")
        .with_chat_api(ChatApiFormat::OpenAiResponses);
    let envelope = RequestEnvelope::new("openai", "gpt-5.5")
        .with_system_prompt(DEFAULT_SYSTEM_PROMPT)
        .with_tools(vec![ToolSpec::new("file.write", "Write a file")])
        .with_messages(vec![ChatMessage::user("write notes.txt")]);

    let response = ProviderChatClient::new(Duration::from_secs(2))
        .send(&provider, &envelope)
        .unwrap();
    server.join().unwrap();

    assert_eq!(response.content.as_deref(), Some("I will write it."));
    assert_eq!(response.usage.prompt_tokens, Some(11));
    assert_eq!(response.usage.completion_tokens, Some(4));
    assert_eq!(response.usage.total_tokens, Some(15));
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].id, "call_1");
    assert_eq!(response.tool_calls[0].name, "write_file");
    assert_eq!(response.tool_calls[0].arguments["file"], "notes.txt");
    assert_eq!(response.tool_calls[0].arguments["text"], "hi");
}

#[test]
fn provider_chat_client_routes_openai_codex_responses_to_codex_endpoint() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        let request_lower = request.to_ascii_lowercase();

        assert!(request.starts_with("POST /v1/codex/responses "));
        assert!(request_lower.contains("authorization: bearer sk-codex"));

        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let json: Value = serde_json::from_str(body).unwrap();
        assert_eq!(json["model"], "gpt-5-codex");
        assert_eq!(json["instructions"], DEFAULT_SYSTEM_PROMPT);
        assert_eq!(json["input"][0]["role"], "user");
        assert_eq!(json["input"][0]["content"], "inspect workspace");
        assert_eq!(json["tools"][0]["name"], "file_read");

        respond_json(
            &mut stream,
            r#"{
                "id": "resp_codex",
                "output": [
                    {
                        "type": "message",
                        "role": "assistant",
                        "content": [
                            {"type": "output_text", "text": "I will inspect it."}
                        ]
                    }
                ],
                "usage": {"input_tokens": 13, "output_tokens": 5, "total_tokens": 18}
            }"#,
        );
    });

    let provider = ProviderConfig::new("codex", format!("http://{addr}/v1"), "sk-codex")
        .with_chat_api(ChatApiFormat::OpenAiCodexResponses);
    let envelope = RequestEnvelope::new("codex", "gpt-5-codex")
        .with_system_prompt(DEFAULT_SYSTEM_PROMPT)
        .with_tools(vec![ToolSpec::new("file.read", "Read a file")])
        .with_messages(vec![ChatMessage::user("inspect workspace")]);

    let response = ProviderChatClient::new(Duration::from_secs(2))
        .send(&provider, &envelope)
        .unwrap();
    server.join().unwrap();

    assert_eq!(response.content.as_deref(), Some("I will inspect it."));
    assert_eq!(response.usage.prompt_tokens, Some(13));
    assert_eq!(response.usage.completion_tokens, Some(5));
    assert_eq!(response.usage.total_tokens, Some(18));
}

fn read_http_request(stream: &mut TcpStream) -> String {
    let mut buffer = Vec::new();
    let mut chunk = [0; 1024];

    loop {
        let read = stream.read(&mut chunk).unwrap();
        assert!(read > 0, "connection closed before full request arrived");
        buffer.extend_from_slice(&chunk[..read]);

        if let Some(header_end) = find_header_end(&buffer) {
            let headers = String::from_utf8_lossy(&buffer[..header_end]).to_ascii_lowercase();
            let content_len = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if buffer.len() >= header_end + 4 + content_len {
                break;
            }
        }
    }

    String::from_utf8(buffer).unwrap()
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn respond_json(stream: &mut TcpStream, body: &str) {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).unwrap();
}

fn respond_sse(stream: &mut TcpStream, events: &[&str]) {
    let mut body = String::new();
    for event in events {
        body.push_str("data: ");
        body.push_str(event);
        body.push_str("\n\n");
    }

    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).unwrap();
}
