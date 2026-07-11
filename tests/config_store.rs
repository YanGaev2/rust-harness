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
