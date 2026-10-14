//! Tests for the pure chat state machine (`harness_cli::chat::ChatApp`) built
//! on the in-repo `harness-tui` library. Ports the spirit of the removed
//! `tests/chat_tui.rs` suite onto the new pure line-based API.

use harness_cli::agent::AgentEvent;
use harness_cli::chat::{BusyAction, ChatAction, ChatApp};
use harness_cli::runtime::ToolBatchResult;
use harness_tui::input::{Event, KeyCode, KeyEvent, MouseEvent, MouseKind};
use harness_tui::text::{Color, Line, visible_width};
use serde_json::json;

fn app() -> ChatApp {
    ChatApp::new("deepseek/deepseek-v4-pro", "C:/work/project")
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::plain(code)
}

fn type_text(app: &mut ChatApp, text: &str) {
    for ch in text.chars() {
        app.handle_key(key(KeyCode::Char(ch)));
    }
}

fn submit_message(app: &mut ChatApp, text: &str) -> ChatAction {
    type_text(app, text);
    app.handle_key(key(KeyCode::Enter))
}

fn lines_text(lines: &[Line]) -> String {
    lines
        .iter()
        .map(|line| line.text())
        .collect::<Vec<_>>()
        .join("\n")
}

fn ok_result(id: &str, tool_name: &str, content: &str) -> ToolBatchResult {
    ToolBatchResult {
        id: id.to_string(),
        tool_name: tool_name.to_string(),
        ok: true,
        repaired: false,
        content: content.to_string(),
        metadata: json!({}),
        error: None,
        hint: None,
    }
}

#[test]
fn cost_command_reports_session_tokens_and_dated_estimate() {
    let mut app = app(); // deepseek/deepseek-v4-pro — a priced preset
    app.set_busy(true);
    app.push_agent_event(&AgentEvent::UsageUpdated(harness_cli::agent::UsageTotals {
        requests: 2,
        prompt_tokens: 1_000_000,
        cached_tokens: 800_000,
        completion_tokens: 50_000,
        ..Default::default()
    }));
    app.set_busy(false);

    let action = submit_message(&mut app, "/cost");
    assert_eq!(action, ChatAction::Continue);

    let lines = app.transcript_lines(120);
    let text = lines_text(&lines);
    assert!(text.contains("1000000"), "prompt tokens: {text}");
    assert!(text.contains("800000"), "cached tokens: {text}");
    assert!(text.contains("50000"), "completion tokens: {text}");
    // 200k fresh * 0.435 + 800k cached * 0.003625 + 50k out * 0.87 (per 1M)
    // = 0.087 + 0.0029 + 0.0435 ≈ $0.1334
    assert!(text.contains("$0.1334"), "estimate: {text}");
    assert!(text.contains("2026-07-13"), "price date: {text}");
}

#[test]
fn cost_command_without_pricing_still_reports_tokens() {
    let mut app = ChatApp::new("local/custom-model", "C:/work/project");
    app.set_busy(true);
    app.push_agent_event(&AgentEvent::UsageUpdated(harness_cli::agent::UsageTotals {
        requests: 1,
        prompt_tokens: 500,
        cached_tokens: 0,
        completion_tokens: 20,
        ..Default::default()
    }));
    app.set_busy(false);

    submit_message(&mut app, "/cost");
    let lines = app.transcript_lines(120);
    let text = lines_text(&lines);
    assert!(text.contains("500"), "{text}");
    assert!(
        text.contains("no pricing known"),
        "must be honest about the missing price list: {text}"
    );
}

// --- input handling ---

#[test]
fn enter_submits_non_empty_message_and_records_history() {
    let mut app = app();
    type_text(&mut app, "write notes.txt");
    let action = app.handle_key(key(KeyCode::Enter));

    assert_eq!(action, ChatAction::Submit("write notes.txt".to_string()));
    assert_eq!(app.input(), "");
    assert!(app.transcript_text().contains("you: write notes.txt"));

    // The prompt landed in history: Up recalls it.
    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.input(), "write notes.txt");
}

#[test]
fn enter_on_empty_input_does_not_submit() {
    let mut app = app();
    assert_eq!(app.handle_key(key(KeyCode::Enter)), ChatAction::Continue);
    assert!(app.transcript_text().is_empty());
}

#[test]
fn ctrl_c_exits() {
    let mut app = app();
    let action = app.handle_key(KeyEvent::ctrl(KeyCode::Char('c')));
    assert_eq!(action, ChatAction::Exit);
}

#[test]
fn esc_arms_a_confirmation_and_only_the_second_esc_exits() {
    let mut app = app();
    // First Esc: no exit, the status row asks for a confirming press.
    assert_eq!(app.handle_key(key(KeyCode::Esc)), ChatAction::Continue);
    let status = lines_text(&app.frame(120, 10));
    assert!(
        status.contains("Press Esc again to exit"),
        "missing confirmation hint: {status}"
    );
    assert_eq!(app.handle_key(key(KeyCode::Esc)), ChatAction::Exit);
}

#[test]
fn any_other_key_disarms_the_exit_confirmation() {
    let mut app = app();
    app.handle_key(key(KeyCode::Esc));
    app.handle_key(key(KeyCode::Char('h')));
    // The armed state was dropped: the next Esc starts over.
    assert_eq!(app.handle_key(key(KeyCode::Esc)), ChatAction::Continue);
    let status = lines_text(&app.frame(120, 10));
    assert!(status.contains("Press Esc again to exit"), "{status}");
}

#[test]
fn esc_that_dismisses_the_completion_menu_does_not_arm_exit() {
    let mut app = app();
    type_text(&mut app, "/mo");
    assert!(app.completion_visible());
    assert_eq!(app.handle_key(key(KeyCode::Esc)), ChatAction::Continue);
    // The menu absorbed that Esc — a follow-up Esc merely arms the
    // confirmation instead of exiting.
    assert_eq!(app.handle_key(key(KeyCode::Esc)), ChatAction::Continue);
}

#[test]
fn esc_dismisses_completion_until_query_changes() {
    let mut app = app();
    type_text(&mut app, "/p");
    assert!(app.completion_visible());

    let action = app.handle_key(key(KeyCode::Esc));
    assert_eq!(action, ChatAction::Continue);
    assert!(!app.completion_visible());

    // Editing the query re-arms the menu.
    type_text(&mut app, "r");
    assert!(app.completion_visible());
}

#[test]
fn paste_inserts_without_submitting() {
    let mut app = app();
    type_text(&mut app, "ask: ");
    let action = app.handle_paste("line one\nline two");
    assert_eq!(action, ChatAction::Continue);
    assert_eq!(app.input(), "ask: line one\nline two");
    assert!(app.transcript_text().is_empty());
}

// --- paste collapse (Claude-Code-style placeholders) ---

#[test]
fn large_paste_collapses_to_a_placeholder() {
    let mut app = app();
    let pasted = (0..184)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.handle_paste(&pasted);
    // The editor holds only the compact marker, not 184 raw lines.
    assert_eq!(app.input(), "[Pasted text #1 +184 lines]");
}

#[test]
fn long_single_line_paste_collapses_to_a_char_placeholder() {
    let mut app = app();
    app.handle_paste(&"x".repeat(2000));
    assert_eq!(app.input(), "[Pasted text #1 +2000 chars]");
}

#[test]
fn submit_expands_the_placeholder_into_the_full_pasted_text() {
    let mut app = app();
    type_text(&mut app, "summarize: ");
    let pasted = (0..10)
        .map(|i| format!("row {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.handle_paste(&pasted);
    let action = app.handle_key(key(KeyCode::Enter));
    let ChatAction::Submit(message) = action else {
        panic!("expected submit, got {action:?}");
    };
    // The model receives the full text…
    assert!(message.starts_with("summarize: row 0\n"), "{message}");
    assert!(message.contains("row 9"), "{message}");
    // …while the transcript shows the compact placeholder.
    assert!(
        app.transcript_text().contains("[Pasted text #1 +10 lines]"),
        "{}",
        app.transcript_text()
    );
    assert!(!app.transcript_text().contains("row 5"));
}

#[test]
fn two_pastes_get_distinct_placeholders_and_both_expand() {
    let mut app = app();
    app.handle_paste(&"alpha\n".repeat(12));
    app.handle_paste(&"beta\n".repeat(7));
    assert!(app.input().contains("[Pasted text #1 +12 lines]"));
    assert!(app.input().contains("[Pasted text #2 +7 lines]"));
    let ChatAction::Submit(message) = app.handle_key(key(KeyCode::Enter)) else {
        panic!("expected submit");
    };
    assert!(message.contains("alpha\nalpha"), "{message}");
    assert!(message.contains("beta\nbeta"), "{message}");
}

#[test]
fn placeholder_deleted_by_the_user_is_not_expanded() {
    let mut app = app();
    app.handle_paste(&"secret\n".repeat(10));
    // The user erases the placeholder entirely and types something else.
    while !app.input().is_empty() {
        app.handle_key(key(KeyCode::Backspace));
    }
    let action = submit_message(&mut app, "just say hi");
    assert_eq!(action, ChatAction::Submit("just say hi".to_string()));
}

#[test]
fn small_paste_is_inserted_verbatim() {
    let mut app = app();
    app.handle_paste("one\ntwo");
    assert_eq!(app.input(), "one\ntwo");
}

// --- welcome banner ---

#[test]
fn welcome_banner_names_the_build_model_and_workspace() {
    let mut app = ChatApp::new("deepseek/deepseek-v4-pro", "C:/work/project");
    app.push_banner("0.1.0");
    let text = lines_text(&app.transcript_lines(80));
    assert!(text.contains("harness"), "{text}");
    assert!(text.contains("0.1.0"), "{text}");
    assert!(text.contains("deepseek/deepseek-v4-pro"), "{text}");
    assert!(text.contains("C:/work/project"), "{text}");
}

#[test]
fn alt_enter_inserts_newline_without_submitting() {
    let mut app = app();
    type_text(&mut app, "line one");
    let action = app.handle_key(KeyEvent::alt(KeyCode::Enter));
    assert_eq!(action, ChatAction::Continue);
    type_text(&mut app, "line two");
    assert_eq!(app.input(), "line one\nline two");
}

#[test]
fn ctrl_j_inserts_newline_without_submitting() {
    let mut app = app();
    type_text(&mut app, "line one");
    let action = app.handle_key(KeyEvent::ctrl(KeyCode::Char('j')));
    assert_eq!(action, ChatAction::Continue);
    type_text(&mut app, "line two");
    assert_eq!(app.input(), "line one\nline two");
}

#[test]
fn arrow_keys_recall_prompt_history() {
    let mut app = app();
    submit_message(&mut app, "first");
    submit_message(&mut app, "second");

    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.input(), "second");
    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.input(), "first");
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.input(), "second");
    // Past the newest entry the compose clears.
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.input(), "");
}

#[test]
fn ctrl_v_requests_clipboard_capture_and_callback_inserts() {
    let mut app = app();
    let action = app.handle_key(KeyEvent::ctrl(KeyCode::Char('v')));
    assert_eq!(action, ChatAction::CaptureClipboard);

    app.apply_clipboard_text("pasted from clipboard");
    assert_eq!(app.input(), "pasted from clipboard");
}

#[test]
fn slash_paste_requests_clipboard_capture() {
    // Windows Terminal binds Ctrl+V to its own paste action, so the 0x16 byte
    // never reaches the app there; /paste is the terminal-agnostic route to
    // the same clipboard capture (text or PNG attachment).
    let mut app = app();
    let action = submit_message(&mut app, "/paste");
    assert_eq!(action, ChatAction::CaptureClipboard);
    assert_eq!(app.input(), "");
}

#[test]
fn slash_paste_appears_in_command_suggestions() {
    let mut app = app();
    type_text(&mut app, "/pa");
    assert!(app.completion_visible());
    let suggestions = app.completion_suggestions();
    assert!(suggestions.contains(&"/paste".to_string()));
}

#[test]
fn slash_retain_thinking_toggles_and_reports_state() {
    // Default ON since 2026-08-08: the reasoning echo is required by the
    // thinking models this harness targets (DeepSeek V4 / Kimi / MiMo), and
    // it is what lets an interrupted turn resume with its thinking intact.
    let mut app = app();
    assert!(app.retain_thinking(), "retention must default on");

    let action = submit_message(&mut app, "/retain-thinking");
    assert_eq!(action, ChatAction::Continue);
    assert!(!app.retain_thinking());
    let lines = app.transcript_lines(120);
    let text = lines_text(&lines);
    assert!(text.contains("retain thinking: off"), "{text}");

    let action = submit_message(&mut app, "/retain-thinking");
    assert_eq!(action, ChatAction::Continue);
    assert!(app.retain_thinking());
    let lines = app.transcript_lines(120);
    let text = lines_text(&lines);
    assert!(text.contains("retain thinking: on"), "{text}");
}

#[test]
fn stream_idle_note_appears_only_after_sustained_silence() {
    // The note distinguishes "the model is silently generating a big tool
    // call" from a frozen UI; short gaps between deltas must not flicker it.
    use harness_cli::chat::stream_idle_note;

    assert_eq!(stream_idle_note(0), None);
    assert_eq!(stream_idle_note(14), None);
    let note = stream_idle_note(42).expect("sustained silence earns a note");
    assert!(note.contains("42s"), "{note}");

    // A fresh busy turn starts with zero silence.
    let mut app = app();
    app.set_busy(true);
    assert_eq!(app.stream_idle_secs(), 0);
}

#[test]
fn slash_retain_thinking_appears_in_command_suggestions() {
    let mut app = app();
    type_text(&mut app, "/ret");
    assert!(app.completion_visible());
    let suggestions = app.completion_suggestions();
    assert!(suggestions.contains(&"/retain-thinking".to_string()));
}

#[test]
fn slash_model_returns_switch_action_without_echo() {
    let mut app = app();
    let action = submit_message(&mut app, "/model claude claude-sonnet-4.5");
    assert_eq!(
        action,
        ChatAction::SwitchModel {
            provider: "claude".to_string(),
            model: "claude-sonnet-4.5".to_string(),
        }
    );
    assert_eq!(app.input(), "");
    assert!(!app.transcript_text().contains("you: /model"));
}

#[test]
fn slash_model_without_catalog_shows_usage() {
    let mut app = app(); // no catalog attached
    let action = submit_message(&mut app, "/model");
    assert_eq!(action, ChatAction::Continue);
    assert!(app.transcript_text().contains("usage: /model"));
}

fn catalog_app() -> ChatApp {
    ChatApp::new("deepseek/deepseek-v4-pro", "C:/work/project").with_catalog(vec![
        (
            "deepseek".to_string(),
            vec![
                "deepseek-v4-pro".to_string(),
                "deepseek-v4-flash".to_string(),
            ],
        ),
        ("glm".to_string(), vec!["glm-5.2".to_string()]),
    ])
}

#[test]
fn slash_model_no_args_opens_interactive_picker() {
    let mut app = catalog_app();
    let action = submit_message(&mut app, "/model");
    assert_eq!(action, ChatAction::Continue);
    assert!(app.model_picker_visible());
    let panel = lines_text(&app.frame(100, 30));
    // Cursor starts on the active pair, which also carries the check mark.
    assert!(panel.contains("› deepseek-v4-pro [deepseek] ✓"));
    assert!(panel.contains("deepseek-v4-flash [deepseek]"));
    assert!(panel.contains("glm-5.2 [glm]"));
    assert!(panel.contains("(1/3)"));
}

#[test]
fn picker_cursor_starts_on_active_pair() {
    let mut app = ChatApp::new("glm/glm-5.2", "C:/work/project").with_catalog(vec![
        (
            "deepseek".to_string(),
            vec![
                "deepseek-v4-pro".to_string(),
                "deepseek-v4-flash".to_string(),
            ],
        ),
        ("glm".to_string(), vec!["glm-5.2".to_string()]),
    ]);
    submit_message(&mut app, "/model");
    let panel = lines_text(&app.frame(100, 30));
    assert!(panel.contains("› glm-5.2 [glm] ✓"));
    assert!(panel.contains("(3/3)"));
}

#[test]
fn picker_down_enter_switches_to_highlighted_model() {
    let mut app = catalog_app();
    submit_message(&mut app, "/model");
    app.handle_key(key(KeyCode::Down));
    let action = app.handle_key(key(KeyCode::Enter));
    assert_eq!(
        action,
        ChatAction::SwitchModel {
            provider: "deepseek".to_string(),
            model: "deepseek-v4-flash".to_string(),
        }
    );
    assert!(!app.model_picker_visible());
}

#[test]
fn picker_esc_closes_without_switching() {
    let mut app = catalog_app();
    submit_message(&mut app, "/model");
    let action = app.handle_key(key(KeyCode::Esc));
    assert_eq!(action, ChatAction::Continue);
    assert!(!app.model_picker_visible());
    // Keys return to the editor afterwards.
    app.handle_key(key(KeyCode::Char('x')));
    assert_eq!(app.input(), "x");
}

#[test]
fn picker_typing_filters_and_enter_picks_the_match() {
    let mut app = catalog_app();
    submit_message(&mut app, "/model");
    for ch in "glm".chars() {
        app.handle_key(key(KeyCode::Char(ch)));
    }
    let panel = lines_text(&app.frame(100, 30));
    assert!(panel.contains("(1/1)"));
    // Picker rows tag the provider in brackets; the filtered-out deepseek
    // rows are gone (the status row still shows the active deepseek label).
    assert!(!panel.contains("[deepseek]"));
    let action = app.handle_key(key(KeyCode::Enter));
    assert_eq!(
        action,
        ChatAction::SwitchModel {
            provider: "glm".to_string(),
            model: "glm-5.2".to_string(),
        }
    );
}

#[test]
fn slash_model_numeric_picks_menu_entry() {
    let mut app = catalog_app();
    let action = submit_message(&mut app, "/model 3");
    assert_eq!(
        action,
        ChatAction::SwitchModel {
            provider: "glm".to_string(),
            model: "glm-5.2".to_string(),
        }
    );
}

#[test]
fn slash_model_out_of_range_number_reports_error() {
    let mut app = catalog_app();
    let action = submit_message(&mut app, "/model 9");
    assert_eq!(action, ChatAction::Continue);
    assert!(app.transcript_text().contains("no menu entry 9"));
}

#[test]
fn slash_model_single_model_name_finds_owning_provider() {
    let mut app = catalog_app();
    let action = submit_message(&mut app, "/model deepseek-v4-flash");
    assert_eq!(
        action,
        ChatAction::SwitchModel {
            provider: "deepseek".to_string(),
            model: "deepseek-v4-flash".to_string(),
        }
    );
}

#[test]
fn slash_model_provider_name_switches_to_its_first_model() {
    let mut app = catalog_app();
    let action = submit_message(&mut app, "/model glm");
    assert_eq!(
        action,
        ChatAction::SwitchModel {
            provider: "glm".to_string(),
            model: "glm-5.2".to_string(),
        }
    );
}

#[test]
fn slash_model_unknown_name_targets_active_provider_as_new_model() {
    let mut app = catalog_app();
    let action = submit_message(&mut app, "/model qwen3.7-max-preview");
    assert_eq!(
        action,
        ChatAction::SwitchModel {
            provider: "deepseek".to_string(),
            model: "qwen3.7-max-preview".to_string(),
        }
    );
}

#[test]
fn slash_provider_lists_catalog_with_active_marker() {
    let mut app = catalog_app();
    submit_message(&mut app, "/provider");
    let transcript = app.transcript_text();
    assert!(transcript.contains("active provider: deepseek/deepseek-v4-pro"));
    assert!(transcript.contains("deepseek: deepseek-v4-pro, deepseek-v4-flash"));
    assert!(transcript.contains("glm: glm-5.2"));
}

#[test]
fn slash_provider_shows_active_provider() {
    let mut app = app();
    submit_message(&mut app, "/provider");
    assert!(
        app.transcript_text()
            .contains("active provider: deepseek/deepseek-v4-pro")
    );
}

#[test]
fn slash_new_returns_new_session_and_empties_the_transcript() {
    let mut app = app();
    app.push_user_message("old turn");
    assert!(lines_text(&app.frame(80, 20)).contains("old turn"));

    let action = submit_message(&mut app, "/new");
    assert_eq!(action, ChatAction::NewSession);
    assert!(app.transcript_text().is_empty());
    assert!(app.transcript_lines(80).is_empty());
    assert!(!lines_text(&app.frame(80, 20)).contains("old turn"));
    assert_eq!(app.input(), "");
}

#[test]
fn slash_clear_empties_the_transcript_on_screen_too() {
    let mut app = app();
    app.push_user_message("hi there");
    assert!(lines_text(&app.frame(80, 20)).contains("hi there"));
    let action = submit_message(&mut app, "/clear");
    assert_eq!(action, ChatAction::Continue);
    assert!(app.transcript_text().is_empty());
    assert!(!lines_text(&app.frame(80, 20)).contains("hi there"));
}

#[test]
fn slash_help_opens_palette_and_any_key_closes_it() {
    let mut app = app();
    let action = submit_message(&mut app, "/help");
    assert_eq!(action, ChatAction::Continue);
    assert!(app.help_visible());

    // The palette content is part of the pinned panel.
    let panel = lines_text(&app.frame(100, 20));
    assert!(panel.contains("Commands"));
    assert!(panel.contains("/model [N | MODEL | PROVIDER MODEL]"));

    // Any key closes the palette without editing the compose.
    let action = app.handle_key(key(KeyCode::Char('x')));
    assert_eq!(action, ChatAction::Continue);
    assert!(!app.help_visible());
    assert_eq!(app.input(), "");
}

#[test]
fn slash_history_lists_matching_past_prompts() {
    let mut app = app();
    submit_message(&mut app, "write cache notes");
    submit_message(&mut app, "summarize routing logs");

    submit_message(&mut app, "/history cache");
    let transcript = app.transcript_text();
    assert!(transcript.contains("history 'cache': 1 match(es)"));
    assert!(transcript.contains("write cache notes"));
}

#[test]
fn unknown_command_pushes_system_line() {
    let mut app = app();
    submit_message(&mut app, "/bogus");
    assert!(
        app.transcript_text()
            .contains("unknown command: /bogus (try /help)")
    );
}

// --- completions ---

#[test]
fn typing_slash_shows_command_suggestions() {
    let mut app = app();
    type_text(&mut app, "/");
    assert!(app.completion_visible());
    let suggestions = app.completion_suggestions();
    assert!(suggestions.contains(&"/model".to_string()));
    assert!(suggestions.contains(&"/history".to_string()));
    assert!(suggestions.contains(&"/exit".to_string()));
}

#[test]
fn completion_up_down_navigate_selection() {
    let mut app = app();
    type_text(&mut app, "/");
    let first = app.completion_selected();
    app.handle_key(key(KeyCode::Down));
    assert_ne!(app.completion_selected(), first);
    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.completion_selected(), first);
}

#[test]
fn tab_completes_selection_with_trailing_space() {
    let mut app = app();
    type_text(&mut app, "/p");
    assert_eq!(app.completion_selected().as_deref(), Some("/provider"));
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.input(), "/provider ");
    assert!(!app.completion_visible());
}

#[test]
fn model_argument_completions_offer_provider_model_pairs() {
    let mut app = catalog_app();
    type_text(&mut app, "/model g");
    assert!(app.completion_visible());
    let suggestions = app.completion_suggestions();
    assert!(suggestions.contains(&"/model glm glm-5.2".to_string()));
    // deepseek pairs do not match the "g" prefix on either side.
    assert!(!suggestions.iter().any(|s| s.contains("deepseek-v4-pro")));
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.input(), "/model glm glm-5.2 ");
}

#[test]
fn model_argument_completions_list_all_pairs_after_bare_command() {
    let mut app = catalog_app();
    type_text(&mut app, "/model ");
    assert!(app.completion_visible());
    assert_eq!(app.completion_suggestions().len(), 3);
}

#[test]
fn add_catalog_model_extends_menu_for_future_switches() {
    let mut app = catalog_app();
    app.add_catalog_model("glm", "glm-5.2-fast-preview");
    let action = submit_message(&mut app, "/model glm-5.2-fast-preview");
    assert_eq!(
        action,
        ChatAction::SwitchModel {
            provider: "glm".to_string(),
            model: "glm-5.2-fast-preview".to_string(),
        }
    );
}

// --- agent events ---

#[test]
fn thinking_events_coalesce_into_one_block() {
    let mut app = app();
    app.push_agent_event(&AgentEvent::Thinking("planning ".to_string()));
    app.push_agent_event(&AgentEvent::Thinking("the edit".to_string()));

    let transcript = app.transcript_text();
    assert!(transcript.contains("thinking: planning the edit"));
    assert_eq!(transcript.matches("thinking:").count(), 1);
}

#[test]
fn single_call_round_has_no_round_banner() {
    let mut app = app();
    app.push_agent_event(&AgentEvent::ToolRoundStarted {
        round: 1,
        tool_calls: 1,
    });
    assert!(!app.transcript_text().contains("tool round"));
}

#[test]
fn multi_call_round_gets_a_banner() {
    let mut app = app();
    app.push_agent_event(&AgentEvent::ToolRoundStarted {
        round: 1,
        tool_calls: 2,
    });
    assert!(app.transcript_text().contains("tool round 1: 2 call(s)"));
}

#[test]
fn tool_card_updates_in_place_on_result_and_surfaces_memo() {
    let mut app = app();
    app.push_user_message("write a file");
    app.push_agent_event(&AgentEvent::ToolCallStarted {
        round: 1,
        id: "call-1".to_string(),
        name: "file.write".to_string(),
        arguments: json!({"path": "notes.txt"}),
    });
    app.push_agent_event(&AgentEvent::ToolResult(ToolBatchResult {
        id: "call-1".to_string(),
        tool_name: "file.write".to_string(),
        ok: true,
        repaired: true,
        content: "wrote 12 bytes".to_string(),
        metadata: json!({}),
        error: None,
        hint: Some("Next time call 'file_write' with arguments like {\"path\": ...}".to_string()),
    }));

    let transcript = app.transcript_text();
    assert!(transcript.contains("file.write"));
    assert!(transcript.contains("notes.txt"));
    assert!(transcript.contains("✓"));
    assert!(transcript.contains("memo:"));
    // Updated in place, not duplicated.
    assert_eq!(transcript.matches("file.write").count(), 1);
}

#[test]
fn tool_card_shows_canonical_name_for_wire_alias() {
    let mut app = app();
    app.push_agent_event(&AgentEvent::ToolCallStarted {
        round: 1,
        id: "c1".to_string(),
        name: "file_list".to_string(),
        arguments: json!({"path": "."}),
    });
    let transcript = app.transcript_text();
    assert!(transcript.contains("file.list"));
    assert!(!transcript.contains("file_list"));
}

#[test]
fn tool_summary_counts_multi_line_results() {
    let mut app = app();
    app.push_agent_event(&AgentEvent::ToolResult(ok_result(
        "c1",
        "file.list",
        ".git/\nCargo.toml\nsrc/",
    )));
    let transcript = app.transcript_text();
    assert!(
        transcript.contains("3 lines"),
        "multi-line results must show a line count, got: {transcript}"
    );
}

fn started(app: &mut ChatApp, id: &str, name: &str, arguments: serde_json::Value) {
    app.push_agent_event(&AgentEvent::ToolCallStarted {
        round: 1,
        id: id.to_string(),
        name: name.to_string(),
        arguments,
    });
}

fn card_rows(app: &mut ChatApp, width: usize) -> Vec<String> {
    app.frame(width, 40)
        .iter()
        .map(Line::text)
        .filter(|row| !row.is_empty())
        .collect()
}

#[test]
fn running_card_shows_a_spinner_and_the_headline_argument() {
    let mut app = app();
    app.set_busy(true);
    started(
        &mut app,
        "c1",
        "read_file",
        json!({"file_path": "src/chat.rs"}),
    );
    let rows = card_rows(&mut app, 80);
    let card = rows
        .iter()
        .find(|row| row.contains("file.read"))
        .expect("card row");
    assert!(card.contains("file.read src/chat.rs"), "{card:?}");
    assert!(!card.contains('{'), "no raw JSON on the card: {card:?}");
    assert!(
        harness_tui::components::spinner::FRAMES
            .iter()
            .any(|frame| card.starts_with(frame)),
        "running card leads with a spinner frame: {card:?}"
    );
}

#[test]
fn search_card_shows_pattern_and_path() {
    let mut app = app();
    app.set_busy(true);
    started(
        &mut app,
        "c1",
        "grep",
        json!({"pattern": "fn main", "path": "src"}),
    );
    let rows = card_rows(&mut app, 80);
    assert!(
        rows.iter().any(|row| row.contains("fn main · src")),
        "{rows:?}"
    );
}

#[test]
fn shell_card_shows_the_output_tail_and_a_failing_exit_code() {
    let mut app = app();
    app.set_busy(true);
    started(
        &mut app,
        "c1",
        "shell.exec",
        json!({"command": "cargo test"}),
    );
    let output: String = (1..=8).map(|i| format!("line {i}\r\n")).collect();
    app.push_agent_event(&AgentEvent::ToolResult(ToolBatchResult {
        metadata: json!({"exit_code": 101}),
        ..ok_result("c1", "shell.exec", &output)
    }));
    let rows = card_rows(&mut app, 80);
    let card = rows
        .iter()
        .position(|row| row.contains("shell.exec cargo test"))
        .unwrap();
    assert!(rows[card].starts_with("✓ "), "{:?}", rows[card]);
    assert_eq!(rows[card + 1], "  ⎿ … +3 earlier lines");
    assert_eq!(rows[card + 2], "    line 4");
    assert_eq!(rows[card + 6], "    line 8");
    assert_eq!(rows[card + 7], "    exit 101");
}

#[test]
fn failed_shell_card_keeps_the_output_tail_not_a_bare_failed() {
    let mut app = app();
    app.set_busy(true);
    started(
        &mut app,
        "c1",
        "shell.exec",
        json!({"command": "cargo test"}),
    );
    app.push_agent_event(&AgentEvent::ToolResult(ToolBatchResult {
        ok: false,
        metadata: json!({"exit_code": 101}),
        ..ok_result("c1", "shell.exec", "test a ... ok\ntest b ... FAILED\n")
    }));
    let rows = card_rows(&mut app, 80);
    let card = rows
        .iter()
        .position(|row| row.contains("shell.exec cargo test"))
        .unwrap();
    assert!(rows[card].starts_with("✗ "), "{:?}", rows[card]);
    assert_eq!(rows[card + 1], "  ⎿ test a ... ok");
    assert_eq!(rows[card + 2], "    test b ... FAILED");
    assert_eq!(rows[card + 3], "    exit 101");
    assert!(
        !rows.iter().any(|row| row.ends_with("⎿ failed")),
        "{rows:?}"
    );
}

#[test]
fn timed_out_shell_card_says_why() {
    let mut app = app();
    app.set_busy(true);
    started(
        &mut app,
        "c1",
        "shell.exec",
        json!({"command": "cargo build"}),
    );
    app.push_agent_event(&AgentEvent::ToolResult(ToolBatchResult {
        ok: false,
        error: Some("shell command timed out after 30s".to_string()),
        ..ok_result("c1", "shell.exec", "Compiling harness\n")
    }));
    let rows = card_rows(&mut app, 80);
    let card = rows
        .iter()
        .position(|row| row.contains("shell.exec cargo build"))
        .unwrap();
    assert_eq!(rows[card + 1], "  ⎿ Compiling harness");
    assert_eq!(rows[card + 2], "    shell command timed out after 30s");
}

#[test]
fn edit_card_shows_a_colored_diff_from_the_call_arguments() {
    let mut app = app();
    app.set_busy(true);
    started(
        &mut app,
        "c1",
        "edit_file",
        json!({"path": "a.txt", "old_string": "beta", "new_string": "BETA\nbeta2"}),
    );
    app.push_agent_event(&AgentEvent::ToolResult(ok_result(
        "c1",
        "file.replace",
        "replaced 1 occurrence(s) in a.txt",
    )));
    let lines = app.frame(40, 40);
    let rows: Vec<String> = lines.iter().map(Line::text).collect();
    let card = rows
        .iter()
        .position(|row| row.contains("file.replace a.txt"))
        .unwrap();
    assert_eq!(rows[card + 1], "  ⎿ −1 +2");
    assert!(rows[card + 2].starts_with("    - beta"), "{rows:?}");
    assert!(rows[card + 3].starts_with("    + BETA"));
    assert!(rows[card + 4].starts_with("    + beta2"));
    let removed = &lines[card + 2];
    assert_eq!(removed.width(), 40, "diff rows span the width");
    assert!(
        removed.spans[1..]
            .iter()
            .all(|span| span.style.bg != Color::Default),
        "diff rows sit on a tinted background: {removed:?}"
    );
}

#[test]
fn read_card_reports_size_not_the_first_line() {
    let mut app = app();
    started(&mut app, "c1", "file.read", json!({"path": "a.txt"}));
    app.push_agent_event(&AgentEvent::ToolResult(ok_result(
        "c1",
        "file.read",
        "alpha\nbeta\ngamma\n",
    )));
    let rows = card_rows(&mut app, 80);
    assert!(rows.iter().any(|row| row == "  ⎿ 3 lines"), "{rows:?}");
    assert!(!rows.iter().any(|row| row.contains("alpha")), "{rows:?}");
}

#[test]
fn failed_card_shows_the_error_in_the_error_color() {
    let mut app = app();
    started(&mut app, "c1", "file.read", json!({"path": "missing.txt"}));
    app.push_agent_event(&AgentEvent::ToolResult(ToolBatchResult {
        ok: false,
        error: Some("file not found: missing.txt".to_string()),
        ..ok_result("c1", "file.read", "")
    }));
    let lines = app.frame(80, 40);
    let error = lines
        .iter()
        .find(|line| line.text().contains("file not found"))
        .expect("error row");
    assert!(error.text().starts_with("  ⎿ "), "{:?}", error.text());
    assert_ne!(error.spans[0].style.fg, Color::Default);
    assert!(
        lines
            .iter()
            .any(|line| line.text().starts_with("✗ file.read missing.txt"))
    );
}

#[test]
fn run_end_marks_unfinished_calls_interrupted() {
    let mut app = app();
    app.set_busy(true);
    started(
        &mut app,
        "c1",
        "shell.exec",
        json!({"command": "sleep 100"}),
    );
    app.set_busy(false);
    let flushed: Vec<String> = app.transcript_lines(80).iter().map(Line::text).collect();
    assert!(
        flushed
            .iter()
            .any(|row| row.starts_with("○ shell.exec sleep 100")),
        "{flushed:?}"
    );
    assert!(
        flushed.iter().any(|row| row == "  ⎿ interrupted"),
        "{flushed:?}"
    );
    assert!(app.transcript_text().contains("○ shell.exec"));
}

#[test]
fn final_deltas_coalesce_into_one_assistant_entry() {
    let mut app = app();
    app.push_agent_event(&AgentEvent::FinalContentDelta("hello ".to_string()));
    app.push_agent_event(&AgentEvent::FinalContentDelta("world".to_string()));

    let transcript = app.transcript_text();
    assert_eq!(transcript, "hello world");
}

// --- markdown rendering ---

#[test]
fn assistant_markdown_renders_heading_and_bullet() {
    let mut app = app();
    app.push_agent_event(&AgentEvent::FinalContentDelta(
        "# Title\n- item".to_string(),
    ));

    let panel = lines_text(&app.frame(100, 50));
    assert!(panel.contains("Title"));
    assert!(!panel.contains("# Title"));
    assert!(panel.contains("• item"));
}

#[test]
fn assistant_markdown_table_renders_aligned_without_pipes() {
    let mut app = app();
    app.push_agent_event(&AgentEvent::FinalContentDelta(
        "Files:\n\n| Name | Desc |\n|---|---|\n| `a.txt` | alpha file |\n| `b.rs` | beta |\n"
            .to_string(),
    ));

    let panel = lines_text(&app.frame(120, 60));
    assert!(!panel.contains('|'), "pipes must not leak: {panel}");
    let rows: Vec<&str> = panel.lines().collect();
    let header = rows.iter().find(|row| row.contains("Name")).unwrap();
    let data = rows.iter().find(|row| row.contains("a.txt")).unwrap();
    assert_eq!(
        header.find("Desc"),
        data.find("alpha file"),
        "table columns must be aligned"
    );
}

/// The six-column table from a real Kimi answer: far wider than a terminal.
const WIDE_TABLE: &str = "| # | Область | Где | Проблема | Что сделать | Эффект |\n\
|---|---|---|---|---|---|\n\
| 1 | Сборка | `Cargo.toml` | Нет `[profile.release]`: без LTO, 16 CU, с символами; бинарь 4.55 МБ | `lto=\"thin\"`, `codegen-units=1`, `strip=true` | -20-30% размера, быстрее холодный старт |\n\
| 2 | Сеть | `src/http.rs` | Новый `ureq::Agent` на каждый запрос, TCP+TLS хендшейк на каждом раунде | Мемоизировать агент по ключу | Секунды на многораундовом ране |";

/// The table rows of a rendered transcript, without the entry indent.
fn grid_rows(app: &mut ChatApp, width: usize) -> Vec<String> {
    app.transcript_lines(width)
        .iter()
        .map(|line| line.text().trim_start().to_string())
        .filter(|row| row.starts_with(['┌', '│', '├', '└']))
        .collect()
}

#[test]
fn table_is_a_full_grid_with_alignment_and_escaped_pipes() {
    let mut app = app();
    app.push_agent_event(&AgentEvent::FinalContentDelta(
        "| Cmd | Count |\n|---|---:|\n| `ls \\| wc` | 5 |\n| `a|b` | 100 |".to_string(),
    ));
    assert_eq!(
        grid_rows(&mut app, 80),
        [
            "┌─────────┬───────┐",
            "│ Cmd     │ Count │",
            "├─────────┼───────┤",
            "│ ls | wc │     5 │",
            "├─────────┼───────┤",
            "│ a|b     │   100 │",
            "└─────────┴───────┘",
        ]
    );
}

#[test]
fn wide_table_wraps_text_inside_the_cells() {
    for width in [80, 100, 165] {
        let mut app = app();
        app.push_agent_event(&AgentEvent::FinalContentDelta(WIDE_TABLE.to_string()));
        for line in app.transcript_lines(width) {
            assert!(line.width() <= width, "row overflows {width}: {line:?}");
        }
        let rows = grid_rows(&mut app, width);
        // A closed rectangle: every row as wide as the top border, and a rule
        // under the header and between the two data rows.
        let top = visible_width(&rows[0]);
        assert!(
            rows.iter().all(|row| visible_width(row) == top),
            "{rows:#?}"
        );
        assert!(rows[0].starts_with('┌') && rows.last().unwrap().starts_with('└'));
        assert_eq!(rows.iter().filter(|row| row.starts_with('├')).count(), 2);
        if width >= 100 {
            // Columns are wide enough for every word: wrapping loses none.
            let text = rows.join(" ");
            for word in ["[profile.release]:", "codegen-units=1,", "многораундовом"] {
                assert!(text.contains(word), "{word} in {rows:#?}");
            }
        }
    }
}

#[test]
fn long_code_line_wraps_with_a_continuation_marker() {
    let source = r"\begin{longtable}{@{}p{0.04\textwidth}p{0.11\textwidth}p{0.16\textwidth}p{0.27\textwidth}@{}}";
    let mut app = app();
    app.push_agent_event(&AgentEvent::FinalContentDelta(format!(
        "```latex\n{source}\n```"
    )));
    let lines = app.transcript_lines(40);

    for line in &lines {
        assert!(line.width() <= 40, "row overflows: {line:?}");
    }
    let rows: Vec<String> = lines.iter().map(|line| line.text()).collect();
    let first = rows
        .iter()
        .position(|row| row.trim_start().starts_with(r"\begin"))
        .expect("first code row");
    let tail: Vec<&String> = rows[first + 1..]
        .iter()
        .take_while(|row| row.trim_start().starts_with('↪'))
        .collect();
    assert!(!tail.is_empty(), "continuations are marked: {rows:#?}");
    // Stripping the markers gives back the source line.
    let rebuilt = std::iter::once(rows[first].trim())
        .chain(
            tail.iter()
                .map(|row| row.trim_start().trim_start_matches("↪ ")),
        )
        .collect::<String>();
    assert_eq!(rebuilt, source);
}

#[test]
fn math_in_answers_renders_as_unicode() {
    let mut app = app();
    app.push_agent_event(&AgentEvent::FinalContentDelta(
        r"Корни $x = \frac{-b \pm \sqrt{b^2-4ac}}{2a}$, сложность \(O(N^2)\), цена $5 и $10.

$$
\sum_{i=1}^{n} x_i^2
$$

Тулы web_search / web_fetch."
            .to_string(),
    ));
    let rows: Vec<String> = app
        .transcript_lines(120)
        .iter()
        .map(|line| line.text().trim().to_string())
        .collect();
    for expected in [
        "Корни x = (-b ± √(b²-4ac))/(2a), сложность O(N²), цена $5 и $10.",
        // A display block is a line of its own.
        "∑ᵢ₌₁ⁿ xᵢ²",
        // `_` inside a word is not emphasis.
        "Тулы web_search / web_fetch.",
    ] {
        assert!(
            rows.iter().any(|row| row == expected),
            "{expected} in {rows:#?}"
        );
    }
}

#[test]
fn display_math_still_streaming_stays_raw_text() {
    let mut app = app();
    app.push_agent_event(&AgentEvent::FinalContentDelta(
        "$$\n\\sum_i x_i".to_string(),
    ));
    let text = lines_text(&app.transcript_lines(80));
    assert!(
        text.contains("$$") && text.contains(r"\sum_i x_i"),
        "{text}"
    );
}

#[test]
fn copy_command_copies_the_last_answer_as_raw_markdown() {
    let mut app = app();
    assert_eq!(submit_message(&mut app, "/copy"), ChatAction::Continue);
    assert!(lines_text(&app.frame(80, 20)).contains("nothing to copy"));

    app.push_agent_event(&AgentEvent::FinalContentDelta("first answer".to_string()));
    app.push_user_message("again");
    app.push_agent_event(&AgentEvent::FinalContentDelta(WIDE_TABLE.to_string()));
    assert_eq!(
        submit_message(&mut app, "/copy"),
        ChatAction::Copy(WIDE_TABLE.to_string())
    );
    assert!(lines_text(&app.frame(80, 20)).contains("copied"));

    // A local command: it copies at once while the agent works.
    app.set_busy(true);
    assert_eq!(
        submit_message(&mut app, "/copy"),
        ChatAction::Copy(WIDE_TABLE.to_string())
    );
    assert_eq!(app.queued_len(), 0);
}

// --- panel ---

#[test]
fn panel_contains_editor_prompt_and_status_row() {
    let mut app = app();
    let panel = lines_text(&app.frame(100, 20));
    assert!(panel.contains("Ask anything"));
    assert!(panel.contains("deepseek/deepseek-v4-pro"));
    assert!(panel.contains("C:/work/project"));

    // The editor prompt row keeps the "› " prefix once text is typed.
    type_text(&mut app, "next question");
    let panel = lines_text(&app.frame(100, 20));
    assert!(panel.contains("› next question"));
}

#[test]
fn editor_sits_between_full_width_rules_without_side_borders() {
    let mut app = app();
    type_text(&mut app, "hello");
    let rows: Vec<String> = app.frame(40, 20).iter().map(|line| line.text()).collect();
    let editor = rows
        .iter()
        .position(|row| row.contains("› hello"))
        .expect("typed row");
    for rule in [&rows[editor - 1], &rows[editor + 1]] {
        assert_eq!(rule.chars().count(), 40, "rule spans the full width");
        assert!(rule.chars().all(|ch| ch == '─'), "{rule:?}");
    }
    // No side borders: a mouse selection copies only the text.
    assert!(!rows[editor].contains('│'), "{:?}", rows[editor]);
}

#[test]
fn user_turn_is_a_full_width_strip() {
    let mut app = app();
    app.push_user_message("a question long enough to wrap onto a second row");
    let lines = app.transcript_lines(30);
    let strip: Vec<&Line> = lines.iter().filter(|line| line.width() > 0).collect();
    assert!(strip.len() >= 2, "expected a wrapped turn: {strip:?}");
    for row in &strip {
        assert_eq!(
            row.width(),
            30,
            "strip row must span the width: {:?}",
            row.text()
        );
        assert!(
            row.spans.iter().all(|span| span.style.bg != Color::Default),
            "every cell sits on the strip background: {row:?}"
        );
    }
    assert!(strip[0].text().starts_with("› a question"));
    assert!(
        strip[1].text().starts_with("  "),
        "continuation hangs under the text"
    );
}

#[test]
fn assistant_answer_is_unmarked_and_code_copies_clean() {
    let mut app = app();
    app.push_agent_event(&AgentEvent::FinalContentDelta(
        "Run this:\n```rust\nfn main() {}\n```".to_string(),
    ));
    let rows: Vec<String> = app.transcript_lines(80).iter().map(Line::text).collect();
    assert_eq!(rows[0], "  Run this:", "no marker, two-column indent");
    assert_eq!(rows[1], "  ```rust", "fences stay literal");
    assert_eq!(rows[2], "  fn main() {}", "code has no gutter glyph");
    assert_eq!(rows[3], "  ```");
}

#[test]
fn answer_trailing_newlines_add_no_extra_blank_rows() {
    let mut app = app();
    app.push_agent_event(&AgentEvent::FinalContentDelta("done.\n\n".to_string()));
    let rows: Vec<String> = app.transcript_lines(80).iter().map(Line::text).collect();
    // The answer row, then exactly one separator row.
    assert_eq!(rows, vec!["  done.".to_string(), String::new()]);
}

#[test]
fn ordered_list_items_hang_under_their_text() {
    let mut app = app();
    app.push_agent_event(&AgentEvent::FinalContentDelta(
        "12. alpha beta gamma delta".to_string(),
    ));
    let rows: Vec<String> = app.transcript_lines(16).iter().map(Line::text).collect();
    assert_eq!(rows[0], "  12. alpha");
    assert!(
        rows[1].starts_with("      "),
        "hangs under the item text: {rows:?}"
    );
}

#[test]
fn reasoning_is_never_italic() {
    let mut app = app();
    app.push_agent_event(&AgentEvent::Thinking("weighing options".to_string()));
    app.push_agent_event(&AgentEvent::FinalContentDelta("done".to_string()));
    let lines = app.transcript_lines(80);
    assert!(lines_text(&lines).contains("weighing options"));
    let reasoning: Vec<&Line> = lines
        .iter()
        .take_while(|line| !line.text().contains("done"))
        .collect();
    assert!(
        reasoning
            .iter()
            .flat_map(|line| &line.spans)
            .all(|span| !span.style.italic && !span.style.dim),
        "reasoning must use an explicit muted color, not italic/dim: {reasoning:?}"
    );
}

#[test]
fn live_reasoning_shows_a_timer_and_only_the_newest_rows() {
    let mut app = app();
    app.set_busy(true);
    let text: String = (1..=9).map(|i| format!("step {i}\n")).collect();
    app.push_agent_event(&AgentEvent::Thinking(text));
    let panel = lines_text(&app.frame(80, 30));
    assert!(panel.contains("∴ Thinking… 0s"), "{panel}");
    assert!(
        panel.contains("step 9") && panel.contains("step 6"),
        "{panel}"
    );
    assert!(
        !panel.contains("step 5"),
        "older rows scroll out of the live tail: {panel}"
    );
}

#[test]
fn run_end_closes_a_reasoning_block_that_was_still_streaming() {
    let mut app = app();
    app.set_busy(true);
    app.push_agent_event(&AgentEvent::Thinking("half a thought".to_string()));
    app.set_busy(false);
    let flushed = lines_text(&app.transcript_lines(80));
    assert!(flushed.contains("∴ Thought for"), "{flushed}");
    assert!(
        !flushed.contains("Thinking…"),
        "no live header in history: {flushed}"
    );
}

#[test]
fn status_row_shows_git_branch_and_session_usage() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir(workspace.path().join(".git")).unwrap();
    std::fs::write(
        workspace.path().join(".git").join("HEAD"),
        "ref: refs/heads/feature/tui\n",
    )
    .unwrap();
    let mut app = ChatApp::new("deepseek/deepseek-v4-pro", workspace.path());
    let status = app.frame(120, 10).pop().unwrap().text();
    assert!(status.contains("(feature/tui)"), "{status:?}");
    assert!(!status.contains('↑'), "no usage before the first response");

    app.set_busy(true);
    app.push_agent_event(&AgentEvent::UsageUpdated(harness_cli::agent::UsageTotals {
        requests: 1,
        prompt_tokens: 12_300,
        cached_tokens: 6_150,
        completion_tokens: 3_400,
        ..Default::default()
    }));
    app.set_busy(false);
    let status = app.frame(120, 10).pop().unwrap().text();
    assert!(status.contains("↑12.3k ↓3.4k · cache 50%"), "{status:?}");
}

fn context_usage(app: &mut ChatApp, context_tokens: u64) {
    app.set_busy(true);
    app.push_agent_event(&AgentEvent::UsageUpdated(harness_cli::agent::UsageTotals {
        requests: 1,
        prompt_tokens: context_tokens,
        context_tokens,
        ..Default::default()
    }));
    app.set_busy(false);
}

fn status(app: &mut ChatApp, width: usize) -> Line {
    app.frame(width, 10).pop().unwrap()
}

/// Color of the context bar: its first span after the model and usage.
fn bar_color(line: &Line) -> Color {
    line.spans
        .iter()
        .find(|span| span.text.contains('█'))
        .map(|span| span.style.fg)
        .expect("a filled bar cell")
}

#[test]
fn status_row_shows_how_full_the_context_window_is() {
    let mut app = app();
    app.set_context_window(Some(1_048_576));
    let before = status(&mut app, 140).text();
    assert!(
        !before.contains("1M"),
        "no meter before the first response: {before:?}"
    );

    context_usage(&mut app, 474_000);
    let row = status(&mut app, 140);
    let text = row.text();
    assert!(text.ends_with("████▌░░░░░ 474k/1M 45% "), "{text:?}");
    assert!(text.contains("deepseek/deepseek-v4-pro"), "{text:?}");
    assert_eq!(row.width(), 140);
}

#[test]
fn the_context_bar_turns_warning_then_error_as_it_fills() {
    let theme = harness_tui::theme::Theme::dark();
    let mut app = app();
    app.set_context_window(Some(200_000));
    context_usage(&mut app, 50_000);
    assert_eq!(bar_color(&status(&mut app, 140)), theme.success);
    context_usage(&mut app, 140_000);
    assert_eq!(bar_color(&status(&mut app, 140)), theme.warning);
    context_usage(&mut app, 190_000);
    let row = status(&mut app, 140);
    assert_eq!(bar_color(&row), theme.error);
    assert!(row.text().contains("190k/200k 95%"), "{:?}", row.text());
}

#[test]
fn context_without_a_known_window_shows_the_token_count() {
    let mut app = app();
    context_usage(&mut app, 45_210);
    let text = status(&mut app, 140).text();
    assert!(text.ends_with("ctx 45.2k "), "{text:?}");
    assert!(!text.contains('░'), "no bar without a window: {text:?}");
}

#[test]
fn a_narrow_status_row_keeps_the_context_meter_over_session_totals() {
    let mut app = app();
    app.set_context_window(Some(1_048_576));
    context_usage(&mut app, 474_000);
    let text = status(&mut app, 70).text();
    assert!(text.contains("474k/1M 45%"), "{text:?}");
    assert!(!text.contains('↑'), "session totals go first: {text:?}");
}

#[test]
fn a_new_session_resets_the_context_meter() {
    let mut app = app();
    app.set_context_window(Some(1_048_576));
    context_usage(&mut app, 474_000);
    submit_message(&mut app, "/new");
    let text = status(&mut app, 140).text();
    assert!(!text.contains("474k"), "{text:?}");
}

#[test]
fn busy_panel_shows_working_row_with_spacer_and_live_entry() {
    let mut app = app();
    app.push_user_message("long task");
    app.set_busy(true);

    let lines = app.frame(100, 20);
    let panel = lines_text(&lines);
    assert!(panel.contains("long task"), "live entry stays visible");

    let rows: Vec<String> = lines.iter().map(|line| line.text()).collect();
    let working = rows
        .iter()
        .position(|row| row.contains("Working… (0s"))
        .expect("busy panel must show the Working row");
    assert!(
        rows[working - 1].trim().is_empty(),
        "expected a blank spacer above the Working row, got: {:?}",
        rows[working - 1]
    );
}

#[test]
fn panel_caps_live_rows_to_the_tail() {
    let mut app = app();
    for i in 0..30 {
        app.push_system_line(format!("note {i}"));
    }
    let panel = lines_text(&app.frame(100, 10));
    assert!(!panel.contains("note 0"), "old rows drop off the cap");
    assert!(panel.contains("note 29"), "the newest rows stay visible");
}

// --- transcript rows ---

#[test]
fn transcript_lines_put_a_blank_line_after_each_entry() {
    let mut app = app();
    app.push_user_message("hello");
    app.push_system_line("note");

    let lines = app.transcript_lines(80);
    let texts: Vec<String> = lines.iter().map(|line| line.text()).collect();
    assert_eq!(texts.iter().filter(|text| text.is_empty()).count(), 2);
    assert!(texts.last().unwrap().is_empty(), "trailing blank line");
    // The transcript is not consumed: the exit print sees it all again.
    assert_eq!(app.transcript_lines(80), lines);
}

// --- reasoning display ---

fn thinking_steps(app: &mut ChatApp, steps: usize) {
    let text: String = (1..=steps).map(|i| format!("step {i}\n")).collect();
    app.push_agent_event(&AgentEvent::Thinking(text));
    app.push_agent_event(&AgentEvent::FinalContentDelta("answer".to_string()));
}

#[test]
fn finished_reasoning_keeps_its_newest_four_rows_under_the_header() {
    let mut app = app();
    thinking_steps(&mut app, 9);
    let rows: Vec<String> = app.transcript_lines(80).iter().map(Line::text).collect();
    let header = rows
        .iter()
        .position(|row| row.starts_with("∴ Thought for"))
        .expect("header");
    assert!(
        rows[header].ends_with("· +5 lines · ctrl+o to expand"),
        "{:?}",
        rows[header]
    );
    assert_eq!(rows[header + 1].trim(), "step 6");
    assert_eq!(rows[header + 4].trim(), "step 9");
    assert!(!rows.iter().any(|row| row.trim() == "step 5"), "{rows:?}");
    // The plain projection (traces, non-TTY) keeps the full text.
    assert!(app.transcript_text().contains("step 1"));
}

#[test]
fn short_reasoning_needs_no_expand_hint() {
    let mut app = app();
    thinking_steps(&mut app, 2);
    let text = lines_text(&app.transcript_lines(80));
    assert!(text.contains("step 1") && text.contains("step 2"), "{text}");
    assert!(!text.contains("ctrl+o"), "{text}");
}

#[test]
fn ctrl_o_expands_every_reasoning_block_in_place_and_folds_it_back() {
    let mut app = app();
    thinking_steps(&mut app, 9);
    let folded = app.transcript_lines(80);
    let entries = app.transcript_len();

    app.handle_key(KeyEvent::ctrl(KeyCode::Char('o')));
    let expanded = lines_text(&app.transcript_lines(80));
    assert_eq!(
        app.transcript_len(),
        entries,
        "no new entry, the same block grows"
    );
    assert!(
        expanded.contains("step 1") && expanded.contains("step 9"),
        "{expanded}"
    );
    assert!(expanded.contains("ctrl+o to fold"), "{expanded}");
    assert!(
        lines_text(&app.frame(80, 30)).contains("reasoning expanded"),
        "the status row confirms the toggle"
    );

    app.handle_key(KeyEvent::ctrl(KeyCode::Char('o')));
    assert_eq!(app.transcript_lines(80), folded);
}

#[test]
fn the_exit_transcript_drops_key_hints_and_reveals_everything() {
    let mut app = app().with_animations(true);
    thinking_steps(&mut app, 9);
    let printed = lines_text(&app.exit_transcript(80));
    assert!(printed.contains("∴ Thought for"), "{printed}");
    assert!(printed.contains("+5 lines"), "{printed}");
    assert!(
        !printed.contains("ctrl+o"),
        "keys do not work after exit: {printed}"
    );
    assert!(
        printed.contains("answer"),
        "animated text is fully shown: {printed}"
    );
    // The live screen keeps its hints.
    assert!(lines_text(&app.transcript_lines(80)).contains("ctrl+o to expand"));
}

#[test]
fn slash_thinking_hides_reasoning_and_shows_it_again() {
    let mut app = app();
    thinking_steps(&mut app, 3);
    submit_message(&mut app, "/thinking");
    let hidden = lines_text(&app.transcript_lines(80));
    assert!(
        !hidden.contains("step 1") && !hidden.contains("∴"),
        "{hidden}"
    );
    assert!(hidden.contains("thinking: hidden"), "{hidden}");
    assert!(hidden.contains("answer"));

    submit_message(&mut app, "/thinking");
    let shown = lines_text(&app.transcript_lines(80));
    assert!(
        shown.contains("step 1") && shown.contains("∴ Thought for"),
        "{shown}"
    );
}

// --- full-screen frame ---

#[test]
fn frame_fills_the_screen_with_the_status_row_on_the_bottom_edge() {
    let mut app = app();
    app.push_banner("0.1.0");
    app.push_user_message("hello");
    let frame = app.frame(60, 20);
    assert_eq!(frame.len(), 20);
    assert!(
        frame[0].text().starts_with("harness"),
        "banner on top: {:?}",
        frame[0].text()
    );
    assert!(frame[19].text().contains("deepseek/deepseek-v4-pro"));
    let editor = frame
        .iter()
        .position(|line| line.text().contains("Ask anything"))
        .unwrap();
    assert_eq!(editor, 17, "editor right above its rule and the status row");
}

fn long_app(rows: usize) -> ChatApp {
    let mut app = app();
    for i in 0..rows {
        app.push_system_line(format!("note {i}"));
    }
    app
}

#[test]
fn wheel_and_page_keys_scroll_the_transcript_and_the_rule_says_how_far() {
    let mut app = long_app(40);
    let bottom = lines_text(&app.frame(60, 20));
    assert!(
        bottom.contains("note 39") && !bottom.contains("note 20"),
        "{bottom}"
    );

    app.handle_event(Event::WheelUp);
    let scrolled = lines_text(&app.frame(60, 20));
    assert!(!scrolled.contains("note 39"), "{scrolled}");
    assert!(scrolled.contains("↓ 3 more rows · PgDn"), "{scrolled}");

    app.handle_key(key(KeyCode::PageUp));
    let paged = lines_text(&app.frame(60, 20));
    // A page is the transcript area (16 rows here) minus two rows of overlap.
    assert!(paged.contains("↓ 17 more rows"), "{paged}");

    app.handle_key(key(KeyCode::PageDown));
    app.handle_event(Event::WheelDown);
    app.handle_event(Event::WheelDown);
    let back = lines_text(&app.frame(60, 20));
    assert!(
        back.contains("note 39") && !back.contains("more rows"),
        "{back}"
    );
}

#[test]
fn a_scrolled_view_stays_put_while_new_rows_arrive_and_submit_jumps_down() {
    let mut app = long_app(40);
    app.frame(60, 20);
    for _ in 0..3 {
        app.handle_event(Event::WheelUp);
    }
    let before = app.frame(60, 20);
    app.push_system_line("fresh news");
    let after = app.frame(60, 20);
    let rows = |frame: &[Line]| frame[..10].iter().map(Line::text).collect::<Vec<_>>();
    assert_eq!(rows(&before), rows(&after), "the visible rows do not move");
    assert!(!lines_text(&after).contains("fresh news"));

    submit_message(&mut app, "next question");
    let jumped = lines_text(&app.frame(60, 20));
    assert!(jumped.contains("next question"), "{jumped}");
}

#[test]
fn scrolling_past_either_end_is_clamped() {
    let mut app = long_app(40);
    for _ in 0..100 {
        app.handle_event(Event::WheelUp);
    }
    let top = lines_text(&app.frame(60, 20));
    assert!(top.starts_with("· note 0"), "{top}");
    for _ in 0..100 {
        app.handle_event(Event::WheelDown);
    }
    assert!(lines_text(&app.frame(60, 20)).contains("note 39"));
}

// --- mouse selection ---

fn mouse(kind: MouseKind, column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent { kind, column, row })
}

#[test]
fn dragging_over_the_transcript_copies_the_selected_text() {
    let mut app = app();
    app.push_system_line("alpha beta");
    app.push_system_line("gamma delta");
    let frame = app.frame(60, 20);
    let first = frame
        .iter()
        .position(|line| line.text() == "· alpha beta")
        .unwrap() as u16;
    let second = frame
        .iter()
        .position(|line| line.text() == "· gamma delta")
        .unwrap() as u16;

    assert_eq!(
        app.handle_event(mouse(MouseKind::Down, 8, first)),
        ChatAction::Continue
    );
    assert_eq!(
        app.handle_event(mouse(MouseKind::Drag, 6, second)),
        ChatAction::Continue
    );
    let action = app.handle_event(mouse(MouseKind::Up, 6, second));
    assert_eq!(action, ChatAction::Copy("beta\n\n· gamma".to_string()));

    // The selection is painted and the status row confirms the copy.
    let frame = app.frame(60, 20);
    let lit = &frame[first as usize];
    assert!(
        lit.spans
            .iter()
            .any(|span| span.text.contains("beta") && span.style.bg != Color::Default),
        "{lit:?}"
    );
    assert!(lines_text(&frame).contains("copied 13 chars"));

    // Typing clears the highlight.
    app.handle_key(key(KeyCode::Char('x')));
    let frame = app.frame(60, 20);
    assert!(
        frame[first as usize]
            .spans
            .iter()
            .all(|span| span.style.bg == Color::Default)
    );
}

#[test]
fn a_click_without_a_drag_copies_nothing() {
    let mut app = app();
    app.push_system_line("alpha");
    app.frame(60, 20);
    app.handle_event(mouse(MouseKind::Down, 3, 4));
    assert_eq!(
        app.handle_event(mouse(MouseKind::Up, 3, 4)),
        ChatAction::Continue
    );
}

#[test]
fn a_drag_that_starts_on_the_panel_selects_nothing() {
    let mut app = app();
    app.frame(60, 20);
    app.handle_event(mouse(MouseKind::Down, 3, 19));
    app.handle_event(mouse(MouseKind::Drag, 9, 2));
    assert_eq!(
        app.handle_event(mouse(MouseKind::Up, 9, 2)),
        ChatAction::Continue
    );
}

#[test]
fn right_click_pastes_like_the_terminal_does() {
    let mut app = app();
    assert_eq!(
        app.handle_event(mouse(MouseKind::RightDown, 0, 0)),
        ChatAction::CaptureClipboard
    );
}

// --- paced reveal ---

#[test]
fn streamed_text_is_revealed_over_frames_and_fades_in() {
    use std::time::{Duration, Instant};
    let mut app = app()
        .with_animations(true)
        .with_terminal_colors(Some((220, 220, 220)), Some((10, 10, 10)));
    let answer = "word ".repeat(120);
    app.set_busy(true);
    app.push_agent_event(&AgentEvent::FinalContentDelta(answer.clone()));

    let t0 = Instant::now();
    app.animate(t0);
    app.animate(t0 + Duration::from_millis(33));
    let early = lines_text(&app.transcript_lines(200));
    let shown = early.matches("word").count();
    assert!(shown > 0 && shown < 120, "a slice is visible: {shown}");
    assert!(app.is_animating());

    // The newest characters are tinted toward the background.
    let rows = app.transcript_lines(200);
    let last = rows
        .iter()
        .rev()
        .find(|line| !line.text().trim().is_empty())
        .unwrap();
    assert!(
        last.spans
            .iter()
            .any(|span| matches!(span.style.fg, Color::Rgb(..))),
        "{last:?}"
    );

    let mut now = t0 + Duration::from_millis(33);
    for _ in 0..200 {
        now += Duration::from_millis(33);
        app.animate(now);
    }
    app.set_busy(false);
    let done = lines_text(&app.transcript_lines(200));
    assert_eq!(done.matches("word").count(), 120);
    assert!(!app.is_animating(), "nothing left to animate");
}

#[test]
fn without_animations_every_delta_shows_at_once() {
    let mut app = app();
    app.push_agent_event(&AgentEvent::FinalContentDelta("all of it".to_string()));
    assert!(lines_text(&app.transcript_lines(80)).contains("all of it"));
}

// --- plain projection ---

#[test]
fn transcript_text_uses_plain_projection() {
    let mut app = app();
    app.push_user_message("hello");
    app.push_agent_event(&AgentEvent::ToolResult(ok_result(
        "c1",
        "file.write",
        "wrote 12 bytes",
    )));
    app.push_agent_event(&AgentEvent::FinalContentDelta("# Title".to_string()));

    let transcript = app.transcript_text();
    assert!(transcript.contains("you: hello"));
    assert!(transcript.contains("✓ file.write — wrote 12 bytes"));
    // The plain projection keeps raw markdown.
    assert!(transcript.contains("# Title"));
}

// --- busy input policy ---

#[test]
fn busy_input_cancels_on_esc_and_ctrl_c_and_forwards_everything_else() {
    let app = app();
    let esc = Event::Key(KeyEvent::plain(KeyCode::Esc));
    assert_eq!(app.busy_action(&esc), BusyAction::Cancel);
    assert_eq!(
        app.busy_action(&Event::Key(KeyEvent::ctrl(KeyCode::Char('c')))),
        BusyAction::Cancel
    );
    for event in [
        Event::Key(KeyEvent::plain(KeyCode::Char('x'))),
        Event::Key(KeyEvent::plain(KeyCode::Enter)),
        Event::WheelUp,
        Event::Paste("text".to_string()),
    ] {
        assert_eq!(app.busy_action(&event), BusyAction::Forward, "{event:?}");
    }
}

#[test]
fn esc_closes_an_open_menu_instead_of_cancelling_the_run() {
    let mut app = app();
    app.set_busy(true);
    type_text(&mut app, "/");
    assert!(app.completion_visible());
    let esc = Event::Key(KeyEvent::plain(KeyCode::Esc));
    assert_eq!(app.busy_action(&esc), BusyAction::Forward);
    app.handle_key(KeyEvent::plain(KeyCode::Esc));
    assert!(!app.completion_visible());
    assert_eq!(app.busy_action(&esc), BusyAction::Cancel);
}

#[test]
fn enter_while_busy_queues_the_prompt_and_sends_it_after_the_run() {
    let mut app = app();
    submit_message(&mut app, "first task");
    app.set_busy(true);
    let action = submit_message(&mut app, "and then the docs");
    assert_eq!(action, ChatAction::Continue, "nothing is sent mid-run");
    assert_eq!(app.queued_len(), 1);
    assert_eq!(app.input(), "", "the editor is free for the next draft");
    let panel = lines_text(&app.frame(80, 20));
    assert!(panel.contains("⏵ queued and then the docs"), "{panel}");
    assert!(
        panel.contains("Queue a follow-up"),
        "busy placeholder: {panel}"
    );
    assert_eq!(app.take_queued(), None, "the queue waits for the run");

    app.set_busy(false);
    assert_eq!(
        app.take_queued(),
        Some(ChatAction::Submit("and then the docs".to_string()))
    );
    assert_eq!(app.queued_len(), 0);
    assert!(app.transcript_text().contains("you: and then the docs"));
}

#[test]
fn local_commands_run_at_once_while_busy() {
    let mut app = app();
    app.set_busy(true);
    submit_message(&mut app, "/cost");
    assert_eq!(app.queued_len(), 0);
    assert!(app.transcript_text().contains("session usage"));
    // Commands that touch the session wait for the run.
    submit_message(&mut app, "/new");
    assert_eq!(app.queued_len(), 1);
}

#[test]
fn an_interrupted_run_hands_queued_prompts_back_to_the_editor() {
    let mut app = app();
    app.set_busy(true);
    submit_message(&mut app, "step one");
    submit_message(&mut app, "step two");
    type_text(&mut app, "draft");
    app.set_busy(false);
    app.restore_queued_to_editor();
    assert_eq!(app.queued_len(), 0);
    assert_eq!(app.input(), "step one\nstep two\ndraft");
    assert_eq!(app.take_queued(), None);
}

#[test]
fn modified_enter_and_ctrl_j_compose_newlines() {
    use harness_tui::input::Modifiers;
    let mut app = app();
    type_text(&mut app, "a");
    for mods in [
        Modifiers {
            shift: true,
            ..Default::default()
        },
        Modifiers {
            ctrl: true,
            ..Default::default()
        },
    ] {
        let action = app.handle_key(KeyEvent {
            code: KeyCode::Enter,
            mods,
        });
        assert_eq!(action, ChatAction::Continue);
    }
    app.handle_key(KeyEvent::ctrl(KeyCode::Char('j')));
    assert_eq!(app.input(), "a\n\n\n");
}

#[test]
fn ctrl_w_delete_and_kill_line_edit_the_prompt() {
    let mut app = app();
    type_text(&mut app, "fix the bug");
    app.handle_key(KeyEvent::ctrl(KeyCode::Char('w')));
    assert_eq!(app.input(), "fix the ");
    app.handle_key(KeyEvent::ctrl(KeyCode::Backspace));
    assert_eq!(app.input(), "fix ");
    app.handle_key(KeyEvent::ctrl(KeyCode::Char('a')));
    app.handle_key(KeyEvent::plain(KeyCode::Delete));
    assert_eq!(app.input(), "ix ");
    app.handle_key(KeyEvent::ctrl(KeyCode::Char('k')));
    assert_eq!(app.input(), "");
}

#[test]
fn up_moves_inside_a_multiline_prompt_before_recalling_history() {
    let mut app = app();
    submit_message(&mut app, "older prompt");
    type_text(&mut app, "line one");
    app.handle_key(KeyEvent::alt(KeyCode::Enter));
    type_text(&mut app, "two");
    app.handle_key(KeyEvent::plain(KeyCode::Up));
    assert_eq!(
        app.input(),
        "line one\ntwo",
        "first Up moves the caret, keeps the draft"
    );
    app.handle_key(KeyEvent::plain(KeyCode::Up));
    assert_eq!(
        app.input(),
        "older prompt",
        "Up on the first line recalls history"
    );
}

// --- /provider add ---

#[test]
fn provider_add_opens_the_wizard_fetches_and_saves() {
    let mut app = app();
    assert_eq!(
        submit_message(&mut app, "/provider add"),
        ChatAction::Continue
    );
    assert!(app.provider_wizard_open());
    assert!(lines_text(&app.frame(100, 40)).contains("Select provider"));

    app.handle_key(key(KeyCode::Up)); // wraps to custom
    app.handle_key(key(KeyCode::Enter));
    type_text(&mut app, "router");
    app.handle_key(key(KeyCode::Enter));
    type_text(&mut app, "https://router.example/v1");
    app.handle_key(key(KeyCode::Enter));
    // A pasted key keeps out of the chat editor.
    app.handle_paste("sk-router\n");
    let ChatAction::FetchModels(provider) = app.handle_key(key(KeyCode::Enter)) else {
        panic!("the key step must ask the loop for the model list");
    };
    assert_eq!(provider.api_key(), "sk-router");
    assert!(lines_text(&app.frame(100, 40)).contains("Fetching models"));

    let discovery = harness_cli::providers::ModelDiscovery::from_openai_compatible_response(
        &provider,
        r#"{"data":[{"id":"m-1"},{"id":"m-2","context_length":64000}]}"#,
    )
    .unwrap();
    app.models_loaded(Ok((provider, discovery)));
    app.handle_key(key(KeyCode::Down)); // Add all -> m-1
    app.handle_key(key(KeyCode::Down)); // m-1 -> m-2
    let ChatAction::SaveProvider { provider, model } = app.handle_key(key(KeyCode::Enter)) else {
        panic!("Enter on a model must save");
    };
    assert_eq!(provider.name(), "router");
    assert_eq!(model, "m-2");
    assert!(!app.provider_wizard_open());
    // The editor never saw the wizard's typing.
    assert_eq!(submit_message(&mut app, ""), ChatAction::Continue);
}

#[test]
fn esc_closes_the_provider_wizard_without_arming_exit() {
    let mut app = app();
    submit_message(&mut app, "/provider add");
    assert_eq!(app.handle_key(key(KeyCode::Esc)), ChatAction::Continue);
    assert!(!app.provider_wizard_open());
    // The next Esc arms the exit confirmation; it does not exit yet.
    assert_eq!(app.handle_key(key(KeyCode::Esc)), ChatAction::Continue);
}

#[test]
fn provider_add_waits_for_a_busy_run_while_plain_provider_runs_at_once() {
    let mut app = app();
    app.set_busy(true);
    submit_message(&mut app, "/provider add");
    assert!(
        !app.provider_wizard_open(),
        "a fetch cannot run during a busy turn"
    );
    assert_eq!(app.queued_len(), 1);

    submit_message(&mut app, "/provider");
    assert_eq!(app.queued_len(), 1, "the listing still runs while busy");
}

#[test]
fn cost_command_uses_prices_saved_from_the_model_list() {
    let mut app = ChatApp::new("router/m-2", "C:/work/project");
    app.set_pricing(Some(harness_cli::providers::ModelPricing {
        input_per_mtok: 1.0,
        cached_input_per_mtok: 1.0,
        output_per_mtok: 2.0,
        as_of: "2026-10-09".to_string(),
    }));
    app.set_busy(true);
    app.push_agent_event(&AgentEvent::UsageUpdated(harness_cli::agent::UsageTotals {
        requests: 1,
        prompt_tokens: 1_000_000,
        cached_tokens: 0,
        completion_tokens: 1_000_000,
        ..Default::default()
    }));
    app.set_busy(false);

    submit_message(&mut app, "/cost");
    let text = lines_text(&app.transcript_lines(120));
    assert!(text.contains("$3.0000"), "{text}");
    assert!(text.contains("2026-10-09"), "{text}");
}
