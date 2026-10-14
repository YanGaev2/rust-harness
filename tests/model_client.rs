use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

use harness_cli::model_client::OpenAiCompatibleModelClient;
use harness_cli::providers::{AuthScheme, ChatApiFormat, ProviderConfig};

#[test]
fn model_client_fetches_openai_compatible_models_with_bearer_key() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buffer = [0; 2048];
        let read = stream.read(&mut buffer).unwrap();
        let request = String::from_utf8_lossy(&buffer[..read]).to_string();
        let request_lower = request.to_ascii_lowercase();

        assert!(request.starts_with("GET /v1/models "));
        assert!(request_lower.contains("authorization: bearer sk-test"));

        let body = r#"{"object":"list","data":[{"id":"glm-4.5"},{"id":"kimi-k2"}]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).unwrap();
    });

    let provider = ProviderConfig::new("custom", format!("http://{addr}/v1"), "sk-test");
    let client = OpenAiCompatibleModelClient::new(Duration::from_secs(2));

    let discovery = client.list_models(&provider).unwrap();
    server.join().unwrap();

    assert_eq!(
        discovery.add_all_model_ids(),
        vec!["glm-4.5".to_string(), "kimi-k2".to_string()]
    );
}

#[test]
fn model_client_uses_provider_auth_header_override() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buffer = [0; 2048];
        let read = stream.read(&mut buffer).unwrap();
        let request = String::from_utf8_lossy(&buffer[..read]).to_string();
        let request_lower = request.to_ascii_lowercase();

        assert!(request.starts_with("GET /v1/models "));
        assert!(request_lower.contains("x-api-key: sk-test"));
        assert!(!request_lower.contains("authorization: bearer sk-test"));

        let body = r#"{"object":"list","data":[{"id":"gateway-model"}]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).unwrap();
    });

    let provider = ProviderConfig::new("custom", format!("http://{addr}/v1"), "sk-test")
        .with_auth_scheme(AuthScheme::Header {
            name: "x-api-key".to_string(),
        });
    let client = OpenAiCompatibleModelClient::new(Duration::from_secs(2));

    let discovery = client.list_models(&provider).unwrap();
    server.join().unwrap();

    assert_eq!(
        discovery.add_all_model_ids(),
        vec!["gateway-model".to_string()]
    );
}

/// Serve one canned response per request; hand back the raw requests.
fn serve(
    responses: Vec<(&'static str, &'static str)>,
) -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        responses
            .into_iter()
            .map(|(status, body)| {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buffer = [0; 4096];
                let read = stream.read(&mut buffer).unwrap();
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).unwrap();
                String::from_utf8_lossy(&buffer[..read]).to_string()
            })
            .collect()
    });
    (format!("http://{addr}/v1"), server)
}

#[test]
fn discover_keeps_openai_format_and_reads_openrouter_window_and_prices() {
    let (url, server) = serve(vec![(
        "200 OK",
        r#"{"data":[{"id":"anthropic/claude-sonnet","context_length":200000,
            "pricing":{"prompt":"0.000003","completion":"0.000015","input_cache_read":"0.0000003"}},
            {"id":"openrouter/auto","pricing":{"prompt":"-1","completion":"-1"}}]}"#,
    )]);
    let provider = ProviderConfig::new("openrouter", url, "sk-test");

    let (provider, discovery) = OpenAiCompatibleModelClient::new(Duration::from_secs(2))
        .discover(provider)
        .unwrap();
    let requests = server.join().unwrap();

    assert_eq!(requests.len(), 1);
    assert_eq!(provider.chat_api(), ChatApiFormat::OpenAiCompatible);
    let info = discovery.model_info("anthropic/claude-sonnet");
    assert_eq!(info.context_window, Some(200_000));
    let pricing = info.pricing.unwrap();
    assert!((pricing.input_per_mtok - 3.0).abs() < 1e-9);
    assert!((pricing.cached_input_per_mtok - 0.3).abs() < 1e-9);
    assert!((pricing.output_per_mtok - 15.0).abs() < 1e-9);
    // Stamped with the fetch date, YYYY-MM-DD.
    assert_eq!(pricing.as_of.len(), 10);
    // A negative price means "unknown", not free.
    assert_eq!(discovery.model_info("openrouter/auto").pricing, None);
}

#[test]
fn discover_retries_anthropic_style_when_the_bearer_key_is_rejected() {
    let (url, server) = serve(vec![
        ("401 Unauthorized", r#"{"error":"invalid x-api-key"}"#),
        (
            "200 OK",
            r#"{"data":[{"type":"model","id":"claude-opus-5","display_name":"Claude Opus 5",
                "max_input_tokens":1000000,"max_tokens":128000}],
               "has_more":false,"first_id":"claude-opus-5","last_id":"claude-opus-5"}"#,
        ),
    ]);
    let provider = ProviderConfig::new("my-anthropic", url, "sk-ant-test");

    let (provider, discovery) = OpenAiCompatibleModelClient::new(Duration::from_secs(2))
        .discover(provider)
        .unwrap();
    let requests = server.join().unwrap();

    let retry = requests[1].to_ascii_lowercase();
    assert!(retry.starts_with("get /v1/models?limit=1000 "), "{retry}");
    assert!(retry.contains("x-api-key: sk-ant-test"));
    assert!(retry.contains("anthropic-version: 2023-06-01"));
    assert_eq!(provider.chat_api(), ChatApiFormat::AnthropicMessages);
    assert_eq!(
        provider.auth_scheme(),
        AuthScheme::Header {
            name: "x-api-key".to_string()
        }
    );
    assert_eq!(discovery.context_window("claude-opus-5"), Some(1_000_000));
}

#[test]
fn discover_reports_the_original_error_when_both_styles_fail() {
    let (url, server) = serve(vec![
        ("401 Unauthorized", r#"{"error":"bad key"}"#),
        ("401 Unauthorized", r#"{"error":"bad key"}"#),
    ]);
    let provider = ProviderConfig::new("custom", url, "sk-wrong");

    let err = OpenAiCompatibleModelClient::new(Duration::from_secs(2))
        .discover(provider)
        .unwrap_err();
    server.join().unwrap();

    assert!(err.to_string().contains("401"), "{err}");
}
