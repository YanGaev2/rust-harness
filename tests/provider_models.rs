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
