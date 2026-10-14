use std::path::PathBuf;

use harness_cli::cli;
use harness_cli::providers::{
    CachePolicy, ChatApiFormat, ModelDiscovery, ProviderConfig, provider_from_profile,
};
use harness_cli::tui::{TuiAction, TuiApp, TuiCommand, setup_lines};
use harness_tui::input::{KeyCode, KeyEvent};
use harness_tui::text::Line;

#[test]
fn setup_tui_accepts_provider_add_command() {
    let mut app = setup_app();

    type_text_tui(&mut app, "/provider add");
    let action = app.handle_key(key(KeyCode::Enter));

    assert_eq!(action, TuiAction::Command(TuiCommand::ProviderAdd));
    assert_eq!(app.input(), "");
    assert!(app.status_message().contains("Provider setup"));
}

#[test]
fn setup_tui_renders_status_paths_commands_and_prompt() {
    let mut app = setup_app();
    type_text_tui(&mut app, "/providers");

    let screen = lines_text(&setup_lines(&app, 96));
    assert!(screen.contains("harness"));
    assert!(screen.contains("no provider configured"));
    assert!(screen.contains("workspace"));
    assert!(screen.contains("C:/work/project"));
    assert!(screen.contains("config"));
    assert!(screen.contains("C:/config/providers.json"));
    assert!(screen.contains("/provider add"));
    assert!(screen.contains("[no provider]"));
    assert!(screen.contains("/providers"));
}

#[test]
fn tui_provider_add_fetches_models_and_saves_the_chosen_one() {
    let mut app = setup_app();

    type_text_tui(&mut app, "/provider add");
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        TuiAction::Command(TuiCommand::ProviderAdd)
    );
    assert!(
        app.dialog_title()
            .is_some_and(|title| title.contains("Provider"))
    );

    for _ in 0..5 {
        app.handle_key(key(KeyCode::Down)); // codex -> ... -> deepseek
    }
    app.handle_key(key(KeyCode::Enter));
    type_text_tui(&mut app, "https://api.deepseek.com/v1");
    app.handle_key(key(KeyCode::Enter));
    type_text_tui(&mut app, "sk-test");
    let TuiAction::FetchModels(provider) = app.handle_key(key(KeyCode::Enter)) else {
        panic!("the key step must ask for the model list");
    };
    assert_eq!(provider.name(), "deepseek");
    assert_eq!(provider.base_url(), "https://api.deepseek.com/v1");
    assert_eq!(provider.api_key(), "sk-test");
    assert!(lines_text(&setup_lines(&app, 100)).contains("Fetching models"));

    app.models_loaded(Ok(discovered(
        provider,
        r#"{"data":[{"id":"deepseek-v4-flash"},{"id":"deepseek-v4-pro","context_length":131072}]}"#,
    )));
    let screen = lines_text(&setup_lines(&app, 100));
    assert!(screen.contains("Add all (2 models)"), "{screen}");
    assert!(screen.contains("131k ctx"), "{screen}");

    // The cursor starts on the family's bench-verified model.
    let TuiAction::SaveProvider { provider, model } = app.handle_key(key(KeyCode::Enter)) else {
        panic!("Enter on a model must save");
    };
    assert_eq!(model, "deepseek-v4-pro");
    assert_eq!(provider.models(), &["deepseek-v4-pro".to_string()]);
    assert_eq!(
        provider
            .model_info("deepseek-v4-pro")
            .and_then(|info| info.context_window),
        Some(131_072)
    );
    assert!(app.dialog_title().is_none());
}

#[test]
fn wizard_defaults_come_from_builtin_profiles() {
    let mut app = setup_app();

    type_text_tui(&mut app, "/provider add");
    app.handle_key(key(KeyCode::Enter)); // open wizard
    app.handle_key(key(KeyCode::Down)); // codex -> xiaomi
    app.handle_key(key(KeyCode::Down)); // xiaomi -> glm
    app.handle_key(key(KeyCode::Enter)); // pick glm -> base URL step
    app.handle_key(key(KeyCode::Enter)); // accept default base URL -> API key step
    type_text_tui(&mut app, "sk-test");
    let action = app.handle_key(key(KeyCode::Enter));

    // The wizard must offer the canonical profile data (providers.rs), not a
    // private copy — the copy had already drifted to open.bigmodel.cn/glm-4.5.
    let TuiAction::FetchModels(provider) = action else {
        panic!("expected a model list request, got {action:?}");
    };
    assert_eq!(provider.name(), "glm");
    assert_eq!(provider.base_url(), "https://api.z.ai/api/paas/v4");
}

#[test]
fn tui_paste_fills_api_key_field_in_wizard_and_strips_newline() {
    let mut app = setup_app();

    type_text_tui(&mut app, "/provider add");
    app.handle_key(key(KeyCode::Enter)); // open wizard on the provider step
    for _ in 0..5 {
        app.handle_key(key(KeyCode::Down)); // codex -> ... -> deepseek
    }
    app.handle_key(key(KeyCode::Enter)); // pick deepseek -> base URL step
    app.handle_key(key(KeyCode::Enter)); // accept default base URL -> API key step

    // Clipboards routinely include a trailing newline. A bracketed paste must
    // land in the active field without that newline submitting or corrupting it.
    app.handle_paste("sk-pasted-secret-123\n");
    let action = app.handle_key(key(KeyCode::Enter));

    let TuiAction::FetchModels(provider) = action else {
        panic!("expected a model list request, got {action:?}");
    };
    assert_eq!(provider.api_key(), "sk-pasted-secret-123");
}

#[test]
fn custom_provider_takes_a_name_and_add_all_keeps_every_model_with_its_info() {
    let mut app = setup_app();
    type_text_tui(&mut app, "/provider add");
    app.handle_key(key(KeyCode::Enter));
    app.handle_key(key(KeyCode::Up)); // wraps to the last entry: custom
    assert!(lines_text(&setup_lines(&app, 100)).contains("› custom"));
    app.handle_key(key(KeyCode::Enter));
    type_text_tui(&mut app, "openrouter");
    app.handle_key(key(KeyCode::Enter));
    type_text_tui(&mut app, "https://openrouter.ai/api/v1");
    app.handle_key(key(KeyCode::Enter));
    type_text_tui(&mut app, "sk-or");
    let TuiAction::FetchModels(provider) = app.handle_key(key(KeyCode::Enter)) else {
        panic!("expected a model list request");
    };
    assert_eq!(provider.name(), "openrouter");
    assert_eq!(provider.key_env(), None);

    app.models_loaded(Ok(discovered(
        provider,
        r#"{"data":[
            {"id":"a/one","context_length":1000000,"pricing":{"prompt":"0.000001","completion":"0.000002"}},
            {"id":"b/two"}]}"#,
    )));
    assert!(lines_text(&setup_lines(&app, 100)).contains("1M ctx · $1/$2 per Mtok"));

    // The cursor starts on "Add all" for a custom provider.
    let TuiAction::SaveProvider { provider, model } = app.handle_key(key(KeyCode::Enter)) else {
        panic!("expected save");
    };
    assert_eq!(
        provider.models(),
        &["a/one".to_string(), "b/two".to_string()]
    );
    assert_eq!(model, "a/one");
    assert_eq!(
        provider
            .pricing("a/one")
            .map(|pricing| pricing.output_per_mtok),
        Some(2.0)
    );
    assert!(provider.model_info("b/two").is_none());
}

#[test]
fn model_filter_narrows_the_list_and_an_unknown_id_is_saved_as_typed() {
    let mut app = open_custom_wizard_at_models(
        r#"{"data":[{"id":"gpt-x"},{"id":"kimi-k3"},{"id":"kimi-k3-thinking"}]}"#,
    );

    type_text_tui(&mut app, "k3-th");
    let screen = lines_text(&setup_lines(&app, 100));
    assert!(screen.contains("kimi-k3-thinking"), "{screen}");
    assert!(!screen.contains("gpt-x"), "{screen}");
    assert!(!screen.contains("Add all"), "{screen}");
    let TuiAction::SaveProvider { model, .. } = app.handle_key(key(KeyCode::Enter)) else {
        panic!("expected save");
    };
    assert_eq!(model, "kimi-k3-thinking");

    let mut app = open_custom_wizard_at_models(r#"{"data":[{"id":"gpt-x"}]}"#);
    type_text_tui(&mut app, "private-model");
    let TuiAction::SaveProvider { provider, model } = app.handle_key(key(KeyCode::Enter)) else {
        panic!("expected save");
    };
    assert_eq!(model, "private-model");
    assert_eq!(provider.models(), &["private-model".to_string()]);
}

#[test]
fn failed_fetch_offers_retry_and_manual_model_id() {
    let mut app = setup_app();
    type_text_tui(&mut app, "/provider add");
    app.handle_key(key(KeyCode::Enter));
    app.handle_key(key(KeyCode::Up)); // custom
    app.handle_key(key(KeyCode::Enter));
    type_text_tui(&mut app, "local");
    app.handle_key(key(KeyCode::Enter));
    type_text_tui(&mut app, "http://localhost:9/v1");
    app.handle_key(key(KeyCode::Enter));
    type_text_tui(&mut app, "sk");
    app.handle_key(key(KeyCode::Enter));

    app.models_loaded(Err("connection refused".to_string()));
    let screen = lines_text(&setup_lines(&app, 100));
    assert!(
        screen.contains("Could not fetch models: connection refused"),
        "{screen}"
    );

    // Enter goes back to the URL with both values kept: Enter, Enter retries.
    app.handle_key(key(KeyCode::Enter));
    app.handle_key(key(KeyCode::Enter));
    let TuiAction::FetchModels(provider) = app.handle_key(key(KeyCode::Enter)) else {
        panic!("expected a retry");
    };
    assert_eq!(provider.base_url(), "http://localhost:9/v1");
    assert_eq!(provider.api_key(), "sk");

    // Tab skips the list: the model id is typed by hand.
    app.models_loaded(Err("still down".to_string()));
    app.handle_key(key(KeyCode::Tab));
    type_text_tui(&mut app, "llama-3");
    let TuiAction::SaveProvider { provider, model } = app.handle_key(key(KeyCode::Enter)) else {
        panic!("expected save");
    };
    assert_eq!(model, "llama-3");
    assert_eq!(provider.name(), "local");
    assert_eq!(provider.base_url(), "http://localhost:9/v1");
}

#[test]
fn setup_tui_esc_arms_then_second_esc_exits() {
    let mut app = setup_app();
    assert_eq!(app.handle_key(key(KeyCode::Esc)), TuiAction::Continue);
    assert!(
        app.status_message().contains("Press Esc again"),
        "{}",
        app.status_message()
    );
    // Any other key disarms the confirmation…
    app.handle_key(key(KeyCode::Char('x')));
    assert_eq!(app.handle_key(key(KeyCode::Esc)), TuiAction::Continue);
    // …and a straight double Esc exits.
    assert_eq!(app.handle_key(key(KeyCode::Esc)), TuiAction::Exit);
}

#[test]
fn tui_esc_closes_the_wizard_instead_of_exiting_harness() {
    let mut app = TuiApp::new(
        "harness",
        PathBuf::from("C:/config/providers.json"),
        PathBuf::from("C:/work/project"),
    );
    type_text_tui(&mut app, "/provider add");
    app.handle_key(key(KeyCode::Enter));
    app.handle_key(key(KeyCode::Enter)); // provider chosen -> base URL step

    // Esc inside the wizard: dialog closes, the program keeps running.
    assert_eq!(app.handle_key(key(KeyCode::Esc)), TuiAction::Continue);
    assert!(app.dialog_title().is_none());

    // At the top level Esc is two-step: arm, then exit.
    assert_eq!(app.handle_key(key(KeyCode::Esc)), TuiAction::Continue);
    assert!(
        app.status_message().contains("Press Esc again"),
        "{}",
        app.status_message()
    );
    assert_eq!(app.handle_key(key(KeyCode::Esc)), TuiAction::Exit);
}

#[test]
fn terminal_setup_enables_bracketed_paste_to_protect_the_terminal() {
    let read = |path: &str| {
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path))
            .unwrap()
    };

    // The chat TUI runs on harness-tui: `tui_terminal::raw_mode` switches the
    // terminal into raw mode with bracketed paste enabled, and pastes arrive
    // as `TuiEvent::Paste` instead of a burst of key events.
    let repl = read("src/repl.rs");
    assert!(
        repl.contains("raw_mode"),
        "src/repl.rs must enter raw mode via harness-tui so bracketed paste is enabled"
    );
    assert!(
        repl.contains("Paste"),
        "src/repl.rs must handle paste events"
    );

    // The setup TUI also runs on harness-tui: `Screen::stdout` opens the
    // terminal with bracketed paste enabled as part of its setup sequence.
    let tui = read("src/tui.rs");
    assert!(
        tui.contains("Screen::stdout"),
        "src/tui.rs (setup TUI) must open the terminal via harness-tui, which enables bracketed paste"
    );

    // harness-tui owns the escape sequence that turns bracketed paste on.
    let terminal = read("crates/harness-tui/src/terminal.rs");
    assert!(
        terminal.contains("BRACKETED_PASTE_ON"),
        "harness-tui terminal must emit the bracketed-paste-on escape sequence"
    );
}

#[test]
fn tui_renders_provider_wizard_dialog_inside_interface() {
    let mut app = TuiApp::new(
        "harness",
        PathBuf::from("C:/config/providers.json"),
        PathBuf::from("C:/work/project"),
    );

    type_text_tui(&mut app, "/provider add");
    app.handle_key(key(KeyCode::Enter));

    let screen = lines_text(&setup_lines(&app, 100));
    assert!(screen.contains("Provider setup"));
    assert!(screen.contains("Select provider"));
    assert!(screen.contains("codex"));
    assert!(screen.contains("deepseek"));
    assert!(screen.contains("Enter accept"));
    assert!(screen.contains("Esc close"));
}

#[test]
fn provider_from_profile_uses_builtin_profile_metadata() {
    let provider = provider_from_profile(
        "deepseek".to_string(),
        "https://api.deepseek.com/v1".to_string(),
        "sk-test".to_string(),
    );

    assert_eq!(provider.name(), "deepseek");
    assert_eq!(provider.base_url(), "https://api.deepseek.com/v1");
    assert_eq!(provider.api_key(), "sk-test");
    assert_eq!(provider.key_env(), Some("DEEPSEEK_API_KEY"));
    assert_eq!(provider.chat_api(), ChatApiFormat::OpenAiCompatible);
    assert_eq!(
        provider.cache_policy(),
        CachePolicy::Automatic {
            hit_tokens_field: "prompt_cache_hit_tokens".to_string(),
            miss_tokens_field: "prompt_cache_miss_tokens".to_string(),
        }
    );
}

#[test]
fn cli_finishes_setup_from_tui_save_provider_action() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    let mut output = Vec::new();
    let (provider, discovery) = discovered(
        ProviderConfig::new("deepseek", "https://api.deepseek.com/v1", "sk-test"),
        r#"{"data":[{"id":"deepseek-v4-flash"},{"id":"deepseek-v4-pro","context_length":131072}]}"#,
    );
    let models = discovery.add_all_model_ids();

    let launch = cli::finish_tui_setup_action(
        TuiAction::SaveProvider {
            provider: provider.with_discovered_models(&discovery, &models),
            model: "deepseek-v4-pro".to_string(),
        },
        &config_path,
        root.path(),
        "harness",
        &mut output,
    )
    .unwrap();

    assert_eq!(launch.config_path, config_path);
    assert_eq!(launch.workspace, root.path());
    assert_eq!(launch.provider_name, "deepseek");
    assert_eq!(launch.model, "deepseek-v4-pro");

    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("Starting harness with deepseek/deepseek-v4-pro"));

    // The window read from /models survives the round trip through the file.
    let loaded = harness_cli::config::ConfigStore::new(&launch.config_path)
        .load()
        .unwrap();
    let saved = loaded.provider("deepseek").unwrap();
    assert_eq!(saved.models().len(), 2);
    assert_eq!(
        saved
            .model_info("deepseek-v4-pro")
            .and_then(|info| info.context_window),
        Some(131_072)
    );
}

fn discovered(provider: ProviderConfig, raw: &str) -> (ProviderConfig, ModelDiscovery) {
    let discovery = ModelDiscovery::from_openai_compatible_response(&provider, raw).unwrap();
    (provider, discovery)
}

/// A custom-provider wizard that already received `raw` as its model list.
fn open_custom_wizard_at_models(raw: &str) -> TuiApp {
    let mut app = setup_app();
    type_text_tui(&mut app, "/provider add");
    app.handle_key(key(KeyCode::Enter));
    app.handle_key(key(KeyCode::Up)); // custom
    app.handle_key(key(KeyCode::Enter));
    type_text_tui(&mut app, "gateway");
    app.handle_key(key(KeyCode::Enter));
    type_text_tui(&mut app, "https://gateway.example/v1");
    app.handle_key(key(KeyCode::Enter));
    type_text_tui(&mut app, "sk");
    let TuiAction::FetchModels(provider) = app.handle_key(key(KeyCode::Enter)) else {
        panic!("expected a model list request");
    };
    app.models_loaded(Ok(discovered(provider, raw)));
    app
}

fn setup_app() -> TuiApp {
    TuiApp::new(
        "harness",
        PathBuf::from("C:/config/providers.json"),
        PathBuf::from("C:/work/project"),
    )
}

fn type_text_tui(app: &mut TuiApp, text: &str) {
    for ch in text.chars() {
        app.handle_key(key(KeyCode::Char(ch)));
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::plain(code)
}

fn lines_text(lines: &[Line]) -> String {
    lines
        .iter()
        .map(|line| line.text())
        .collect::<Vec<_>>()
        .join("\n")
}
