use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

use harness_cli::model_client::OpenAiCompatibleModelClient;
use harness_cli::providers::{AuthScheme, ProviderConfig};

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
