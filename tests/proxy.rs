//! Proxy behaviour of the chat client.
//!
//! The rule under test: the harness talks through a proxy ONLY when the
//! config asks for one. `HTTP_PROXY`/`HTTPS_PROXY` from the environment are
//! ignored unless a provider (or the global config) says `"proxy": "env"` —
//! ambient env proxies silently rerouting localhost traffic is exactly the
//! failure mode that broke every mock-server test during the ureq 3
//! migration.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use harness_cli::chat_client::OpenAiCompatibleChatClient;
use harness_cli::prompt::DEFAULT_SYSTEM_PROMPT;
use harness_cli::providers::ProviderConfig;
use harness_cli::request::{ChatMessage, RequestEnvelope};

fn read_http_request(stream: &mut TcpStream) -> String {
    let mut buffer = Vec::new();
    let mut chunk = [0; 1024];
    loop {
        let read = stream.read(&mut chunk).unwrap();
        assert!(read > 0, "connection closed before full request arrived");
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(header_end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
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

fn ok_chat_response(stream: &mut TcpStream) {
    let body = r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).unwrap();
}

#[test]
fn configured_proxy_receives_the_request_as_a_connect_tunnel() {
    // Only the "proxy" listens; the target host does not exist. ureq opens a
    // CONNECT tunnel naming the target, then sends the real request through
    // it — seeing both here proves the configured proxy is actually used.
    let proxy_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = proxy_listener.accept().unwrap();
        let connect = read_http_request(&mut stream);
        assert!(
            connect.starts_with("CONNECT target.invalid:59999"),
            "request line was: {}",
            connect.lines().next().unwrap_or("")
        );
        stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .unwrap();
        // Inside the tunnel flows the ordinary origin-form request.
        let request = read_http_request(&mut stream);
        assert!(request.starts_with("POST /v1/chat/completions"));
        ok_chat_response(&mut stream);
    });

    let provider = ProviderConfig::new("proxied", "http://target.invalid:59999/v1", "sk-test")
        .with_proxy(format!("http://{proxy_addr}"));
    let envelope = RequestEnvelope::new("proxied", "m")
        .with_system_prompt(DEFAULT_SYSTEM_PROMPT)
        .with_messages(vec![ChatMessage::user("ping")]);

    let response = OpenAiCompatibleChatClient::new(Duration::from_secs(2))
        .send(&provider, &envelope)
        .unwrap();
    server.join().unwrap();
    assert_eq!(response.content.as_deref(), Some("ok"));
}

#[test]
fn env_proxies_are_ignored_unless_config_opts_in() {
    // This test intentionally poisons the proxy env vars for this whole test
    // process (it lives alone in this integration binary next to the proxy
    // opt-in test above, which never reads the environment).
    unsafe {
        std::env::set_var("HTTP_PROXY", "http://127.0.0.1:1");
        std::env::set_var("HTTPS_PROXY", "http://127.0.0.1:1");
        std::env::set_var("ALL_PROXY", "http://127.0.0.1:1");
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        // Direct request: origin-form path, not absolute-form.
        assert!(request.starts_with("POST /v1/chat/completions"));
        ok_chat_response(&mut stream);
    });

    // No proxy in the provider config → the poisoned env must not matter.
    let provider = ProviderConfig::new("direct", format!("http://{addr}/v1"), "sk-test");
    let envelope = RequestEnvelope::new("direct", "m")
        .with_system_prompt(DEFAULT_SYSTEM_PROMPT)
        .with_messages(vec![ChatMessage::user("ping")]);

    let response = OpenAiCompatibleChatClient::new(Duration::from_secs(2))
        .send(&provider, &envelope)
        .unwrap();
    server.join().unwrap();
    assert_eq!(response.content.as_deref(), Some("ok"));
}
