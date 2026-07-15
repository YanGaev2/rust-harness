use harness_cli::config::ConfigStore;
use harness_cli::providers::{AuthScheme, CachePolicy, ChatApiFormat, ProviderConfig};

#[test]
fn config_store_round_trips_custom_provider_with_models() {
    let root = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(root.path().join("providers.json"));

    let provider = ProviderConfig::new("local-openai", "http://localhost:11434/v1", "sk-local")
        .with_model("qwen3-coder")
        .with_model("glm-4.5");
    store.save_provider(provider).unwrap();

    let loaded = store.load().unwrap();
    let stored = loaded.provider("local-openai").unwrap();

    assert_eq!(stored.base_url(), "http://localhost:11434/v1");
    assert_eq!(stored.api_key(), "sk-local");
    assert_eq!(
        stored.models(),
        &["glm-4.5".to_string(), "qwen3-coder".to_string()]
    );
}

#[test]
fn config_store_round_trips_auth_and_cache_metadata() {
    let root = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(root.path().join("providers.json"));

    let provider = ProviderConfig::new("gateway", "http://localhost/v1", "sk-gateway")
        .with_auth_scheme(AuthScheme::Header {
            name: "x-api-key".to_string(),
        })
        .with_cache_policy(CachePolicy::Header {
            name: "x-provider-cache-key".to_string(),
        })
        .with_key_env("GATEWAY_API_KEY");
    store.save_provider(provider).unwrap();

    let loaded = store.load().unwrap();
    let stored = loaded.provider("gateway").unwrap();

    assert_eq!(
        stored.auth_scheme(),
        AuthScheme::Header {
            name: "x-api-key".to_string()
        }
    );
    assert_eq!(
        stored.cache_policy(),
        CachePolicy::Header {
            name: "x-provider-cache-key".to_string()
        }
    );
    assert_eq!(stored.key_env(), Some("GATEWAY_API_KEY"));
}

#[test]
fn config_store_round_trips_anthropic_automatic_cache_metadata() {
    let root = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(root.path().join("providers.json"));

    let provider = ProviderConfig::new("claude", "https://api.anthropic.com/v1", "sk-anthropic")
        .with_cache_policy(CachePolicy::AnthropicAutomatic {
            ttl: Some("1h".to_string()),
        });
    store.save_provider(provider).unwrap();

    let loaded = store.load().unwrap();
    let stored = loaded.provider("claude").unwrap();

    assert_eq!(
        stored.cache_policy(),
        CachePolicy::AnthropicAutomatic {
            ttl: Some("1h".to_string())
        }
    );
}

#[test]
fn config_store_round_trips_body_cache_control_metadata() {
    let root = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(root.path().join("providers.json"));

    let provider = ProviderConfig::new("cache-body", "http://localhost/v1", "sk-cache")
        .with_cache_policy(CachePolicy::BodyCacheControl {
            ttl: Some("5m".to_string()),
        });
    store.save_provider(provider).unwrap();

    let loaded = store.load().unwrap();
    let stored = loaded.provider("cache-body").unwrap();

    assert_eq!(
        stored.cache_policy(),
        CachePolicy::BodyCacheControl {
            ttl: Some("5m".to_string())
        }
    );
}

#[test]
fn config_store_round_trips_openai_responses_chat_api_metadata() {
    let root = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(root.path().join("providers.json"));

    let provider = ProviderConfig::new("openai", "https://api.openai.com/v1", "sk-openai")
        .with_chat_api(ChatApiFormat::OpenAiResponses);
    store.save_provider(provider).unwrap();

    let loaded = store.load().unwrap();
    let stored = loaded.provider("openai").unwrap();

    assert_eq!(stored.chat_api(), ChatApiFormat::OpenAiResponses);
}

#[test]
fn config_store_round_trips_openai_codex_responses_chat_api_metadata() {
    let root = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(root.path().join("providers.json"));

    let provider = ProviderConfig::new("codex", "https://api.openai.com/v1", "sk-openai")
        .with_chat_api(ChatApiFormat::OpenAiCodexResponses);
    store.save_provider(provider).unwrap();

    let loaded = store.load().unwrap();
    let stored = loaded.provider("codex").unwrap();

    assert_eq!(stored.chat_api(), ChatApiFormat::OpenAiCodexResponses);
}

#[test]
fn config_store_round_trips_provider_proxy() {
    let root = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(root.path().join("providers.json"));

    let provider = ProviderConfig::new("openrouter", "https://openrouter.ai/api/v1", "sk-or")
        .with_proxy("http://user:pass@127.0.0.1:8080");
    store.save_provider(provider).unwrap();

    let loaded = store.load().unwrap();
    assert_eq!(
        loaded.provider("openrouter").unwrap().proxy(),
        Some("http://user:pass@127.0.0.1:8080")
    );
}

#[test]
fn config_store_round_trips_global_proxy_and_resolves_into_providers() {
    let root = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(root.path().join("providers.json"));

    store
        .save_provider(ProviderConfig::new("a", "https://a.example/v1", "k"))
        .unwrap();
    store
        .save_provider(ProviderConfig::new("b", "https://b.example/v1", "k").with_proxy("none"))
        .unwrap();
    store.set_proxy("http://127.0.0.1:9999").unwrap();

    let loaded = store.load().unwrap();
    assert_eq!(loaded.proxy(), Some("http://127.0.0.1:9999"));
    // Global proxy flows into providers that did not set their own...
    assert_eq!(
        loaded.resolved_provider("a").unwrap().proxy(),
        Some("http://127.0.0.1:9999")
    );
    // ...while an explicit per-provider "none" keeps that provider direct.
    assert_eq!(loaded.resolved_provider("b").unwrap().proxy(), Some("none"));
}

#[test]
fn config_store_round_trips_provider_extra_body() {
    let root = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(root.path().join("providers.json"));

    let extra = serde_json::json!({
        "provider": {"only": ["wandb"], "allow_fallbacks": false},
    });
    let provider = ProviderConfig::new("openrouter", "https://openrouter.ai/api/v1", "sk-or")
        .with_extra_body(extra.clone());
    store.save_provider(provider).unwrap();

    let loaded = store.load().unwrap();
    assert_eq!(
        loaded.provider("openrouter").unwrap().extra_body(),
        Some(&extra)
    );
}

#[test]
fn config_store_loads_legacy_files_without_proxy_fields() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("providers.json");
    std::fs::write(
        &path,
        r#"{"providers":{"old":{"name":"old","base_url":"https://x.example/v1","api_key":"k","models":[]}}}"#,
    )
    .unwrap();

    let loaded = ConfigStore::new(&path).load().unwrap();
    assert_eq!(loaded.proxy(), None);
    assert_eq!(loaded.provider("old").unwrap().proxy(), None);
}
