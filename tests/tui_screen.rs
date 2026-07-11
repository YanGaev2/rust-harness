use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use harness_cli::cli;
use harness_cli::providers::{CachePolicy, ChatApiFormat};
use harness_cli::tui::{
    SetupTuiAction, SetupTuiApp, TuiAction, TuiApp, TuiCommand, TuiProviderDraft, render_setup_tui,
    render_tui,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

#[test]
fn setup_tui_accepts_provider_add_command() {
    let mut app = setup_app();

    type_text(&mut app, "/provider add");
    let action = app.handle_key(key(KeyCode::Enter));

    assert_eq!(action, SetupTuiAction::ProviderAdd);
    assert_eq!(app.input(), "");
    assert!(app.status_message().contains("Provider setup"));
}

#[test]
fn setup_tui_renders_status_paths_commands_and_prompt() {
    let mut app = setup_app();
    type_text(&mut app, "/providers");

    let backend = TestBackend::new(96, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| render_setup_tui(frame, &app))
        .unwrap();

    let screen = buffer_text(terminal.backend().buffer());
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
fn tui_provider_add_runs_inside_wizard_and_returns_provider_draft() {
    let mut app = TuiApp::new(
        "harness",
        PathBuf::from("C:/config/providers.json"),
        PathBuf::from("C:/work/project"),
    );

    type_text_tui(&mut app, "/provider add");
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        TuiAction::Command(TuiCommand::ProviderAdd)
    );
    assert!(
        app.dialog_title()
            .is_some_and(|title| title.contains("Provider"))
    );

    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Enter));

    type_text_tui(&mut app, "https://api.deepseek.com/v1");
    app.handle_key(key(KeyCode::Enter));
    type_text_tui(&mut app, "sk-test");
    app.handle_key(key(KeyCode::Enter));
    type_text_tui(&mut app, "deepseek-v4-pro");
    let action = app.handle_key(key(KeyCode::Enter));

    assert_eq!(
        action,
        TuiAction::SaveProvider(TuiProviderDraft {
            name: "deepseek".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            api_key: "sk-test".to_string(),
            model: "deepseek-v4-pro".to_string(),
        })
    );
}

#[test]
fn tui_paste_fills_api_key_field_in_wizard_and_strips_newline() {
    let mut app = TuiApp::new(
        "harness",
        PathBuf::from("C:/config/providers.json"),
        PathBuf::from("C:/work/project"),
    );

    type_text_tui(&mut app, "/provider add");
    app.handle_key(key(KeyCode::Enter)); // open wizard on the provider step
    app.handle_key(key(KeyCode::Enter)); // accept first provider -> base URL step
    app.handle_key(key(KeyCode::Enter)); // accept default base URL -> API key step

    // Clipboards routinely include a trailing newline. A bracketed paste must
    // land in the active field without that newline submitting or corrupting it.
    app.handle_paste("sk-pasted-secret-123\n");
    app.handle_key(key(KeyCode::Enter)); // accept API key -> model step
    type_text_tui(&mut app, "custom-model");
    let action = app.handle_key(key(KeyCode::Enter)); // save

    let TuiAction::SaveProvider(draft) = action else {
        panic!("expected save provider action, got {action:?}");
    };
    assert_eq!(draft.api_key, "sk-pasted-secret-123");
    assert_eq!(draft.model, "custom-model");
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

    // The setup TUI still runs on crossterm and must enable bracketed paste
    // itself so multi-line pastes do not break the terminal.
    let tui = read("src/tui.rs");
    assert!(
        tui.contains("EnableBracketedPaste"),
        "src/tui.rs (setup TUI) must enable bracketed paste"
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

    let backend = TestBackend::new(100, 28);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| render_tui(frame, &app)).unwrap();

    let screen = buffer_text(terminal.backend().buffer());
    assert!(screen.contains("Provider setup"));
    assert!(screen.contains("Select provider"));
    assert!(screen.contains("codex"));
    assert!(screen.contains("deepseek"));
    assert!(screen.contains("Enter accept"));
    assert!(screen.contains("Esc close"));
}

#[test]
fn tui_provider_draft_uses_builtin_profile_metadata() {
    let provider = cli::provider_config_from_tui_draft(TuiProviderDraft {
        name: "deepseek".to_string(),
        base_url: "https://api.deepseek.com/v1".to_string(),
        api_key: "sk-test".to_string(),
        model: "deepseek-v4-pro".to_string(),
    });

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
    assert_eq!(provider.models(), &["deepseek-v4-pro".to_string()]);
}

#[test]
fn cli_finishes_setup_from_tui_save_provider_action() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("providers.json");
    let mut output = Vec::new();

    let launch = cli::finish_tui_setup_action(
        TuiAction::SaveProvider(TuiProviderDraft {
            name: "deepseek".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            api_key: "sk-test".to_string(),
            model: "deepseek-v4-pro".to_string(),
        }),
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

    let loaded = harness_cli::config::ConfigStore::new(&launch.config_path)
        .load()
        .unwrap();
    assert!(loaded.provider("deepseek").is_some());
}

fn setup_app() -> SetupTuiApp {
    SetupTuiApp::new(
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

fn type_text(app: &mut SetupTuiApp, text: &str) {
    for ch in text.chars() {
        app.handle_key(key(KeyCode::Char(ch)));
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn buffer_text(buffer: &Buffer) -> String {
    let mut text = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}
