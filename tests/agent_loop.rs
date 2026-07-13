use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use harness_cli::agent::{AgentError, AgentEvent, AgentRunner};
use harness_cli::providers::ProviderConfig;
use harness_cli::request::ChatMessage;
use serde_json::Value;

#[test]
fn agent_run_accumulates_cache_aware_usage_and_estimates_cost() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let workspace = tempfile::tempdir().unwrap();

    let server = thread::spawn(move || {
        let (mut first_stream, _) = listener.accept().unwrap();
        read_http_request(&mut first_stream);
        respond(
            &mut first_stream,
            r#"{
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "u-1",
                            "type": "function",
                            "function": {
                                "name": "write_file",
                                "arguments": "{\"file_path\":\"a.txt\",\"content\":\"x\"}"
                            }
                        }]
                    }
                }],
                "usage": {"prompt_tokens": 1000, "completion_tokens": 10,
                          "total_tokens": 1010,
                          "prompt_cache_hit_tokens": 600,
                          "prompt_cache_miss_tokens": 400}
            }"#,
        );

        let (mut second_stream, _) = listener.accept().unwrap();
        read_http_request(&mut second_stream);
        respond(
            &mut second_stream,
            r#"{
                "choices": [{"message": {"role": "assistant", "content": "done"}}],
                "usage": {"prompt_tokens": 2000, "completion_tokens": 20,
                          "total_tokens": 2020,
                          "prompt_cache_hit_tokens": 1800,
                          "prompt_cache_miss_tokens": 200}
            }"#,
        );
    });

    // Named after a verified preset so the built-in price list applies.
    let provider = ProviderConfig::new("deepseek", format!("http://{addr}/v1"), "sk-agent");
    let runner = AgentRunner::new(provider, "deepseek-v4-pro", workspace.path())
        .with_timeout(Duration::from_secs(2))
        .with_max_tool_rounds(3);

    let mut usage_events = 0;
    let result = runner
        .run_with_events("write a.txt", |event| {
            if matches!(event, AgentEvent::UsageUpdated(_)) {
                usage_events += 1;
            }
        })
        .unwrap();
    server.join().unwrap();

    let usage = &result.trace.usage;
    assert_eq!(usage.requests, 2);
    assert_eq!(usage.prompt_tokens, 3000);
    assert_eq!(usage.cached_tokens, 2400);
    assert_eq!(usage.completion_tokens, 30);
    assert_eq!(usage_events, 2, "one usage event per round");

    // 600 fresh * $0.435 + 2400 cached * $0.003625 + 30 out * $0.87, per 1M.
    let cost = result.trace.estimated_cost_usd.expect("priced model");
    assert!((cost - 0.0002958).abs() < 1e-9, "cost={cost}");
    assert_eq!(result.trace.pricing_as_of.as_deref(), Some("2026-07-13"));
}

#[test]
fn agent_runner_executes_tool_calls_and_continues_until_final_answer() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let workspace = tempfile::tempdir().unwrap();

    let server = thread::spawn(move || {
        let (mut first_stream, _) = listener.accept().unwrap();
        let first = read_http_request(&mut first_stream);
        let first_body: Value =
            serde_json::from_str(first.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(first_body["messages"][0]["role"], "system");
        assert_eq!(first_body["messages"][1]["role"], "user");
        assert_eq!(first_body["tools"][1]["function"]["name"], "write_file");
        respond(
            &mut first_stream,
            r#"{
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call-1",
                            "type": "function",
                            "function": {
                                "name": "write_file",
                                "arguments": "{\"file\":\"notes.txt\",\"text\":\"done by tool\"}"
                            }
                        }]
                    }
                }],
                "usage": {"prompt_tokens": 20, "completion_tokens": 8, "total_tokens": 28}
            }"#,
        );

        let (mut second_stream, _) = listener.accept().unwrap();
        let second = read_http_request(&mut second_stream);
        let second_body: Value =
            serde_json::from_str(second.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(second_body["messages"][2]["role"], "assistant");
        assert_eq!(second_body["messages"][2]["tool_calls"][0]["id"], "call-1");
        assert_eq!(
            second_body["messages"][2]["tool_calls"][0]["function"]["name"],
            "write_file"
        );
        assert_eq!(second_body["messages"][3]["role"], "tool");
        assert_eq!(second_body["messages"][3]["tool_call_id"], "call-1");
        let tool_result: Value =
            serde_json::from_str(second_body["messages"][3]["content"].as_str().unwrap()).unwrap();
        assert_eq!(tool_result["ok"], true);
        // `write_file` with `file`/`text` aliases is exactly the advertised
        // wire name plus accepted aliases — a clean call, not a repair,
        // so no corrective memo is attached.
        assert_eq!(tool_result["repaired"], false);
        assert!(
            tool_result["hint"].is_null(),
            "clean call must not carry a memo: {tool_result}"
        );

        respond(
            &mut second_stream,
            r#"{
                "choices": [{"message": {"role": "assistant", "content": "finished"}}],
                "usage": {"prompt_tokens": 30, "completion_tokens": 2, "total_tokens": 32}
            }"#,
        );
    });

    let provider = ProviderConfig::new("local", format!("http://{addr}/v1"), "sk-agent");
    let runner = AgentRunner::new(provider, "deepseek-v4-pro", workspace.path())
        .with_timeout(Duration::from_secs(2))
        .with_max_tool_rounds(3);

    let result = runner.run("write notes.txt").unwrap();
    server.join().unwrap();

    assert_eq!(result.final_content.as_deref(), Some("finished"));
    assert_eq!(result.tool_results.len(), 1);
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("notes.txt")).unwrap(),
        "done by tool"
    );
}

#[test]
fn agent_runner_returns_tool_errors_to_model_and_continues() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let workspace = tempfile::tempdir().unwrap();

    let server = thread::spawn(move || {
        let mut first_stream = accept_with_timeout(&listener);
        let first = read_http_request(&mut first_stream);
        let first_body: Value =
            serde_json::from_str(first.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(first_body["messages"][1]["content"], "try a tool");
        respond(
            &mut first_stream,
            r#"{
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call-bad",
                            "type": "function",
                            "function": {
                                "name": "missing_tool",
                                "arguments": "{}"
                            }
                        }]
                    }
                }]
            }"#,
        );

        let mut second_stream = accept_with_timeout(&listener);
        let second = read_http_request(&mut second_stream);
        let second_body: Value =
            serde_json::from_str(second.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(second_body["messages"][3]["role"], "tool");
        assert_eq!(second_body["messages"][3]["tool_call_id"], "call-bad");
        let tool_result: Value =
            serde_json::from_str(second_body["messages"][3]["content"].as_str().unwrap()).unwrap();
        assert_eq!(tool_result["ok"], false);
        assert!(
            tool_result["error"]
                .as_str()
                .unwrap()
                .contains("unknown tool")
        );

        respond(
            &mut second_stream,
            r#"{
                "choices": [{"message": {"role": "assistant", "content": "recovered"}}]
            }"#,
        );
    });

    let provider = ProviderConfig::new("local", format!("http://{addr}/v1"), "sk-agent");
    let runner = AgentRunner::new(provider, "deepseek-v4-pro", workspace.path())
        .with_timeout(Duration::from_secs(2))
        .with_max_tool_rounds(3);

    let result = runner.run("try a tool");
    let server_result = server.join();
    let result = result.unwrap();
    server_result.unwrap();

    assert_eq!(result.final_content.as_deref(), Some("recovered"));
    assert_eq!(result.tool_results.len(), 1);
    assert!(!result.tool_results[0].ok);
    assert!(
        result.tool_results[0]
            .error
            .as_deref()
            .unwrap()
            .contains("unknown tool")
    );
}

#[test]
fn agent_runner_emits_events_for_tool_rounds_results_and_final_content() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let workspace = tempfile::tempdir().unwrap();

    let server = thread::spawn(move || {
        let (mut first_stream, _) = listener.accept().unwrap();
        let first = read_http_request(&mut first_stream);
        let first_body: Value =
            serde_json::from_str(first.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(first_body["messages"][1]["content"], "write streamed notes");
        respond(
            &mut first_stream,
            r#"{
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call-stream",
                            "type": "function",
                            "function": {
                                "name": "write_file",
                                "arguments": "{\"file\":\"stream.txt\",\"text\":\"streamed tool\"}"
                            }
                        }]
                    }
                }]
            }"#,
        );

        let (mut second_stream, _) = listener.accept().unwrap();
        let second = read_http_request(&mut second_stream);
        let second_body: Value =
            serde_json::from_str(second.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(second_body["messages"][3]["tool_call_id"], "call-stream");
        respond(
            &mut second_stream,
            r#"{
                "choices": [{"message": {"role": "assistant", "content": "streamed final"}}]
            }"#,
        );
    });

    let provider = ProviderConfig::new("local", format!("http://{addr}/v1"), "sk-agent");
    let runner = AgentRunner::new(provider, "deepseek-v4-pro", workspace.path())
        .with_timeout(Duration::from_secs(2))
        .with_max_tool_rounds(3);

    let mut events = Vec::new();
    let result = runner
        .run_with_events("write streamed notes", |event| events.push(event))
        .unwrap();
    server.join().unwrap();

    assert_eq!(result.final_content.as_deref(), Some("streamed final"));
    // Usage bookkeeping events interleave; this test asserts the UI stream.
    events.retain(|event| !matches!(event, AgentEvent::UsageUpdated(_)));
    assert!(matches!(
        events[0],
        AgentEvent::ToolRoundStarted {
            round: 1,
            tool_calls: 1
        }
    ));
    let AgentEvent::ToolCallStarted {
        round, id, name, ..
    } = &events[1]
    else {
        panic!("expected tool-call-started event, got {:?}", events[1]);
    };
    assert_eq!(*round, 1);
    assert_eq!(id, "call-stream");
    assert_eq!(name, "write_file");
    let AgentEvent::ToolResult(result) = &events[2] else {
        panic!("expected tool result event, got {:?}", events[2]);
    };
    assert_eq!(result.id, "call-stream");
    assert!(result.ok);
    assert_eq!(
        events[3],
        AgentEvent::FinalContentDelta("streamed final".to_string())
    );
}

#[test]
fn agent_emits_thinking_event_when_provider_returns_reasoning() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let workspace = tempfile::tempdir().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_http_request(&mut stream);
        respond(
            &mut stream,
            r#"{
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "reasoning_content": "Let me weigh the options first.",
                        "content": "final answer"
                    }
                }]
            }"#,
        );
    });

    let provider = ProviderConfig::new("local", format!("http://{addr}/v1"), "sk-agent");
    let runner = AgentRunner::new(provider, "deepseek-reasoner", workspace.path())
        .with_timeout(Duration::from_secs(2))
        .with_max_tool_rounds(2);

    let mut events = Vec::new();
    let result = runner
        .run_with_events("think it through", |event| events.push(event))
        .unwrap();
    server.join().unwrap();

    assert_eq!(result.final_content.as_deref(), Some("final answer"));
    events.retain(|event| !matches!(event, AgentEvent::UsageUpdated(_)));
    assert_eq!(
        events[0],
        AgentEvent::Thinking("Let me weigh the options first.".to_string())
    );
    // The reasoning is also captured in the trace for `--trace` output.
    let trace_json = serde_json::to_value(&result.trace).unwrap();
    assert_eq!(trace_json["events"][0]["type"], "thinking");
    assert_eq!(
        trace_json["events"][0]["content"],
        "Let me weigh the options first."
    );
}

#[test]
fn agent_streams_content_and_thinking_deltas_without_duplicating() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let workspace = tempfile::tempdir().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        // Streaming mode must request an SSE stream.
        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let json: Value = serde_json::from_str(body).unwrap();
        assert_eq!(json["stream"], true);
        respond_sse(
            &mut stream,
            &[
                r#"{"choices":[{"delta":{"reasoning_content":"plan"},"index":0}]}"#,
                r#"{"choices":[{"delta":{"content":"wri"},"index":0}]}"#,
                r#"{"choices":[{"delta":{"content":"ting"},"index":0}]}"#,
                "[DONE]",
            ],
        );
    });

    let provider = ProviderConfig::new("local", format!("http://{addr}/v1"), "sk-agent");
    let runner = AgentRunner::new(provider, "deepseek-v4-pro", workspace.path())
        .with_timeout(Duration::from_secs(2))
        .with_streaming(true);

    let mut events = Vec::new();
    let result = runner
        .run_with_events("write a file", |event| events.push(event))
        .unwrap();
    server.join().unwrap();

    assert_eq!(result.final_content.as_deref(), Some("writing"));

    // Reasoning arrives as a Thinking event...
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::Thinking(text) if text == "plan")),
        "expected a streamed thinking event, got {events:?}"
    );
    // ...and content arrives as live deltas that are NOT re-emitted whole at the end.
    let content_deltas: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::FinalContentDelta(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(content_deltas, vec!["wri", "ting"]);
}

#[test]
fn agent_runner_cancelled_flag_stops_before_any_request() {
    // No server at all: if the runner tried to send a request it would burn the
    // full timeout and fail with a connection error instead of Cancelled.
    let workspace = tempfile::tempdir().unwrap();
    let cancel = Arc::new(AtomicBool::new(true));
    let provider = ProviderConfig::new("local", "http://127.0.0.1:9/v1", "sk-agent");
    let runner = AgentRunner::new(provider, "deepseek-v4-pro", workspace.path())
        .with_timeout(Duration::from_secs(2))
        .with_cancel_flag(cancel);

    let err = runner.run("never sent").unwrap_err();

    assert!(matches!(err, AgentError::Cancelled { .. }));
    let trace = err.trace().expect("cancelled run must keep its trace");
    let trace_json = serde_json::to_value(trace).unwrap();
    assert_eq!(trace_json["events"][0]["type"], "error");
    assert_eq!(trace_json["events"][0]["message"], "interrupted by user");
}

#[test]
fn agent_runner_cancel_during_tools_skips_the_next_round() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let workspace = tempfile::tempdir().unwrap();

    // The server answers exactly one request (a tool call); if the runner sent
    // a second one after cancellation, the runner would hang on a dead socket.
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_http_request(&mut stream);
        respond(
            &mut stream,
            r#"{
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call-1",
                            "type": "function",
                            "function": {
                                "name": "write_file",
                                "arguments": "{\"file\":\"notes.txt\",\"text\":\"done\"}"
                            }
                        }]
                    }
                }]
            }"#,
        );
    });

    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_in_events = cancel.clone();
    let provider = ProviderConfig::new("local", format!("http://{addr}/v1"), "sk-agent");
    let runner = AgentRunner::new(provider, "deepseek-v4-pro", workspace.path())
        .with_timeout(Duration::from_secs(2))
        .with_max_tool_rounds(3)
        .with_cancel_flag(cancel);

    // Simulate Esc arriving while the tool batch runs: flag flips as soon as
    // the tool result comes back, before the next provider round starts.
    let err = runner
        .run_with_events("write then get interrupted", |event| {
            if matches!(event, AgentEvent::ToolResult(_)) {
                cancel_in_events.store(true, Ordering::SeqCst);
            }
        })
        .unwrap_err();
    server.join().unwrap();

    assert!(matches!(err, AgentError::Cancelled { .. }));
    let trace_json = serde_json::to_value(err.trace().unwrap()).unwrap();
    let types: Vec<&str> = trace_json["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["type"].as_str().unwrap())
        .collect();
    assert_eq!(types, vec!["model_tool_calls", "tool_result", "error"]);
}

#[test]
fn agent_streaming_cancel_stops_mid_stream() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let workspace = tempfile::tempdir().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_http_request(&mut stream);
        respond_sse(
            &mut stream,
            &[
                r#"{"choices":[{"delta":{"content":"wri"},"index":0}]}"#,
                r#"{"choices":[{"delta":{"content":"ting"},"index":0}]}"#,
                "[DONE]",
            ],
        );
    });

    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_in_events = cancel.clone();
    let provider = ProviderConfig::new("local", format!("http://{addr}/v1"), "sk-agent");
    let runner = AgentRunner::new(provider, "deepseek-v4-pro", workspace.path())
        .with_timeout(Duration::from_secs(2))
        .with_streaming(true)
        .with_cancel_flag(cancel);

    let mut deltas = Vec::new();
    let err = runner
        .run_with_events("stream then stop", |event| {
            if let AgentEvent::FinalContentDelta(text) = &event {
                deltas.push(text.clone());
                cancel_in_events.store(true, Ordering::SeqCst);
            }
        })
        .unwrap_err();
    server.join().unwrap();

    assert!(matches!(err, AgentError::Cancelled { .. }));
    // The second SSE chunk must never be surfaced after the flag flipped.
    assert_eq!(deltas, vec!["wri".to_string()]);
}

#[test]
fn agent_runner_replays_history_before_new_user_message() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let workspace = tempfile::tempdir().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        let body: Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        // History slots in after the system prompt and before the new user turn.
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "first question");
        assert_eq!(body["messages"][2]["role"], "assistant");
        assert_eq!(body["messages"][2]["content"], "first answer");
        assert_eq!(body["messages"][3]["role"], "user");
        assert_eq!(body["messages"][3]["content"], "follow-up");
        respond(
            &mut stream,
            r#"{
                "choices": [{"message": {"role": "assistant", "content": "second answer"}}]
            }"#,
        );
    });

    let history = vec![
        ChatMessage::user("first question"),
        ChatMessage::assistant("first answer"),
    ];
    let provider = ProviderConfig::new("local", format!("http://{addr}/v1"), "sk-agent");
    let runner = AgentRunner::new(provider, "deepseek-v4-pro", workspace.path())
        .with_timeout(Duration::from_secs(2))
        .with_history(history.clone());

    let result = runner.run("follow-up").unwrap();
    server.join().unwrap();

    assert_eq!(result.final_content.as_deref(), Some("second answer"));
    // The result carries the full post-run conversation so the caller can
    // persist it and feed it back as next-turn history.
    let mut expected = history;
    expected.push(ChatMessage::user("follow-up"));
    expected.push(ChatMessage::assistant("second answer"));
    assert_eq!(result.messages, expected);
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

fn accept_with_timeout(listener: &TcpListener) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(2);

    loop {
        match listener.accept() {
            Ok((stream, _)) => return stream,
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for agent request"
                );
                thread::sleep(Duration::from_millis(10));
            }
            Err(err) => panic!("failed to accept agent request: {err}"),
        }
    }
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn respond(stream: &mut TcpStream, body: &str) {
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
