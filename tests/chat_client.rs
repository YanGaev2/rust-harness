use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use harness_cli::chat_client::{
    AnthropicMessagesChatClient, ChatClientError, OpenAiCompatibleChatClient, ProviderChatClient,
    StreamDelta, WireEvent, WireTap,
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
fn status_error_captures_retry_after_seconds() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_http_request(&mut stream);
        let response_body = r#"{"error":"rate limited"}"#;
        let response = format!(
            "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nRetry-After: 7\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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

    match err {
        ChatClientError::Status {
            code,
            retry_after_seconds,
            ..
        } => {
            assert_eq!(code, 429);
            assert_eq!(retry_after_seconds, Some(7));
        }
        other => panic!("expected Status, got {other:?}"),
    }
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

    let usage = ProviderChatClient::new(Duration::from_secs(2))
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
fn json_repair_keeps_commas_inside_string_literals() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        assert!(request.starts_with("POST /v1/chat/completions "));

        // The arguments carry a RAW newline inside the string value (models
        // emit those unescaped) right after a comma, plus real trailing
        // commas after the array item and the last member. Repair must
        // escape the control char and strip only the real trailing commas —
        // the ",\n}" inside the string stays byte-for-byte.
        let response_body = r#"{
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-strings",
                        "type": "function",
                        "function": {
                            "name": "write_file",
                            "arguments": "{\"note\": \"end,\n}\", \"items\": [\"a\",],}"
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
    assert_eq!(response.tool_calls[0].arguments["note"], "end,\n}");
    assert_eq!(
        response.tool_calls[0].arguments["items"],
        serde_json::json!(["a"])
    );
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

/// History replayed with retention on: prior assistant turns carry their
/// chain-of-thought back to the provider (DeepSeek V4 / Kimi / MiMo thinking
/// modes require the echo; others merely tolerate it).
fn reasoning_history() -> Vec<ChatMessage> {
    vec![
        ChatMessage::user("write notes.txt"),
        ChatMessage::assistant_tool_calls(vec![harness_cli::request::MessageToolCall::new(
            "call-1",
            "file_write",
            serde_json::json!({"file": "notes.txt"}),
        )])
        .with_reasoning("think first"),
        ChatMessage::tool_result("call-1", "{\"ok\":true}"),
        ChatMessage::assistant("done earlier").with_reasoning("think second"),
        ChatMessage::user("next step"),
    ]
}

#[test]
fn openai_body_replays_reasoning_content_when_retained() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let json: Value = serde_json::from_str(body).unwrap();

        // messages[0] is the system prompt; history starts at 1.
        assert_eq!(json["messages"][2]["role"], "assistant");
        assert_eq!(json["messages"][2]["reasoning_content"], "think first");
        assert_eq!(
            json["messages"][2]["tool_calls"][0]["function"]["name"],
            "file_write"
        );
        assert_eq!(json["messages"][4]["reasoning_content"], "think second");
        assert_eq!(json["messages"][4]["content"], "done earlier");
        // User/tool turns never grow the field.
        assert!(json["messages"][1].get("reasoning_content").is_none());
        assert!(json["messages"][3].get("reasoning_content").is_none());

        respond_json(
            &mut stream,
            r#"{"choices": [{"message": {"role": "assistant", "content": "ok"}}]}"#,
        );
    });

    let provider = ProviderConfig::new("deepseek", format!("http://{addr}/v1"), "sk-test");
    let envelope = RequestEnvelope::new("deepseek", "deepseek-v4-pro")
        .with_system_prompt(DEFAULT_SYSTEM_PROMPT)
        .with_messages(reasoning_history())
        .with_retain_thinking(true);

    let response = OpenAiCompatibleChatClient::new(Duration::from_secs(2))
        .send(&provider, &envelope)
        .unwrap();
    server.join().unwrap();
    assert_eq!(response.content.as_deref(), Some("ok"));
}

#[test]
fn openai_body_omits_reasoning_content_by_default() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let json: Value = serde_json::from_str(body).unwrap();

        for message in json["messages"].as_array().unwrap() {
            assert!(
                message.get("reasoning_content").is_none(),
                "default must keep reasoning out of the wire: {message}"
            );
        }

        respond_json(
            &mut stream,
            r#"{"choices": [{"message": {"role": "assistant", "content": "ok"}}]}"#,
        );
    });

    let provider = ProviderConfig::new("deepseek", format!("http://{addr}/v1"), "sk-test");
    let envelope = RequestEnvelope::new("deepseek", "deepseek-v4-pro")
        .with_system_prompt(DEFAULT_SYSTEM_PROMPT)
        .with_messages(reasoning_history());

    OpenAiCompatibleChatClient::new(Duration::from_secs(2))
        .send(&provider, &envelope)
        .unwrap();
    server.join().unwrap();
}

#[test]
fn anthropic_body_replays_thinking_block_first_when_retained() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let json: Value = serde_json::from_str(body).unwrap();

        // Hermes contract for Kimi/DeepSeek anthropic endpoints: an unsigned
        // thinking block synthesised from the stored reasoning goes FIRST.
        let tool_turn = &json["messages"][1]["content"];
        assert_eq!(tool_turn[0]["type"], "thinking");
        assert_eq!(tool_turn[0]["thinking"], "think first");
        assert_eq!(tool_turn[1]["type"], "tool_use");

        let text_turn = &json["messages"][3]["content"];
        assert_eq!(text_turn[0]["type"], "thinking");
        assert_eq!(text_turn[0]["thinking"], "think second");
        assert_eq!(text_turn[1]["type"], "text");
        assert_eq!(text_turn[1]["text"], "done earlier");

        respond_json(
            &mut stream,
            r#"{
                "content": [{"type": "text", "text": "ok"}],
                "usage": {"input_tokens": 1, "output_tokens": 1}
            }"#,
        );
    });

    let provider = ProviderConfig::new("xiaomi", format!("http://{addr}/v1"), "sk-anthropic")
        .with_auth_scheme(AuthScheme::Header {
            name: "x-api-key".to_string(),
        });
    let envelope = RequestEnvelope::new("xiaomi", "mimo-thinking")
        .with_system_prompt(DEFAULT_SYSTEM_PROMPT)
        .with_messages(reasoning_history())
        .with_retain_thinking(true);

    AnthropicMessagesChatClient::new(Duration::from_secs(2))
        .send(&provider, &envelope)
        .unwrap();
    server.join().unwrap();
}

#[test]
fn anthropic_body_omits_thinking_blocks_by_default() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let json: Value = serde_json::from_str(body).unwrap();

        assert_eq!(json["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(json["messages"][3]["content"], "done earlier");
        let messages_text = json["messages"].to_string();
        assert!(
            !messages_text.contains("thinking") && !messages_text.contains("think first"),
            "default must keep reasoning out of the wire: {messages_text}"
        );

        respond_json(
            &mut stream,
            r#"{
                "content": [{"type": "text", "text": "ok"}],
                "usage": {"input_tokens": 1, "output_tokens": 1}
            }"#,
        );
    });

    let provider = ProviderConfig::new("xiaomi", format!("http://{addr}/v1"), "sk-anthropic")
        .with_auth_scheme(AuthScheme::Header {
            name: "x-api-key".to_string(),
        });
    let envelope = RequestEnvelope::new("xiaomi", "mimo-thinking")
        .with_system_prompt(DEFAULT_SYSTEM_PROMPT)
        .with_messages(reasoning_history());

    AnthropicMessagesChatClient::new(Duration::from_secs(2))
        .send(&provider, &envelope)
        .unwrap();
    server.join().unwrap();
}

#[test]
fn responses_body_never_replays_reasoning_even_when_retained() {
    // The Responses API only accepts genuine reasoning items (ids + encrypted
    // content we do not store), so retention is a documented no-op there —
    // and must never leak half-formed fields into the payload.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let json: Value = serde_json::from_str(body).unwrap();
        let input_text = json["input"].to_string();
        assert!(
            !input_text.contains("reasoning") && !input_text.contains("thinking"),
            "responses payload must stay reasoning-free: {input_text}"
        );

        respond_json(
            &mut stream,
            r#"{
                "id": "resp_1",
                "output": [{
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "ok"}]
                }],
                "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
            }"#,
        );
    });

    let provider = ProviderConfig::new("openai", format!("http://{addr}/v1"), "sk-responses")
        .with_chat_api(ChatApiFormat::OpenAiResponses);
    let envelope = RequestEnvelope::new("openai", "gpt-5.5")
        .with_system_prompt(DEFAULT_SYSTEM_PROMPT)
        .with_messages(reasoning_history())
        .with_retain_thinking(true);

    ProviderChatClient::new(Duration::from_secs(2))
        .send(&provider, &envelope)
        .unwrap();
    server.join().unwrap();
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

type WireEvents = Arc<Mutex<Vec<(String, String, String)>>>;

/// A tap that copies every event into a shared (kind, url, payload) list.
fn collecting_wire_tap() -> (WireTap, WireEvents) {
    let events: WireEvents = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let tap = WireTap::new(move |event| {
        let (kind, url, payload) = match event {
            WireEvent::Request { url, body } => ("request", url, body),
            WireEvent::Response { url, body } => ("response", url, body),
            WireEvent::SseData { url, data } => ("sse", url, data),
        };
        sink.lock()
            .unwrap()
            .push((kind.to_string(), url.to_string(), payload.to_string()));
    });
    (tap, events)
}

#[test]
fn wire_tap_captures_raw_request_and_response_bodies() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let response_body = r#"{"choices":[{"message":{"role":"assistant","reasoning_content":"думаю","content":"готово"}}],"usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}}"#;
    let canned = response_body.to_string();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        let body = request.split("\r\n\r\n").nth(1).unwrap().to_string();
        respond_json(&mut stream, &canned);
        body
    });

    let (tap, events) = collecting_wire_tap();
    let provider = ProviderConfig::new("kimi", format!("http://{addr}/v1"), "sk-test");
    let envelope = RequestEnvelope::new("kimi", "kimi-k3")
        .with_messages(vec![ChatMessage::user("сделай раз")]);
    let response = OpenAiCompatibleChatClient::new(Duration::from_secs(2))
        .with_wire_tap(tap)
        .send(&provider, &envelope)
        .unwrap();
    let sent_body = server.join().unwrap();

    assert_eq!(response.reasoning.as_deref(), Some("думаю"));
    let events = events.lock().unwrap();
    assert_eq!(
        events.len(),
        2,
        "expected request + response, got {events:?}"
    );
    // The request event carries the exact bytes that went to the provider.
    assert_eq!(events[0].0, "request");
    assert!(events[0].1.ends_with("/chat/completions"));
    assert_eq!(events[0].2, sent_body);
    // The response event is the unparsed body, verbatim.
    assert_eq!(events[1].0, "response");
    assert_eq!(events[1].2, response_body);
}

#[test]
fn stream_chat_survives_streams_longer_than_the_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_http_request(&mut stream);
        // Dribble the SSE body out slower than the client's timeout: only a
        // whole-request cap would abort this; a live stream must survive.
        let events = [
            r#"{"choices":[{"delta":{"reasoning_content":"thinking hard"},"index":0}]}"#,
            r#"{"choices":[{"delta":{"content":"alive"},"index":0}]}"#,
            "[DONE]",
        ];
        let mut body = String::new();
        for event in events {
            body.push_str("data: ");
            body.push_str(event);
            body.push_str("\n\n");
        }
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(header.as_bytes()).unwrap();
        for chunk in body.as_bytes().chunks(40) {
            stream.write_all(chunk).unwrap();
            stream.flush().unwrap();
            thread::sleep(Duration::from_millis(120));
        }
    });

    let provider = ProviderConfig::new("kimi", format!("http://{addr}/v1"), "sk-test");
    let envelope =
        RequestEnvelope::new("kimi", "kimi-k3").with_messages(vec![ChatMessage::user("ping")]);
    // ~200 bytes over 5+ chunks x 120ms ≈ 600ms of streaming vs a 300ms timeout.
    let response = OpenAiCompatibleChatClient::new(Duration::from_millis(300))
        .stream_chat(&provider, &envelope, |_| {})
        .unwrap();
    server.join().unwrap();

    assert_eq!(response.reasoning.as_deref(), Some("thinking hard"));
    assert_eq!(response.content.as_deref(), Some("alive"));
}

#[test]
fn stream_chat_fails_fast_when_the_server_never_responds() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        // Accept and hold the socket open without ever writing a byte.
        let (stream, _) = listener.accept().unwrap();
        thread::sleep(Duration::from_millis(1500));
        drop(stream);
    });

    let provider = ProviderConfig::new("kimi", format!("http://{addr}/v1"), "sk-test");
    let envelope =
        RequestEnvelope::new("kimi", "kimi-k3").with_messages(vec![ChatMessage::user("ping")]);
    let started = std::time::Instant::now();
    let result = OpenAiCompatibleChatClient::new(Duration::from_millis(200)).stream_chat(
        &provider,
        &envelope,
        |_| {},
    );
    let elapsed = started.elapsed();
    server.join().unwrap();

    assert!(result.is_err(), "silent server must be an error");
    // The response-header wait stays bounded by the timeout even though the
    // stream body itself has no cap.
    assert!(elapsed < Duration::from_millis(1200), "took {elapsed:?}");
}

#[test]
fn stream_chat_surfaces_sse_error_events_as_status_errors() {
    // baseten/Kimi K3, 2026-08-08: the server aborts a stream by sending
    // `{"error":{...}}` + [DONE] and no tool-call deltas. The parser used to
    // deserialize that into an empty chunk (`choices` defaults to []) and
    // return a fake Ok, so the agent treated the cut turn as a normal answer.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_http_request(&mut stream);
        respond_sse(
            &mut stream,
            &[
                r#"{"choices":[{"delta":{"reasoning_content":"planning"},"index":0}]}"#,
                r#"{"error":{"message":"HttpError: HTTP 400: Tool calls cutoff by max_tokens.","type":"internal_server_error","code":500},"tid":"t-1"}"#,
                "[DONE]",
            ],
        );
    });

    let provider = ProviderConfig::new("kimi", format!("http://{addr}/v1"), "sk-test");
    let envelope =
        RequestEnvelope::new("kimi", "kimi-k3").with_messages(vec![ChatMessage::user("ping")]);
    let mut reasoning = String::new();
    let result = OpenAiCompatibleChatClient::new(Duration::from_secs(2)).stream_chat(
        &provider,
        &envelope,
        |delta| {
            if let StreamDelta::Reasoning(chunk) = delta {
                reasoning.push_str(chunk);
            }
        },
    );
    server.join().unwrap();

    // Deltas before the error still reached the caller (partial progress).
    assert_eq!(reasoning, "planning");
    match result {
        Err(ChatClientError::Status { code, body, .. }) => {
            assert_eq!(code, 500, "code comes from the error payload");
            assert!(body.contains("Tool calls cutoff"), "body={body}");
        }
        other => panic!("expected Status error, got {other:?}"),
    }
}

#[test]
fn stream_chat_treats_eof_without_done_as_an_error() {
    // A stream that ends without [DONE] or a finish_reason is a truncated
    // turn (LB idle kill, upstream crash), not a completed answer.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_http_request(&mut stream);
        respond_sse(
            &mut stream,
            &[
                r#"{"choices":[{"delta":{"content":"hal"},"index":0}]}"#,
                r#"{"choices":[{"delta":{"content":"f an answ"},"index":0}]}"#,
            ],
        );
    });

    let provider = ProviderConfig::new("kimi", format!("http://{addr}/v1"), "sk-test");
    let envelope =
        RequestEnvelope::new("kimi", "kimi-k3").with_messages(vec![ChatMessage::user("ping")]);
    let result = OpenAiCompatibleChatClient::new(Duration::from_secs(2)).stream_chat(
        &provider,
        &envelope,
        |_| {},
    );
    server.join().unwrap();

    match result {
        Err(ChatClientError::Io(err)) => {
            assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof, "{err}");
        }
        other => panic!("expected UnexpectedEof, got {other:?}"),
    }
}

#[test]
fn stream_chat_accepts_finish_reason_as_stream_completion() {
    // Some OpenAI-compatible backends close the connection right after the
    // finishing chunk without a [DONE] marker; that is a complete answer.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_http_request(&mut stream);
        respond_sse(
            &mut stream,
            &[
                r#"{"choices":[{"delta":{"content":"done"},"index":0}]}"#,
                r#"{"choices":[{"delta":{},"finish_reason":"stop","index":0}]}"#,
            ],
        );
    });

    let provider = ProviderConfig::new("kimi", format!("http://{addr}/v1"), "sk-test");
    let envelope =
        RequestEnvelope::new("kimi", "kimi-k3").with_messages(vec![ChatMessage::user("ping")]);
    let response = OpenAiCompatibleChatClient::new(Duration::from_secs(2))
        .stream_chat(&provider, &envelope, |_| {})
        .unwrap();
    server.join().unwrap();

    assert_eq!(response.content.as_deref(), Some("done"));
}

#[test]
fn streaming_idle_timeout_has_a_floor_for_silent_tool_call_generation() {
    // baseten buffers whole tool calls: the SSE socket is legitimately silent
    // for minutes while the model writes a large file. The per-read idle cap
    // must not inherit the snappy 60s request timeout.
    use harness_cli::http::{STREAM_IDLE_FLOOR, stream_idle_timeout};

    assert_eq!(
        stream_idle_timeout(Duration::from_secs(60)),
        STREAM_IDLE_FLOOR
    );
    assert_eq!(
        stream_idle_timeout(Duration::from_secs(600)),
        Duration::from_secs(600),
        "an explicitly larger timeout wins over the floor"
    );
}

#[test]
fn wire_tap_captures_raw_sse_data_lines() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let lines = [
        r#"{"choices":[{"delta":{"reasoning_content":"план"},"index":0}]}"#,
        r#"{"choices":[{"delta":{"content":"ок"},"index":0}]}"#,
        "[DONE]",
    ];
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_http_request(&mut stream);
        respond_sse(&mut stream, &lines);
    });

    let (tap, events) = collecting_wire_tap();
    let provider = ProviderConfig::new("kimi", format!("http://{addr}/v1"), "sk-test");
    let envelope = RequestEnvelope::new("kimi", "kimi-k3")
        .with_messages(vec![ChatMessage::user("сделай два")]);
    let response = OpenAiCompatibleChatClient::new(Duration::from_secs(2))
        .with_wire_tap(tap)
        .stream_chat(&provider, &envelope, |_| {})
        .unwrap();
    server.join().unwrap();

    assert_eq!(response.reasoning.as_deref(), Some("план"));
    assert_eq!(response.content.as_deref(), Some("ок"));
    let events = events.lock().unwrap();
    assert_eq!(events[0].0, "request");
    // Every raw data line is captured pre-parse, including the [DONE] marker.
    let sse: Vec<&str> = events
        .iter()
        .filter(|event| event.0 == "sse")
        .map(|event| event.2.as_str())
        .collect();
    assert_eq!(sse, lines);
}

/// A keep-alive server: answers every request on a connection with `reply`
/// until the client hangs up, and counts the connections it accepted.
fn keep_alive_server(
    reply: &'static str,
    content_type: &'static str,
) -> (std::net::SocketAddr, Arc<std::sync::atomic::AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = connections.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            thread::spawn(move || {
                let mut buffer = Vec::new();
                let mut chunk = [0; 4096];
                loop {
                    let Some(end) = find_header_end(&buffer) else {
                        match stream.read(&mut chunk) {
                            Ok(0) | Err(_) => return,
                            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
                        }
                        continue;
                    };
                    let headers = String::from_utf8_lossy(&buffer[..end]).to_ascii_lowercase();
                    let length = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    while buffer.len() < end + 4 + length {
                        match stream.read(&mut chunk) {
                            Ok(0) | Err(_) => return,
                            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
                        }
                    }
                    buffer.drain(..end + 4 + length);
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n{reply}",
                        reply.len()
                    );
                    if stream.write_all(response.as_bytes()).is_err() {
                        return;
                    }
                }
            });
        }
    });
    (addr, connections)
}

#[test]
fn consecutive_requests_reuse_one_connection() {
    let envelope = RequestEnvelope::new("deepseek", "deepseek-v4-pro")
        .with_messages(vec![ChatMessage::user("hi")]);
    let client = ProviderChatClient::new(Duration::from_secs(5));

    // Streaming: the stream stops reading at [DONE]; the rest of the body is
    // drained so the connection goes back to the pool.
    let (addr, connections) = keep_alive_server(
        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        "text/event-stream",
    );
    let provider = ProviderConfig::new("deepseek", format!("http://{addr}/v1"), "sk-test");
    for _ in 0..2 {
        let response = client.stream_chat(&provider, &envelope, |_| {}).unwrap();
        assert_eq!(response.content.as_deref(), Some("ok"));
        // The drain runs in the background; the next agent round starts
        // after tool calls, never within microseconds.
        thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(connections.load(std::sync::atomic::Ordering::SeqCst), 1);

    let (addr, connections) = keep_alive_server(
        r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#,
        "application/json",
    );
    let provider = ProviderConfig::new("deepseek", format!("http://{addr}/v1"), "sk-test");
    for _ in 0..2 {
        let response = client.send(&provider, &envelope).unwrap();
        assert_eq!(response.content.as_deref(), Some("ok"));
    }
    assert_eq!(connections.load(std::sync::atomic::Ordering::SeqCst), 1);
}
