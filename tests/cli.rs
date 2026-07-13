use std::io::{Cursor, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Once, OnceLock};
use std::thread;

use harness_cli::cli;
use harness_cli::config::ConfigStore;
use harness_cli::providers::{AuthScheme, CachePolicy, ChatApiFormat, ProviderConfig};
use serde_json::Value;

/// Point HARNESS_HOME at a process-shared tempdir so `agent run`'s automatic
/// trace persistence never touches the developer's real `~/.harness`. Every
/// test that reaches the agent loop must call this before `cli::run`.
fn isolate_harness_home() -> PathBuf {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    static SET: Once = Once::new();
    let dir = DIR.get_or_init(|| tempfile::tempdir().unwrap());
    SET.call_once(|| {
        // SAFETY: set exactly once, before any agent-run test reads it; the
        // value never changes afterwards.
        unsafe { std::env::set_var("HARNESS_HOME", dir.path()) };
    });
    dir.path().to_path_buf()
}

#[test]
fn package_installs_short_harness_binary() {
    let manifest =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();

    assert!(manifest.contains("[[bin]]"));
    assert!(manifest.contains("name = \"harness\""));
}

#[test]
fn default_launch_resolves_setup_interface_when_no_provider_is_configured() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");

    let launch = cli::resolve_default_launch(&config_path, root.path(), "harness").unwrap();

    match launch {
        cli::DefaultLaunch::Setup(options) => {
            assert_eq!(options.config_path, config_path);
            assert_eq!(options.workspace, root.path());
            assert_eq!(options.command_name, "harness");
        }
        other => panic!("expected setup launch, got {other:?}"),
    }
}

#[test]
fn repl_resolves_first_configured_provider_when_flags_are_omitted() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    let store = ConfigStore::new(&config_path);
    store
        .save_provider(
            ProviderConfig::new("deepseek", "https://api.deepseek.com", "")
                .with_model("deepseek-v4-pro"),
        )
        .unwrap();
    let config = store.load().unwrap();

    // `harness repl` with no --provider/--model must pick the configured provider.
    let (provider, model) = cli::resolve_repl_provider(&config, None, None).unwrap();
    assert_eq!(provider.name(), "deepseek");
    assert_eq!(model, "deepseek-v4-pro");
}

#[test]
fn repl_reports_actionable_error_when_no_provider_is_configured() {
    let config = harness_cli::config::HarnessConfig::default();
    let err = cli::resolve_repl_provider(&config, None, None).unwrap_err();
    let message = format!("{err}");
    assert!(
        message.contains("/provider add") || message.contains("no provider"),
        "error should guide the user to configure a provider: {message}"
    );
}

#[test]
fn repl_rejects_unknown_explicit_provider() {
    let config = harness_cli::config::HarnessConfig::default();
    let err = cli::resolve_repl_provider(&config, Some("ghost"), Some("m")).unwrap_err();
    assert!(format!("{err}").contains("ghost"));
}

#[test]
fn default_setup_interface_opens_before_provider_wizard() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    let mut input = Cursor::new(Vec::new());
    let mut output = Vec::new();

    let err = cli::run_default_setup_interface(
        &config_path,
        root.path(),
        "harness",
        &mut input,
        &mut output,
    )
    .unwrap_err();

    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("no provider configured"));
    assert!(output.contains("workspace:"));
    assert!(output.contains(root.path().to_string_lossy().as_ref()));
    assert!(output.contains("config:"));
    assert!(output.contains(config_path.to_string_lossy().as_ref()));
    assert!(output.contains("/provider add"));
    assert!(output.contains("[no provider] > "));
    assert!(!output.contains("Provider name: "));
    assert!(err.to_string().contains("interface closed"));
}

#[test]
fn default_setup_interface_adds_provider_from_interface_command() {
    let base_url = spawn_model_server(r#"{"data":[{"id":"deepseek-v4-pro"}]}"#);
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    let mut input =
        Cursor::new(format!("/provider add\ndeepseek\n{base_url}\nsk-local\n1\n").into_bytes());
    let mut output = Vec::new();

    let launch = cli::run_default_setup_interface(
        &config_path,
        root.path(),
        "harness",
        &mut input,
        &mut output,
    )
    .unwrap();

    assert_eq!(launch.config_path, config_path);
    assert_eq!(launch.workspace, root.path());
    assert_eq!(launch.provider_name, "deepseek");
    assert_eq!(launch.model, "deepseek-v4-pro");

    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("no provider configured"));
    assert!(output.contains("Provider setup"));
    assert!(output.contains("[no provider] > "));
    assert!(output.contains("/provider add"));
    assert!(output.contains("Provider name: "));
    assert!(output.contains("Base URL: "));
    assert!(output.contains("API key: "));
    assert!(output.contains("saved provider deepseek with"));

    let loaded = ConfigStore::new(&config_path).load().unwrap();
    let provider = loaded.provider("deepseek").unwrap();
    assert!(provider.models().contains(&"deepseek-v4-pro".to_string()));
    assert_eq!(provider.key_env(), Some("DEEPSEEK_API_KEY"));
}

#[test]
fn default_launch_selects_configured_provider_and_model_for_repl() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    ConfigStore::new(&config_path)
        .save_provider(
            ProviderConfig::new("deepseek", "https://api.deepseek.com/v1", "")
                .with_key_env("DEEPSEEK_API_KEY")
                .with_model("deepseek-chat"),
        )
        .unwrap();

    let launch = cli::resolve_default_launch(&config_path, root.path(), "harness").unwrap();

    match launch {
        cli::DefaultLaunch::Repl(options) => {
            assert_eq!(options.config_path, config_path);
            assert_eq!(options.workspace, root.path());
            assert_eq!(options.provider_name, "deepseek");
            assert_eq!(options.model, "deepseek-chat");
        }
        other => panic!("expected repl launch, got {other:?}"),
    }
}

fn assert_cli_usage_contains(args: &[&str], expected: &str) {
    let mut output = Vec::new();
    let err = cli::run(
        args.iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>(),
        &mut output,
    )
    .unwrap_err();

    assert!(
        err.to_string().contains(expected),
        "expected error to contain {expected:?}, got {err}"
    );
    assert!(output.is_empty());
}

#[test]
fn diagnostics_command_reports_binary_size_and_current_rss() {
    let root = tempfile::tempdir().unwrap();
    let binary_path = root.path().join("fake-bin.exe");
    std::fs::write(&binary_path, [1_u8, 2, 3, 4]).unwrap();
    let mut output = Vec::new();

    cli::run(
        vec![
            "harness-cli".to_string(),
            "diagnostics".to_string(),
            "--binary".to_string(),
            binary_path.display().to_string(),
            "--max-binary-bytes".to_string(),
            "10".to_string(),
        ],
        &mut output,
    )
    .unwrap();

    let result: Value = serde_json::from_slice(&output).unwrap();
    assert!(result["process"]["pid"].as_u64().unwrap() > 0);
    assert!(result["process"]["rss_bytes"].as_u64().unwrap() > 0);
    assert_eq!(result["binary"]["size_bytes"], 4);
    assert_eq!(result["binary"]["max_bytes"], 10);
    assert_eq!(result["binary"]["within_limit"], true);
    assert_eq!(result["limits"]["within_limits"], true);
}

#[test]
fn diagnostics_command_marks_binary_limit_exceeded_without_failing_json_output() {
    let root = tempfile::tempdir().unwrap();
    let binary_path = root.path().join("large-bin.exe");
    std::fs::write(&binary_path, [0_u8; 8]).unwrap();
    let mut output = Vec::new();

    cli::run(
        vec![
            "harness-cli".to_string(),
            "diagnostics".to_string(),
            "--binary".to_string(),
            binary_path.display().to_string(),
            "--max-binary-bytes".to_string(),
            "4".to_string(),
        ],
        &mut output,
    )
    .unwrap();

    let result: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(result["binary"]["size_bytes"], 8);
    assert_eq!(result["binary"]["max_bytes"], 4);
    assert_eq!(result["binary"]["within_limit"], false);
    assert_eq!(result["limits"]["within_limits"], false);
}

#[test]
fn provider_models_command_prints_add_all_and_sorted_models() {
    let base_url = spawn_model_server(r#"{"data":[{"id":"kimi-k2"},{"id":"glm-4.5"}]}"#);
    let mut output = Vec::new();

    cli::run(
        vec![
            "harness-cli".to_string(),
            "provider".to_string(),
            "models".to_string(),
            "--name".to_string(),
            "custom".to_string(),
            "--url".to_string(),
            base_url,
            "--key".to_string(),
            "sk-test".to_string(),
            "--timeout-ms".to_string(),
            "250".to_string(),
        ],
        &mut output,
    )
    .unwrap();

    let output = String::from_utf8(output).unwrap();
    assert_eq!(output, "Add all\nglm-4.5\nkimi-k2\n");
}

#[test]
fn provider_add_uses_verified_preset_without_url_and_model() {
    // The preset flow: pick a bench-verified provider, paste only the key.
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    let mut output = Vec::new();

    for (name, expected_url, expected_model) in [
        ("deepseek", "https://api.deepseek.com/v1", "deepseek-v4-pro"),
        ("glm", "https://api.z.ai/api/paas/v4", "glm-5.2"),
    ] {
        cli::run(
            vec![
                "harness-cli".to_string(),
                "provider".to_string(),
                "add".to_string(),
                "--config".to_string(),
                config_path.display().to_string(),
                "--name".to_string(),
                name.to_string(),
                "--key-env".to_string(),
                "TEST_KEY_ENV".to_string(),
            ],
            &mut output,
        )
        .unwrap();

        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        let provider = &saved["providers"][name];
        assert_eq!(provider["base_url"], expected_url, "{name}");
        assert_eq!(provider["models"][0], expected_model, "{name}");
    }
}

#[test]
fn provider_add_still_requires_url_for_unverified_families() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    let mut output = Vec::new();

    let err = cli::run(
        vec![
            "harness-cli".to_string(),
            "provider".to_string(),
            "add".to_string(),
            "--config".to_string(),
            config_path.display().to_string(),
            "--name".to_string(),
            "kimi".to_string(),
            "--key-env".to_string(),
            "KIMI_API_KEY".to_string(),
        ],
        &mut output,
    )
    .unwrap_err();

    assert!(err.to_string().contains("--url"), "{err}");
}

#[test]
fn provider_add_command_can_save_all_discovered_models() {
    let base_url = spawn_model_server(r#"{"data":[{"id":"qwen3-coder"},{"id":"glm-4.5"}]}"#);
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    let mut output = Vec::new();

    cli::run(
        vec![
            "harness-cli".to_string(),
            "provider".to_string(),
            "add".to_string(),
            "--config".to_string(),
            config_path.display().to_string(),
            "--name".to_string(),
            "local-openai".to_string(),
            "--url".to_string(),
            base_url,
            "--key".to_string(),
            "sk-local".to_string(),
            "--add-all".to_string(),
            "--timeout-ms".to_string(),
            "250".to_string(),
        ],
        &mut output,
    )
    .unwrap();

    let output = String::from_utf8(output).unwrap();
    assert_eq!(output, "saved provider local-openai with 2 models\n");

    let loaded = ConfigStore::new(config_path).load().unwrap();
    let provider = loaded.provider("local-openai").unwrap();
    assert_eq!(
        provider.models(),
        &["glm-4.5".to_string(), "qwen3-coder".to_string()]
    );
}

#[test]
fn provider_list_command_prints_saved_providers_without_api_keys() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    let store = ConfigStore::new(&config_path);
    store
        .save_provider(
            ProviderConfig::new("z-gateway", "https://api.example.com/v1", "sk-secret")
                .with_model("z-model")
                .with_auth_scheme(AuthScheme::Header {
                    name: "x-api-key".to_string(),
                })
                .with_chat_api(ChatApiFormat::OpenAiResponses)
                .with_cache_policy(CachePolicy::Disabled),
        )
        .unwrap();
    store
        .save_provider(
            ProviderConfig::new("alpha", "http://localhost:11434/v1", "")
                .with_key_env("ALPHA_API_KEY")
                .with_model("b-model")
                .with_model("a-model"),
        )
        .unwrap();
    let mut output = Vec::new();

    cli::run(
        vec![
            "harness-cli".to_string(),
            "provider".to_string(),
            "list".to_string(),
            "--config".to_string(),
            config_path.display().to_string(),
        ],
        &mut output,
    )
    .unwrap();

    let output = String::from_utf8(output).unwrap();
    assert_eq!(
        output,
        concat!(
            "alpha\tmodels=a-model,b-model\turl=http://localhost:11434/v1\tauth=bearer\tkey=env:ALPHA_API_KEY\tchat=openai-compatible\tcache=cache-header=X-Harness-Cache-Key\n",
            "z-gateway\tmodels=z-model\turl=https://api.example.com/v1\tauth=header:x-api-key\tkey=inline\tchat=openai-responses\tcache=cache=disabled\n"
        )
    );
    assert!(!output.contains("sk-secret"));
}

#[test]
fn provider_add_command_can_save_explicit_chat_api_format() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    let mut output = Vec::new();

    cli::run(
        vec![
            "harness-cli".to_string(),
            "provider".to_string(),
            "add".to_string(),
            "--config".to_string(),
            config_path.display().to_string(),
            "--name".to_string(),
            "openai-responses".to_string(),
            "--url".to_string(),
            "https://api.openai.com/v1".to_string(),
            "--key".to_string(),
            "sk-openai".to_string(),
            "--chat-api".to_string(),
            "openai-responses".to_string(),
            "--model".to_string(),
            "gpt-5".to_string(),
        ],
        &mut output,
    )
    .unwrap();

    let loaded = ConfigStore::new(config_path).load().unwrap();
    let provider = loaded.provider("openai-responses").unwrap();
    assert_eq!(provider.chat_api(), ChatApiFormat::OpenAiResponses);
}

#[test]
fn provider_add_command_can_save_openai_codex_responses_chat_api_format() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    let mut output = Vec::new();

    cli::run(
        vec![
            "harness-cli".to_string(),
            "provider".to_string(),
            "add".to_string(),
            "--config".to_string(),
            config_path.display().to_string(),
            "--name".to_string(),
            "codex".to_string(),
            "--url".to_string(),
            "https://api.openai.com/v1".to_string(),
            "--key-env".to_string(),
            "OPENAI_API_KEY".to_string(),
            "--chat-api".to_string(),
            "openai-codex-responses".to_string(),
            "--model".to_string(),
            "gpt-5-codex".to_string(),
        ],
        &mut output,
    )
    .unwrap();

    let loaded = ConfigStore::new(config_path).load().unwrap();
    let provider = loaded.provider("codex").unwrap();
    assert_eq!(provider.chat_api(), ChatApiFormat::OpenAiCodexResponses);
}

#[test]
fn provider_add_command_can_save_key_env_without_storing_api_key() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    let mut output = Vec::new();

    cli::run(
        vec![
            "harness-cli".to_string(),
            "provider".to_string(),
            "add".to_string(),
            "--config".to_string(),
            config_path.display().to_string(),
            "--name".to_string(),
            "env-provider".to_string(),
            "--url".to_string(),
            "https://api.example.com/v1".to_string(),
            "--key-env".to_string(),
            "ENV_PROVIDER_API_KEY".to_string(),
            "--model".to_string(),
            "env-model".to_string(),
        ],
        &mut output,
    )
    .unwrap();

    let loaded = ConfigStore::new(config_path).load().unwrap();
    let provider = loaded.provider("env-provider").unwrap();
    assert_eq!(provider.api_key(), "");
    assert_eq!(provider.key_env(), Some("ENV_PROVIDER_API_KEY"));
    assert_eq!(
        provider.auth_header_with_lookup(|name| {
            (name == "ENV_PROVIDER_API_KEY").then(|| "sk-env-provider".to_string())
        }),
        Some((
            "Authorization".to_string(),
            "Bearer sk-env-provider".to_string()
        ))
    );
}

#[test]
fn provider_add_command_can_save_automatic_cache_policy_metadata() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    let mut output = Vec::new();

    cli::run(
        vec![
            "harness-cli".to_string(),
            "provider".to_string(),
            "add".to_string(),
            "--config".to_string(),
            config_path.display().to_string(),
            "--name".to_string(),
            "cache-gateway".to_string(),
            "--url".to_string(),
            "https://api.example.com/v1".to_string(),
            "--key-env".to_string(),
            "CACHE_GATEWAY_API_KEY".to_string(),
            "--cache".to_string(),
            "automatic".to_string(),
            "--cache-hit-field".to_string(),
            "prompt_cache_hit_tokens".to_string(),
            "--cache-miss-field".to_string(),
            "prompt_cache_miss_tokens".to_string(),
            "--model".to_string(),
            "deepseek-v4-pro".to_string(),
        ],
        &mut output,
    )
    .unwrap();

    let loaded = ConfigStore::new(config_path).load().unwrap();
    let provider = loaded.provider("cache-gateway").unwrap();
    assert_eq!(
        provider.cache_policy(),
        CachePolicy::Automatic {
            hit_tokens_field: "prompt_cache_hit_tokens".to_string(),
            miss_tokens_field: "prompt_cache_miss_tokens".to_string(),
        }
    );
    assert_eq!(provider.cache_header("cache-key"), None);
}

#[test]
fn provider_add_command_can_discover_models_with_custom_auth_header() {
    let base_url = spawn_model_server_expect_header(
        "x-api-key",
        "sk-env-header",
        r#"{"data":[{"id":"header-model"}]}"#,
    );
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    let mut output = Vec::new();

    cli::run(
        vec![
            "harness-cli".to_string(),
            "provider".to_string(),
            "add".to_string(),
            "--config".to_string(),
            config_path.display().to_string(),
            "--name".to_string(),
            "header-provider".to_string(),
            "--url".to_string(),
            base_url,
            "--key".to_string(),
            "sk-env-header".to_string(),
            "--key-env".to_string(),
            "HEADER_PROVIDER_API_KEY".to_string(),
            "--auth".to_string(),
            "header".to_string(),
            "--auth-header".to_string(),
            "x-api-key".to_string(),
            "--add-all".to_string(),
        ],
        &mut output,
    )
    .unwrap();

    let loaded = ConfigStore::new(config_path).load().unwrap();
    let provider = loaded.provider("header-provider").unwrap();
    assert_eq!(
        provider.auth_scheme(),
        AuthScheme::Header {
            name: "x-api-key".to_string()
        }
    );
    assert_eq!(provider.models(), &["header-model".to_string()]);
}

#[test]
fn provider_add_interactive_prompts_models_and_saves_add_all_selection() {
    let base_url = spawn_model_server(r#"{"data":[{"id":"qwen3-coder"},{"id":"glm-4.5"}]}"#);
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    let mut input = Cursor::new(format!("local-openai\n{base_url}\nsk-local\n0\n").into_bytes());
    let mut output = Vec::new();

    cli::run_with_input(
        vec![
            "harness-cli".to_string(),
            "provider".to_string(),
            "add".to_string(),
            "--interactive".to_string(),
            "--config".to_string(),
            config_path.display().to_string(),
        ],
        &mut input,
        &mut output,
    )
    .unwrap();

    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("Provider name: "));
    assert!(output.contains("Base URL: "));
    assert!(output.contains("API key: "));
    assert!(output.contains("0) Add all"));
    assert!(output.contains("1) glm-4.5"));
    assert!(output.contains("2) qwen3-coder"));
    assert!(output.contains("saved provider local-openai with 2 models"));

    let loaded = ConfigStore::new(config_path).load().unwrap();
    let provider = loaded.provider("local-openai").unwrap();
    assert_eq!(
        provider.models(),
        &["glm-4.5".to_string(), "qwen3-coder".to_string()]
    );
}

#[test]
fn provider_add_interactive_uses_key_env_without_prompting_for_api_key() {
    let base_url = spawn_model_server(r#"{"data":[{"id":"env-model"}]}"#);
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    let mut input = Cursor::new(b"1\n".to_vec());
    let mut output = Vec::new();

    cli::run_with_input(
        vec![
            "harness-cli".to_string(),
            "provider".to_string(),
            "add".to_string(),
            "--interactive".to_string(),
            "--config".to_string(),
            config_path.display().to_string(),
            "--name".to_string(),
            "env-provider".to_string(),
            "--url".to_string(),
            base_url,
            "--key-env".to_string(),
            "ENV_PROVIDER_API_KEY".to_string(),
        ],
        &mut input,
        &mut output,
    )
    .unwrap();

    let output = String::from_utf8(output).unwrap();
    assert!(!output.contains("API key: "));
    assert!(output.contains("1) env-model"));

    let loaded = ConfigStore::new(config_path).load().unwrap();
    let provider = loaded.provider("env-provider").unwrap();
    assert_eq!(provider.api_key(), "");
    assert_eq!(provider.key_env(), Some("ENV_PROVIDER_API_KEY"));
    assert_eq!(provider.models(), &["env-model".to_string()]);
}

#[test]
fn provider_add_interactive_preserves_explicit_chat_api_format() {
    let base_url = spawn_model_server(r#"{"data":[{"id":"gpt-5"},{"id":"gpt-5-mini"}]}"#);
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    let mut input = Cursor::new(b"1\n".to_vec());
    let mut output = Vec::new();

    cli::run_with_input(
        vec![
            "harness-cli".to_string(),
            "provider".to_string(),
            "add".to_string(),
            "--interactive".to_string(),
            "--config".to_string(),
            config_path.display().to_string(),
            "--name".to_string(),
            "openai-responses".to_string(),
            "--url".to_string(),
            base_url,
            "--key".to_string(),
            "sk-openai".to_string(),
            "--chat-api".to_string(),
            "responses".to_string(),
        ],
        &mut input,
        &mut output,
    )
    .unwrap();

    let loaded = ConfigStore::new(config_path).load().unwrap();
    let provider = loaded.provider("openai-responses").unwrap();
    assert_eq!(provider.chat_api(), ChatApiFormat::OpenAiResponses);
    assert_eq!(provider.models(), &["gpt-5".to_string()]);
}

#[test]
fn provider_add_interactive_can_save_selected_model_by_number() {
    let base_url = spawn_model_server(r#"{"data":[{"id":"qwen3-coder"},{"id":"glm-4.5"}]}"#);
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    let mut input = Cursor::new(b"2\n".to_vec());
    let mut output = Vec::new();

    cli::run_with_input(
        vec![
            "harness-cli".to_string(),
            "provider".to_string(),
            "add".to_string(),
            "--interactive".to_string(),
            "--config".to_string(),
            config_path.display().to_string(),
            "--name".to_string(),
            "local-openai".to_string(),
            "--url".to_string(),
            base_url,
            "--key".to_string(),
            "sk-local".to_string(),
        ],
        &mut input,
        &mut output,
    )
    .unwrap();

    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("0) Add all"));
    assert!(output.contains("2) qwen3-coder"));
    assert!(output.contains("saved provider local-openai with 1 models"));

    let loaded = ConfigStore::new(config_path).load().unwrap();
    let provider = loaded.provider("local-openai").unwrap();
    assert_eq!(provider.models(), &["qwen3-coder".to_string()]);
}

#[test]
fn provider_add_command_applies_builtin_profile_metadata_by_name() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    let mut output = Vec::new();

    cli::run(
        vec![
            "harness-cli".to_string(),
            "provider".to_string(),
            "add".to_string(),
            "--config".to_string(),
            config_path.display().to_string(),
            "--name".to_string(),
            "claude".to_string(),
            "--url".to_string(),
            "https://api.anthropic.com/v1".to_string(),
            "--key".to_string(),
            "sk-anthropic".to_string(),
            "--model".to_string(),
            "claude-sonnet-4.5".to_string(),
        ],
        &mut output,
    )
    .unwrap();

    let loaded = ConfigStore::new(config_path).load().unwrap();
    let provider = loaded.provider("claude").unwrap();
    assert_eq!(
        provider.auth_scheme(),
        AuthScheme::Header {
            name: "x-api-key".to_string()
        }
    );
    assert_eq!(provider.chat_api(), ChatApiFormat::AnthropicMessages);
}

#[test]
fn provider_subscriptions_command_lists_builtin_profiles() {
    let mut output = Vec::new();

    cli::run(
        vec![
            "harness-cli".to_string(),
            "provider".to_string(),
            "subscriptions".to_string(),
        ],
        &mut output,
    )
    .unwrap();

    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("codex\tOPENAI_API_KEY"));
    assert!(output.contains("xiaomi\tXIAOMI_API_KEY"));
    assert!(output.contains("glm\tGLM_API_KEY"));
    assert!(output.contains("kimi\tKIMI_API_KEY"));
    assert!(output.contains("claude\tANTHROPIC_API_KEY"));
    assert!(output.contains("deepseek\tDEEPSEEK_API_KEY"));
    assert!(output.contains("cache=automatic(prompt_cache_hit_tokens,prompt_cache_miss_tokens)"));
}

#[test]
fn tool_call_command_executes_repaired_file_write_and_prints_json() {
    let root = tempfile::tempdir().unwrap();
    let mut output = Vec::new();

    cli::run(
        vec![
            "harness-cli".to_string(),
            "tool".to_string(),
            "call".to_string(),
            "--workspace".to_string(),
            root.path().display().to_string(),
            "write_file".to_string(),
            r#"{"file":"notes.txt","text":"from cli"}"#.to_string(),
        ],
        &mut output,
    )
    .unwrap();

    let result: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(result["tool_name"], "file.write");
    // `write_file` is the advertised prior-aligned name now — clean call.
    assert_eq!(result["repaired"], false);
    assert_eq!(
        std::fs::read_to_string(root.path().join("notes.txt")).unwrap(),
        "from cli"
    );
}

#[test]
fn tool_call_command_executes_repaired_file_append_and_prints_json() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("notes.txt"), "first\n").unwrap();
    let mut output = Vec::new();

    cli::run(
        vec![
            "harness-cli".to_string(),
            "tool".to_string(),
            "call".to_string(),
            "--workspace".to_string(),
            root.path().display().to_string(),
            "append_file".to_string(),
            r#"{"file":"notes.txt","text":"second\n"}"#.to_string(),
        ],
        &mut output,
    )
    .unwrap();

    let result: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(result["tool_name"], "file.append");
    // `append_file` is the advertised prior-aligned name now — clean call.
    assert_eq!(result["repaired"], false);
    assert_eq!(
        std::fs::read_to_string(root.path().join("notes.txt")).unwrap(),
        "first\nsecond\n"
    );
}

#[test]
fn tool_batch_command_executes_json_array_and_prints_ordered_results() {
    let root = tempfile::tempdir().unwrap();
    let mut output = Vec::new();

    cli::run(
        vec![
            "harness-cli".to_string(),
            "tool".to_string(),
            "batch".to_string(),
            "--workspace".to_string(),
            root.path().display().to_string(),
            "--max-concurrency".to_string(),
            "2".to_string(),
            r#"[
                {"id":"one","name":"write_file","arguments":{"file":"one.txt","text":"1"}},
                {"id":"bad","name":"missing_tool","arguments":{}},
                {"id":"two","name":"write_file","arguments":{"file":"two.txt","text":"2"}}
            ]"#
            .to_string(),
        ],
        &mut output,
    )
    .unwrap();

    let results: Value = serde_json::from_slice(&output).unwrap();
    let results = results.as_array().unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0]["id"], "one");
    assert_eq!(results[0]["ok"], true);
    assert_eq!(results[1]["id"], "bad");
    assert_eq!(results[1]["ok"], false);
    assert!(
        results[1]["error"]
            .as_str()
            .unwrap()
            .contains("unknown tool")
    );
    assert_eq!(results[2]["id"], "two");
    assert_eq!(results[2]["ok"], true);
    assert_eq!(
        std::fs::read_to_string(root.path().join("one.txt")).unwrap(),
        "1"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("two.txt")).unwrap(),
        "2"
    );
}

#[test]
fn tool_batch_command_accepts_timeout_ms_flag() {
    let root = tempfile::tempdir().unwrap();
    let mut output = Vec::new();

    cli::run(
        vec![
            "harness-cli".to_string(),
            "tool".to_string(),
            "batch".to_string(),
            "--workspace".to_string(),
            root.path().display().to_string(),
            "--timeout-ms".to_string(),
            "10".to_string(),
            "[]".to_string(),
        ],
        &mut output,
    )
    .unwrap();

    let results: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(results.as_array().unwrap().len(), 0);
}

#[test]
fn tool_batch_command_can_move_and_delete_files() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src").join("draft.txt"), "draft").unwrap();
    let mut output = Vec::new();

    cli::run(
        vec![
            "harness-cli".to_string(),
            "tool".to_string(),
            "batch".to_string(),
            "--workspace".to_string(),
            root.path().display().to_string(),
            "--max-concurrency".to_string(),
            "1".to_string(),
            r#"[
                {"id":"move","name":"rename_file","arguments":{"from":"src/draft.txt","to":"notes/final.txt"}},
                {"id":"delete","name":"remove_file","arguments":{"file":"notes/final.txt"}}
            ]"#
            .to_string(),
        ],
        &mut output,
    )
    .unwrap();

    let results: Value = serde_json::from_slice(&output).unwrap();
    let results = results.as_array().unwrap();
    assert_eq!(results[0]["tool_name"], "file.move");
    assert_eq!(results[0]["ok"], true);
    assert_eq!(results[1]["tool_name"], "file.delete");
    assert_eq!(results[1]["ok"], true);
    assert!(!root.path().join("src").join("draft.txt").exists());
    assert!(!root.path().join("notes").join("final.txt").exists());
}

#[test]
fn tool_call_command_can_run_file_search_tool() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src").join("notes.txt"), "alpha\nneedle\n").unwrap();
    let mut output = Vec::new();

    cli::run(
        vec![
            "harness-cli".to_string(),
            "tool".to_string(),
            "call".to_string(),
            "--workspace".to_string(),
            root.path().display().to_string(),
            "file.search".to_string(),
            r#"{"path":"src","query":"needle"}"#.to_string(),
        ],
        &mut output,
    )
    .unwrap();

    let result: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(result["tool_name"], "file.search");
    assert!(
        result["content"]
            .as_str()
            .unwrap()
            .contains("src/notes.txt:2:needle")
    );
}

#[test]
fn chat_once_command_loads_provider_and_prints_chat_response_json() {
    let base_url = spawn_chat_server();
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    ConfigStore::new(&config_path)
        .save_provider(
            harness_cli::providers::ProviderConfig::new("local", base_url, "sk-chat")
                .with_model("deepseek-v4-pro"),
        )
        .unwrap();
    let mut output = Vec::new();

    cli::run(
        vec![
            "harness-cli".to_string(),
            "chat".to_string(),
            "once".to_string(),
            "--config".to_string(),
            config_path.display().to_string(),
            "--provider".to_string(),
            "local".to_string(),
            "--model".to_string(),
            "deepseek-v4-pro".to_string(),
            "--message".to_string(),
            "hello".to_string(),
            "--timeout-ms".to_string(),
            "250".to_string(),
        ],
        &mut output,
    )
    .unwrap();

    let result: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(result["content"], "ok");
    assert_eq!(result["tool_calls"].as_array().unwrap().len(), 0);
    assert_eq!(result["cache"]["hit_tokens"], 5);
    assert_eq!(result["cache"]["miss_tokens"], 2);
    assert_eq!(result["cache"]["cacheable_prompt_tokens"], 7);
    assert_eq!(result["cache"]["hit_ratio_percent"], 71);
    assert_eq!(result["cache"]["saved_prompt_tokens"], 5);
}

#[test]
fn chat_stream_command_prints_openai_compatible_deltas_as_text() {
    let base_url = spawn_stream_chat_server();
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    ConfigStore::new(&config_path)
        .save_provider(
            harness_cli::providers::ProviderConfig::new("local", base_url, "sk-stream")
                .with_model("gpt-stream"),
        )
        .unwrap();
    let mut output = Vec::new();

    cli::run(
        vec![
            "harness-cli".to_string(),
            "chat".to_string(),
            "stream".to_string(),
            "--config".to_string(),
            config_path.display().to_string(),
            "--provider".to_string(),
            "local".to_string(),
            "--model".to_string(),
            "gpt-stream".to_string(),
            "--message".to_string(),
            "hello".to_string(),
            "--timeout-ms".to_string(),
            "250".to_string(),
        ],
        &mut output,
    )
    .unwrap();

    assert_eq!(String::from_utf8(output).unwrap(), "hello\n");
}

#[test]
fn agent_run_command_auto_saves_trace_under_harness_home() {
    let harness_home = isolate_harness_home();
    let base_url = spawn_agent_server();
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    ConfigStore::new(&config_path)
        .save_provider(
            harness_cli::providers::ProviderConfig::new("local", base_url, "sk-agent")
                .with_model("deepseek-v4-pro"),
        )
        .unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let mut output = Vec::new();

    cli::run(
        vec![
            "harness-cli".to_string(),
            "agent".to_string(),
            "run".to_string(),
            "--config".to_string(),
            config_path.display().to_string(),
            "--workspace".to_string(),
            workspace.display().to_string(),
            "--provider".to_string(),
            "local".to_string(),
            "--model".to_string(),
            "deepseek-v4-pro".to_string(),
            "--message".to_string(),
            "write notes".to_string(),
            "--timeout-ms".to_string(),
            "250".to_string(),
        ],
        &mut output,
    )
    .unwrap();

    // Without any --trace flag, the run's raw trace lands in the global store.
    let traces_dir = harness_home
        .join("projects")
        .join(harness_cli::session::workspace_slug(&workspace))
        .join("traces");
    let entries: Vec<PathBuf> = std::fs::read_dir(&traces_dir)
        .expect("traces dir should exist")
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(entries.len(), 1);
    let wrapper: Value =
        serde_json::from_str(&std::fs::read_to_string(&entries[0]).unwrap()).unwrap();
    assert_eq!(wrapper["turn"], 1);
    assert_eq!(wrapper["trace"]["provider"], "local");
    assert_eq!(wrapper["trace"]["events"][0]["type"], "model_tool_calls");
}

#[test]
fn agent_run_command_executes_tool_loop_and_prints_final_json() {
    isolate_harness_home();
    let base_url = spawn_agent_server();
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    ConfigStore::new(&config_path)
        .save_provider(
            harness_cli::providers::ProviderConfig::new("local", base_url, "sk-agent")
                .with_model("deepseek-v4-pro"),
        )
        .unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let mut output = Vec::new();

    cli::run(
        vec![
            "harness-cli".to_string(),
            "agent".to_string(),
            "run".to_string(),
            "--config".to_string(),
            config_path.display().to_string(),
            "--workspace".to_string(),
            workspace.display().to_string(),
            "--provider".to_string(),
            "local".to_string(),
            "--model".to_string(),
            "deepseek-v4-pro".to_string(),
            "--message".to_string(),
            "write notes".to_string(),
            "--timeout-ms".to_string(),
            "250".to_string(),
        ],
        &mut output,
    )
    .unwrap();

    let result: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(result["final_content"], "finished");
    assert_eq!(result["tool_rounds"], 1);
    assert_eq!(result["tool_results"][0]["repaired"], false);
    assert_eq!(
        std::fs::read_to_string(workspace.join("notes.txt")).unwrap(),
        "agent cli"
    );
}

#[test]
fn agent_run_command_writes_full_trace_and_tool_error_report() {
    isolate_harness_home();
    let base_url = spawn_tool_error_agent_server();
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    ConfigStore::new(&config_path)
        .save_provider(
            harness_cli::providers::ProviderConfig::new("local", base_url, "sk-agent")
                .with_model("deepseek-v4-pro"),
        )
        .unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let trace_path = root.path().join("trace.json");
    let errors_path = root.path().join("tool-errors.json");
    let mut output = Vec::new();

    cli::run(
        vec![
            "harness-cli".to_string(),
            "agent".to_string(),
            "run".to_string(),
            "--config".to_string(),
            config_path.display().to_string(),
            "--workspace".to_string(),
            workspace.display().to_string(),
            "--provider".to_string(),
            "local".to_string(),
            "--model".to_string(),
            "deepseek-v4-pro".to_string(),
            "--message".to_string(),
            "try a bad tool then recover".to_string(),
            "--timeout-ms".to_string(),
            "250".to_string(),
            "--trace".to_string(),
            trace_path.display().to_string(),
            "--tool-errors".to_string(),
            errors_path.display().to_string(),
        ],
        &mut output,
    )
    .unwrap();

    let result: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(result["final_content"], "recovered");

    let trace: Value = serde_json::from_slice(&std::fs::read(&trace_path).unwrap()).unwrap();
    assert_eq!(trace["provider"], "local");
    assert_eq!(trace["model"], "deepseek-v4-pro");
    assert_eq!(trace["events"][0]["type"], "model_tool_calls");
    assert_eq!(trace["events"][0]["calls"][0]["id"], "call-bad");
    assert_eq!(trace["events"][0]["calls"][0]["name"], "missing_tool");
    assert_eq!(trace["events"][1]["type"], "tool_result");
    assert_eq!(trace["events"][1]["result"]["ok"], false);

    let errors: Value = serde_json::from_slice(&std::fs::read(&errors_path).unwrap()).unwrap();
    assert_eq!(errors.as_array().unwrap().len(), 1);
    assert_eq!(errors[0]["id"], "call-bad");
    assert_eq!(errors[0]["tool_name"], "missing_tool");
    assert!(
        errors[0]["error"]
            .as_str()
            .unwrap()
            .contains("unknown tool")
    );
}

#[test]
fn agent_run_command_accepts_max_rounds_limit() {
    isolate_harness_home();
    let base_url = spawn_looping_agent_server();
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    ConfigStore::new(&config_path)
        .save_provider(
            harness_cli::providers::ProviderConfig::new("local", base_url, "sk-agent")
                .with_model("deepseek-v4-pro"),
        )
        .unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let trace_path = root.path().join("max-rounds-trace.json");
    let errors_path = root.path().join("max-rounds-tool-errors.json");
    let mut output = Vec::new();

    let err = cli::run(
        vec![
            "harness-cli".to_string(),
            "agent".to_string(),
            "run".to_string(),
            "--config".to_string(),
            config_path.display().to_string(),
            "--workspace".to_string(),
            workspace.display().to_string(),
            "--provider".to_string(),
            "local".to_string(),
            "--model".to_string(),
            "deepseek-v4-pro".to_string(),
            "--message".to_string(),
            "loop tools".to_string(),
            "--timeout-ms".to_string(),
            "250".to_string(),
            "--max-rounds".to_string(),
            "1".to_string(),
            "--trace".to_string(),
            trace_path.display().to_string(),
            "--tool-errors".to_string(),
            errors_path.display().to_string(),
        ],
        &mut output,
    )
    .unwrap_err();

    assert!(err.to_string().contains("maximum tool rounds exceeded: 1"));
    let trace: Value = serde_json::from_slice(&std::fs::read(&trace_path).unwrap()).unwrap();
    assert_eq!(trace["events"][0]["type"], "model_tool_calls");
    assert_eq!(trace["events"][1]["type"], "tool_result");
    assert_eq!(trace["events"][2]["type"], "model_tool_calls");
    assert_eq!(trace["events"][3]["type"], "error");
    assert_eq!(
        trace["events"][3]["message"],
        "maximum tool rounds exceeded: 1"
    );
    let errors: Value = serde_json::from_slice(&std::fs::read(&errors_path).unwrap()).unwrap();
    assert_eq!(errors.as_array().unwrap().len(), 0);
}

#[test]
fn agent_run_command_accepts_max_tool_concurrency_limit() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let mut output = Vec::new();

    let err = cli::run(
        vec![
            "harness-cli".to_string(),
            "agent".to_string(),
            "run".to_string(),
            "--config".to_string(),
            config_path.display().to_string(),
            "--workspace".to_string(),
            workspace.display().to_string(),
            "--provider".to_string(),
            "local".to_string(),
            "--model".to_string(),
            "deepseek-v4-pro".to_string(),
            "--message".to_string(),
            "loop tools".to_string(),
            "--max-tool-concurrency".to_string(),
            "1".to_string(),
        ],
        &mut output,
    )
    .unwrap_err();

    assert!(err.to_string().contains("unknown provider: local"));
}

#[test]
fn agent_run_command_accepts_tool_timeout_limit() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let mut output = Vec::new();

    let err = cli::run(
        vec![
            "harness-cli".to_string(),
            "agent".to_string(),
            "run".to_string(),
            "--config".to_string(),
            config_path.display().to_string(),
            "--workspace".to_string(),
            workspace.display().to_string(),
            "--provider".to_string(),
            "local".to_string(),
            "--model".to_string(),
            "deepseek-v4-pro".to_string(),
            "--message".to_string(),
            "loop tools".to_string(),
            "--tool-timeout-ms".to_string(),
            "50".to_string(),
        ],
        &mut output,
    )
    .unwrap_err();

    assert!(err.to_string().contains("unknown provider: local"));
}

#[test]
fn agent_run_command_rejects_zero_resource_limits() {
    for (flag, expected) in [
        ("--max-rounds", "--max-rounds must be a positive integer"),
        (
            "--max-tool-concurrency",
            "--max-tool-concurrency must be a positive integer",
        ),
        (
            "--tool-timeout-ms",
            "--tool-timeout-ms must be a positive integer",
        ),
    ] {
        assert_cli_usage_contains(
            &[
                "harness-cli",
                "agent",
                "run",
                "--provider",
                "local",
                "--model",
                "deepseek-v4-pro",
                "--message",
                "loop tools",
                flag,
                "0",
            ],
            expected,
        );
    }
}

#[test]
fn clipboard_paste_command_can_capture_supplied_text_for_terminal_paste_flow() {
    let root = tempfile::tempdir().unwrap();
    let mut output = Vec::new();

    cli::run(
        vec![
            "harness-cli".to_string(),
            "clipboard".to_string(),
            "paste".to_string(),
            "--workspace".to_string(),
            root.path().display().to_string(),
            "--text".to_string(),
            "pasted text".to_string(),
        ],
        &mut output,
    )
    .unwrap();

    let result: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(result["kind"], "text");
    assert_eq!(result["mime_type"], "text/plain; charset=utf-8");
    assert!(
        result["prompt_fragment"]
            .as_str()
            .unwrap()
            .contains("pasted text")
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join(result["relative_path"].as_str().unwrap()))
            .unwrap(),
        "pasted text"
    );
}

#[test]
fn help_lists_interactive_repl_command() {
    let mut output = Vec::new();
    let err = cli::run(
        vec!["harness-cli".to_string(), "unknown".to_string()],
        &mut output,
    )
    .unwrap_err();

    assert!(
        err.to_string()
            .contains("harness-cli repl --config PATH --workspace PATH --provider NAME --model MODEL [--timeout-ms N] [--max-rounds N] [--max-tool-concurrency N] [--tool-timeout-ms N]")
    );
}

#[test]
fn repl_command_accepts_max_rounds_limit() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    let mut output = Vec::new();

    let err = cli::run(
        vec![
            "harness-cli".to_string(),
            "repl".to_string(),
            "--config".to_string(),
            config_path.display().to_string(),
            "--workspace".to_string(),
            root.path().display().to_string(),
            "--provider".to_string(),
            "local".to_string(),
            "--model".to_string(),
            "deepseek-v4-pro".to_string(),
            "--max-rounds".to_string(),
            "1".to_string(),
        ],
        &mut output,
    )
    .unwrap_err();

    assert!(err.to_string().contains("unknown provider: local"));
}

#[test]
fn repl_command_accepts_timeout_ms_limit() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    let mut output = Vec::new();

    let err = cli::run(
        vec![
            "harness-cli".to_string(),
            "repl".to_string(),
            "--config".to_string(),
            config_path.display().to_string(),
            "--workspace".to_string(),
            root.path().display().to_string(),
            "--provider".to_string(),
            "local".to_string(),
            "--model".to_string(),
            "deepseek-v4-pro".to_string(),
            "--timeout-ms".to_string(),
            "250".to_string(),
        ],
        &mut output,
    )
    .unwrap_err();

    assert!(err.to_string().contains("unknown provider: local"));
}

#[test]
fn repl_command_accepts_max_tool_concurrency_limit() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    let mut output = Vec::new();

    let err = cli::run(
        vec![
            "harness-cli".to_string(),
            "repl".to_string(),
            "--config".to_string(),
            config_path.display().to_string(),
            "--workspace".to_string(),
            root.path().display().to_string(),
            "--provider".to_string(),
            "local".to_string(),
            "--model".to_string(),
            "deepseek-v4-pro".to_string(),
            "--max-tool-concurrency".to_string(),
            "1".to_string(),
        ],
        &mut output,
    )
    .unwrap_err();

    assert!(err.to_string().contains("unknown provider: local"));
}

#[test]
fn repl_command_accepts_tool_timeout_limit() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    let mut output = Vec::new();

    let err = cli::run(
        vec![
            "harness-cli".to_string(),
            "repl".to_string(),
            "--config".to_string(),
            config_path.display().to_string(),
            "--workspace".to_string(),
            root.path().display().to_string(),
            "--provider".to_string(),
            "local".to_string(),
            "--model".to_string(),
            "deepseek-v4-pro".to_string(),
            "--tool-timeout-ms".to_string(),
            "50".to_string(),
        ],
        &mut output,
    )
    .unwrap_err();

    assert!(err.to_string().contains("unknown provider: local"));
}

#[test]
fn repl_command_rejects_zero_resource_limits() {
    for (flag, expected) in [
        ("--max-rounds", "--max-rounds must be a positive integer"),
        (
            "--max-tool-concurrency",
            "--max-tool-concurrency must be a positive integer",
        ),
        (
            "--tool-timeout-ms",
            "--tool-timeout-ms must be a positive integer",
        ),
    ] {
        assert_cli_usage_contains(
            &[
                "harness-cli",
                "repl",
                "--provider",
                "local",
                "--model",
                "deepseek-v4-pro",
                flag,
                "0",
            ],
            expected,
        );
    }
}

fn spawn_model_server(body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buffer = [0; 2048];
        let read = stream.read(&mut buffer).unwrap();
        let request = String::from_utf8_lossy(&buffer[..read]);
        assert!(request.starts_with("GET /v1/models "));

        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).unwrap();
    });

    format!("http://{addr}/v1")
}

fn spawn_model_server_expect_header(
    header: &'static str,
    value: &'static str,
    body: &'static str,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buffer = [0; 2048];
        let read = stream.read(&mut buffer).unwrap();
        let request = String::from_utf8_lossy(&buffer[..read]);
        assert!(request.starts_with("GET /v1/models "));
        assert!(request.to_ascii_lowercase().contains(&format!(
            "{}: {}",
            header.to_ascii_lowercase(),
            value
        )));

        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).unwrap();
    });

    format!("http://{addr}/v1")
}

fn spawn_chat_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        let request_lower = request.to_ascii_lowercase();
        assert!(request.starts_with("POST /v1/chat/completions "));
        assert!(request_lower.contains("authorization: bearer sk-chat"));

        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let json: Value = serde_json::from_str(body).unwrap();
        assert_eq!(json["model"], "deepseek-v4-pro");
        assert_eq!(json["messages"][0]["role"], "system");
        assert_eq!(json["messages"][1]["content"], "hello");
        assert!(json["tools"].as_array().unwrap().len() >= 3);

        let response_body = r#"{
            "choices": [{"message": {"role": "assistant", "content": "ok"}}],
            "usage": {
                "prompt_tokens": 7,
                "completion_tokens": 1,
                "total_tokens": 8,
                "prompt_cache_hit_tokens": 5,
                "prompt_cache_miss_tokens": 2
            }
        }"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        stream.write_all(response.as_bytes()).unwrap();
    });

    format!("http://{addr}/v1")
}

fn spawn_stream_chat_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        let request_lower = request.to_ascii_lowercase();
        assert!(request.starts_with("POST /v1/chat/completions "));
        assert!(request_lower.contains("authorization: bearer sk-stream"));

        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let json: Value = serde_json::from_str(body).unwrap();
        assert_eq!(json["model"], "gpt-stream");
        assert_eq!(json["messages"][1]["content"], "hello");
        assert_eq!(json["stream"], true);

        let response_body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"he\"},\"index\":0}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"llo\"},\"index\":0}]}\n\n",
            "data: [DONE]\n\n",
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        stream.write_all(response.as_bytes()).unwrap();
    });

    format!("http://{addr}/v1")
}

fn spawn_agent_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    thread::spawn(move || {
        let (mut first_stream, _) = listener.accept().unwrap();
        let first = read_http_request(&mut first_stream);
        let first_body: Value =
            serde_json::from_str(first.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(first_body["messages"][1]["content"], "write notes");
        respond(
            &mut first_stream,
            r#"{
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call-cli",
                            "type": "function",
                            "function": {
                                "name": "write_file",
                                "arguments": "{\"file\":\"notes.txt\",\"text\":\"agent cli\"}"
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
        assert_eq!(second_body["messages"][3]["role"], "tool");
        assert_eq!(second_body["messages"][3]["tool_call_id"], "call-cli");
        respond(
            &mut second_stream,
            r#"{
                "choices": [{"message": {"role": "assistant", "content": "finished"}}]
            }"#,
        );
    });

    format!("http://{addr}/v1")
}

fn spawn_tool_error_agent_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    thread::spawn(move || {
        let (mut first_stream, _) = listener.accept().unwrap();
        let first = read_http_request(&mut first_stream);
        let first_body: Value =
            serde_json::from_str(first.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(
            first_body["messages"][1]["content"],
            "try a bad tool then recover"
        );
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

        let (mut second_stream, _) = listener.accept().unwrap();
        let second = read_http_request(&mut second_stream);
        let second_body: Value =
            serde_json::from_str(second.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(second_body["messages"][3]["role"], "tool");
        assert_eq!(second_body["messages"][3]["tool_call_id"], "call-bad");
        let tool_result: Value =
            serde_json::from_str(second_body["messages"][3]["content"].as_str().unwrap()).unwrap();
        assert_eq!(tool_result["ok"], false);
        respond(
            &mut second_stream,
            r#"{
                "choices": [{"message": {"role": "assistant", "content": "recovered"}}]
            }"#,
        );
    });

    format!("http://{addr}/v1")
}

fn spawn_looping_agent_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    thread::spawn(move || {
        let (mut first_stream, _) = listener.accept().unwrap();
        let first = read_http_request(&mut first_stream);
        let first_body: Value =
            serde_json::from_str(first.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(first_body["messages"][1]["content"], "loop tools");
        respond(
            &mut first_stream,
            r#"{
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call-loop-1",
                            "type": "function",
                            "function": {
                                "name": "write_file",
                                "arguments": "{\"file\":\"loop.txt\",\"text\":\"first\"}"
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
        assert_eq!(second_body["messages"][3]["role"], "tool");
        respond(
            &mut second_stream,
            r#"{
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call-loop-2",
                            "type": "function",
                            "function": {
                                "name": "write_file",
                                "arguments": "{\"file\":\"loop.txt\",\"text\":\"second\"}"
                            }
                        }]
                    }
                }]
            }"#,
        );
    });

    format!("http://{addr}/v1")
}

fn respond(stream: &mut TcpStream, body: &str) {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).unwrap();
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

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}
