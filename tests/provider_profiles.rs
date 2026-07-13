use harness_cli::providers::{
    AuthScheme, BuiltinProvider, CachePolicy, ChatApiFormat, ProviderConfig, ProviderRegistry,
    builtin_pricing,
};

#[test]
fn verified_presets_carry_base_url_and_bench_tested_default_model() {
    // The user picks a provider in setup, pastes only an API key, and
    // everything else comes from the preset. Only bench-verified models
    // are defaulted: deepseek-v4-pro and glm-5.2 (26-task bench, 2026-07-13).
    let deepseek = BuiltinProvider::DeepSeek.profile();
    assert_eq!(deepseek.base_url, Some("https://api.deepseek.com/v1"));
    assert_eq!(deepseek.model_hints.first(), Some(&"deepseek-v4-pro"));

    let glm = BuiltinProvider::Glm.profile();
    assert_eq!(glm.base_url, Some("https://api.z.ai/api/paas/v4"));
    assert_eq!(glm.model_hints.first(), Some(&"glm-5.2"));

    // Families we have not bench-verified ship no base_url yet — the user
    // must pass --url explicitly rather than trust an untested default.
    assert_eq!(BuiltinProvider::Kimi.profile().base_url, None);
}

#[test]
fn builtin_pricing_covers_verified_models_with_a_dated_price_list() {
    let glm = builtin_pricing("glm", "glm-5.2").expect("glm-5.2 pricing");
    assert_eq!(glm.input_per_mtok, 1.40);
    assert_eq!(glm.cached_input_per_mtok, 0.26);
    assert_eq!(glm.output_per_mtok, 4.40);
    assert_eq!(glm.as_of, "2026-07-13");

    let pro = builtin_pricing("deepseek", "deepseek-v4-pro").expect("v4-pro pricing");
    assert_eq!(pro.input_per_mtok, 0.435);
    assert_eq!(pro.cached_input_per_mtok, 0.003625);
    assert_eq!(pro.output_per_mtok, 0.87);

    // Unknown model → honest None, not a guessed price.
    assert!(builtin_pricing("deepseek", "deepseek-unknown").is_none());
    assert!(builtin_pricing("acme", "mystery-1").is_none());
}

#[test]
fn pricing_estimates_a_session_cost_from_cache_aware_token_counts() {
    let pricing = builtin_pricing("glm", "glm-5.2").unwrap();

    // 1M prompt tokens of which 800k cached, 100k output:
    // 200k * $1.40 + 800k * $0.26 + 100k * $4.40 (per 1M each).
    let cost = pricing.estimate_usd(1_000_000, 800_000, 100_000);
    assert!((cost - (0.2 * 1.40 + 0.8 * 0.26 + 0.1 * 4.40)).abs() < 1e-9);

    // Cached counts above prompt tokens must not go negative.
    let cost = pricing.estimate_usd(100, 500, 0);
    assert!(cost >= 0.0);
}

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
