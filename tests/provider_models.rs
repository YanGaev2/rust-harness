use harness_cli::providers::{ModelChoice, ModelDiscovery, ProviderConfig};

#[test]
fn openai_compatible_model_discovery_includes_add_all_choice() {
    let raw = r#"{
        "object": "list",
        "data": [
            {"id": "kimi-k2"},
            {"id": "deepseek-chat"},
            {"id": "glm-4.5"}
        ]
    }"#;
    let provider = ProviderConfig::new("custom-router", "https://api.example.com/v1", "sk-test");

    let discovery = ModelDiscovery::from_openai_compatible_response(&provider, raw).unwrap();

    assert_eq!(discovery.provider_name(), "custom-router");
    assert_eq!(
        discovery.choices(),
        &[
            ModelChoice::AddAll,
            ModelChoice::Model("deepseek-chat".to_string()),
            ModelChoice::Model("glm-4.5".to_string()),
            ModelChoice::Model("kimi-k2".to_string()),
        ]
    );
    assert_eq!(
        discovery.add_all_model_ids(),
        vec![
            "deepseek-chat".to_string(),
            "glm-4.5".to_string(),
            "kimi-k2".to_string(),
        ]
    );
}

#[test]
fn model_discovery_reads_the_context_window_under_its_common_names() {
    // baseten and OpenRouter: `context_length`; Groq-style: `context_window`;
    // vLLM: `max_model_len`. DeepSeek reports none.
    let raw = r#"{
        "object": "list",
        "data": [
            {"id": "moonshotai/Kimi-K3", "context_length": 1048576, "max_completion_tokens": 262144},
            {"id": "llama-groq", "context_window": 131072},
            {"id": "local-vllm", "max_model_len": 32768},
            {"id": "deepseek-v4-pro", "object": "model", "owned_by": "deepseek"}
        ]
    }"#;
    let provider = ProviderConfig::new("baseten", "https://inference.baseten.co/v1", "sk-test");
    let discovery = ModelDiscovery::from_openai_compatible_response(&provider, raw).unwrap();

    assert_eq!(
        discovery.context_window("moonshotai/Kimi-K3"),
        Some(1_048_576)
    );
    assert_eq!(discovery.context_window("llama-groq"), Some(131_072));
    assert_eq!(discovery.context_window("local-vllm"), Some(32_768));
    assert_eq!(discovery.context_window("deepseek-v4-pro"), None);
    assert_eq!(discovery.context_window("not-listed"), None);
}
