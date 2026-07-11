use harness_cli::providers::{
    AuthScheme, BuiltinProvider, CachePolicy, ChatApiFormat, ProviderConfig, ProviderRegistry,
};

#[test]
fn builtin_subscription_profiles_include_requested_families_and_deepseek_cache_hints() {
    let registry = ProviderRegistry::with_builtin_subscriptions();

    for provider in [
        BuiltinProvider::Codex,
        BuiltinProvider::Xiaomi,
        BuiltinProvider::Glm,
        BuiltinProvider::Kimi,
        BuiltinProvider::Claude,
        BuiltinProvider::DeepSeek,
    ] {
        let config = registry.get(provider.name()).expect("missing provider");
        assert_eq!(config.auth_scheme(), provider.profile().auth_scheme);
        assert_eq!(config.key_env(), Some(provider.profile().key_env));
        assert_eq!(config.cache_policy(), provider.profile().cache_policy);
        assert_eq!(config.chat_api(), provider.profile().chat_api);
        assert!(
            !config.models().is_empty(),
            "missing model hints for {provider:?}"
        );
    }

    let claude = registry.get("claude").unwrap();
    assert_eq!(
        claude.auth_scheme(),
        AuthScheme::Header {
            name: "x-api-key".to_string()
        }
    );
    assert_eq!(claude.chat_api(), ChatApiFormat::AnthropicMessages);
    assert_eq!(
        claude.cache_policy(),
        CachePolicy::AnthropicAutomatic { ttl: None }
    );

    let codex = registry.get("codex").unwrap();
    assert_eq!(codex.chat_api(), ChatApiFormat::OpenAiCodexResponses);
    assert!(codex.models().contains(&"gpt-5-codex".to_string()));

    let deepseek = registry.get("deepseek").unwrap();
    assert_eq!(
        deepseek.cache_policy(),
        CachePolicy::Automatic {
            hit_tokens_field: "prompt_cache_hit_tokens".to_string(),
            miss_tokens_field: "prompt_cache_miss_tokens".to_string(),
        }
    );
    assert!(deepseek.models().contains(&"deepseek-v4-pro".to_string()));
}

#[test]
fn custom_provider_can_override_auth_header_and_cache_header() {
    let provider = ProviderConfig::new("gateway", "http://localhost/v1", "sk-gateway")
        .with_auth_scheme(AuthScheme::Header {
            name: "x-api-key".to_string(),
        })
        .with_cache_policy(CachePolicy::Header {
            name: "x-provider-cache-key".to_string(),
        });

    assert_eq!(
        provider.auth_header(),
        Some(("x-api-key".to_string(), "sk-gateway".to_string()))
    );
    assert_eq!(
        provider.cache_header("abc123"),
        Some(("x-provider-cache-key".to_string(), "abc123".to_string()))
    );
}

#[test]
fn body_cache_control_policy_does_not_emit_cache_header() {
    let provider = ProviderConfig::new("cache-body", "http://localhost/v1", "sk-cache")
        .with_cache_policy(CachePolicy::BodyCacheControl {
            ttl: Some("1h".to_string()),
        });

    assert_eq!(provider.cache_header("abc123"), None);
}

#[test]
fn subscription_auth_resolves_key_env_when_api_key_is_empty() {
    let provider = ProviderConfig::from_profile(BuiltinProvider::DeepSeek.profile())
        .with_base_url("https://api.deepseek.com/v1");

    let header = provider.auth_header_with_lookup(|name| {
        (name == "DEEPSEEK_API_KEY").then(|| "sk-env-deepseek".to_string())
    });

    assert_eq!(
        header,
        Some((
            "Authorization".to_string(),
            "Bearer sk-env-deepseek".to_string()
        ))
    );
}

#[test]
fn explicit_api_key_takes_precedence_over_key_env() {
    let provider = ProviderConfig::from_profile(BuiltinProvider::DeepSeek.profile())
        .with_api_key("sk-explicit");

    let header = provider.auth_header_with_lookup(|_| Some("sk-env".to_string()));

    assert_eq!(
        header,
        Some((
            "Authorization".to_string(),
            "Bearer sk-explicit".to_string()
        ))
    );
}

#[test]
fn header_auth_scheme_can_resolve_key_env() {
    let provider = ProviderConfig::from_profile(BuiltinProvider::Claude.profile());

    let header = provider.auth_header_with_lookup(|name| {
        (name == "ANTHROPIC_API_KEY").then(|| "sk-env-anthropic".to_string())
    });

    assert_eq!(
        header,
        Some(("x-api-key".to_string(), "sk-env-anthropic".to_string()))
    );
}
