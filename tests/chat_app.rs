//! Tests for the pure chat state machine (`harness_cli::chat::ChatApp`) built
//! on the in-repo `harness-tui` library. Ports the spirit of the removed
//! `tests/chat_tui.rs` suite onto the new pure line-based API.

use harness_cli::agent::AgentEvent;
use harness_cli::chat::{BusyAction, ChatAction, ChatApp};
use harness_cli::runtime::ToolBatchResult;
use harness_tui::input::{Event, KeyCode, KeyEvent};
use harness_tui::text::{Color, Line};
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
    }));
    app.set_busy(false);

    let action = submit_message(&mut app, "/cost");
    assert_eq!(action, ChatAction::Continue);

    let (lines, _) = app.peek_scrollback(120);
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
    }));
    app.set_busy(false);

    submit_message(&mut app, "/cost");
    let (lines, _) = app.peek_scrollback(120);
    let text = lines_text(&lines);
    assert!(text.contains("500"), "{text}");
    assert!(
        text.contains("no built-in pricing"),
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
    let status = lines_text(&app.panel_lines(120, 10));
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
    let status = lines_text(&app.panel_lines(120, 10));
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
    let text = lines_text(&app.take_scrollback(80));
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

// --- resize repaint: re-wrap the emitted tail at the new width ---

#[test]
fn viewport_tail_rewraps_emitted_entries_and_excludes_live_ones() {
    let mut app = app();
    submit_message(&mut app, "first message with quite a few words in it");
    let (_, through) = app.peek_scrollback(80);
    app.acknowledge_emitted(through);
    // A second entry that was never flushed must not appear: the panel
    // renders it as the live tail, so the repaint would duplicate it.
    submit_message(&mut app, "second unflushed");

    let tail = app.viewport_tail(20, 30);
    let text = lines_text(&tail);
    assert!(text.contains("first message"), "{text}");
    assert!(!text.contains("second unflushed"), "{text}");
    // Re-wrapped to the new width: no rendered line exceeds 20 columns.
    for line in &tail {
        assert!(
            line.text().chars().count() <= 20,
            "line wider than 20: {:?}",
            line.text()
        );
    }
}

#[test]
fn viewport_tail_returns_only_the_last_rows() {
    let mut app = app();
    submit_message(&mut app, "oldest entry");
    submit_message(&mut app, "newest entry");
    let (_, through) = app.peek_scrollback(80);
    app.acknowledge_emitted(through);

    let tail = app.viewport_tail(80, 2);
    assert!(tail.len() <= 2, "got {} lines", tail.len());
    let text = lines_text(&tail);
    assert!(text.contains("newest"), "{text}");
    assert!(!text.contains("oldest"), "{text}");
}

// --- slash commands ---

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
    let (lines, _) = app.peek_scrollback(120);
    let text = lines_text(&lines);
    assert!(text.contains("retain thinking: off"), "{text}");

    let action = submit_message(&mut app, "/retain-thinking");
    assert_eq!(action, ChatAction::Continue);
    assert!(app.retain_thinking());
    let (lines, _) = app.peek_scrollback(120);
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
    let panel = lines_text(&app.panel_lines(100, 30));
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
    let panel = lines_text(&app.panel_lines(100, 30));
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
    let panel = lines_text(&app.panel_lines(100, 30));
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
fn slash_new_returns_new_session_and_resets_emitted() {
    let mut app = app();
    app.push_user_message("old turn");
    app.take_scrollback(80);
    assert!(app.emitted() > 0);

    let action = submit_message(&mut app, "/new");
    assert_eq!(action, ChatAction::NewSession);
    assert!(app.transcript_text().is_empty());
    assert_eq!(app.emitted(), 0);
    assert_eq!(app.input(), "");
}

#[test]
fn slash_clear_empties_transcript_and_resets_emitted() {
    let mut app = app();
    app.push_user_message("hi");
    app.take_scrollback(80);
    let action = submit_message(&mut app, "/clear");
    assert_eq!(action, ChatAction::ClearScreen);
    assert!(app.transcript_text().is_empty());
    assert_eq!(app.emitted(), 0);
}

#[test]
fn slash_help_opens_palette_and_any_key_closes_it() {
    let mut app = app();
    let action = submit_message(&mut app, "/help");
    assert_eq!(action, ChatAction::Continue);
    assert!(app.help_visible());

    // The palette content is part of the pinned panel.
    let panel = lines_text(&app.panel_lines(100, 20));
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

fn card_rows(app: &ChatApp, width: usize) -> Vec<String> {
    app.panel_lines(width, 40)
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
    let rows = card_rows(&app, 80);
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
    let rows = card_rows(&app, 80);
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
    let rows = card_rows(&app, 80);
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
    let lines = app.panel_lines(40, 40);
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
    let rows = card_rows(&app, 80);
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
    let lines = app.panel_lines(80, 40);
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
    let flushed: Vec<String> = app.take_scrollback(80).iter().map(Line::text).collect();
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

    let panel = lines_text(&app.panel_lines(100, 50));
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

    let panel = lines_text(&app.panel_lines(120, 60));
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

// --- panel ---

#[test]
fn panel_contains_editor_prompt_and_status_row() {
    let mut app = app();
    let panel = lines_text(&app.panel_lines(100, 20));
    assert!(panel.contains("Ask anything"));
    assert!(panel.contains("deepseek/deepseek-v4-pro"));
    assert!(panel.contains("C:/work/project"));

    // The editor prompt row keeps the "› " prefix once text is typed.
    type_text(&mut app, "next question");
    let panel = lines_text(&app.panel_lines(100, 20));
    assert!(panel.contains("› next question"));
}

#[test]
fn editor_sits_between_full_width_rules_without_side_borders() {
    let mut app = app();
    type_text(&mut app, "hello");
    let rows: Vec<String> = app
        .panel_lines(40, 20)
        .iter()
        .map(|line| line.text())
        .collect();
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
    let lines = app.take_scrollback(30);
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
    let rows: Vec<String> = app.take_scrollback(80).iter().map(Line::text).collect();
    assert_eq!(rows[0], "  Run this:", "no marker, two-column indent");
    assert_eq!(rows[1], "  ```rust", "fences stay literal");
    assert_eq!(rows[2], "  fn main() {}", "code has no gutter glyph");
    assert_eq!(rows[3], "  ```");
}

#[test]
fn answer_trailing_newlines_add_no_extra_blank_rows() {
    let mut app = app();
    app.push_agent_event(&AgentEvent::FinalContentDelta("done.\n\n".to_string()));
    let rows: Vec<String> = app.take_scrollback(80).iter().map(Line::text).collect();
    // The answer row, then exactly one separator row.
    assert_eq!(rows, vec!["  done.".to_string(), String::new()]);
}

#[test]
fn ordered_list_items_hang_under_their_text() {
    let mut app = app();
    app.push_agent_event(&AgentEvent::FinalContentDelta(
        "12. alpha beta gamma delta".to_string(),
    ));
    let rows: Vec<String> = app.take_scrollback(16).iter().map(Line::text).collect();
    assert_eq!(rows[0], "  12. alpha");
    assert!(
        rows[1].starts_with("      "),
        "hangs under the item text: {rows:?}"
    );
}

#[test]
fn reasoning_is_never_italic() {
    let mut app = app();
    submit_message(&mut app, "/thinking");
    app.push_agent_event(&AgentEvent::Thinking("weighing options".to_string()));
    app.push_agent_event(&AgentEvent::FinalContentDelta("done".to_string()));
    let lines = app.take_scrollback(80);
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
    let panel = lines_text(&app.panel_lines(80, 30));
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
fn finished_reasoning_commits_as_one_summary_line() {
    let mut app = app();
    app.set_busy(true);
    app.push_agent_event(&AgentEvent::Thinking("secret plan\nmore".to_string()));
    app.push_agent_event(&AgentEvent::FinalContentDelta("answer".to_string()));
    let flushed = lines_text(&app.take_scrollback(80));
    assert!(
        flushed.contains("∴ Thought for 1s · ctrl+o to expand"),
        "{flushed}"
    );
    assert!(
        !flushed.contains("secret plan"),
        "collapsed by default: {flushed}"
    );
    // The plain projection (traces, non-TTY) keeps the full text.
    assert!(app.transcript_text().contains("thinking: secret plan"));
}

#[test]
fn run_end_closes_a_reasoning_block_that_was_still_streaming() {
    let mut app = app();
    app.set_busy(true);
    app.push_agent_event(&AgentEvent::Thinking("half a thought".to_string()));
    app.set_busy(false);
    let flushed = lines_text(&app.take_scrollback(80));
    assert!(flushed.contains("∴ Thought for"), "{flushed}");
    assert!(
        !flushed.contains("Thinking…"),
        "no live header in history: {flushed}"
    );
}

#[test]
fn ctrl_o_replays_the_latest_reasoning_in_full() {
    let mut app = app();
    app.push_agent_event(&AgentEvent::Thinking("first idea".to_string()));
    app.push_agent_event(&AgentEvent::FinalContentDelta("one".to_string()));
    app.push_agent_event(&AgentEvent::Thinking("second idea".to_string()));
    app.push_agent_event(&AgentEvent::FinalContentDelta("two".to_string()));
    app.take_scrollback(80);
    assert_eq!(
        app.handle_key(KeyEvent::ctrl(KeyCode::Char('o'))),
        ChatAction::Continue
    );
    let replay = lines_text(&app.take_scrollback(80));
    assert!(replay.contains("second idea"), "{replay}");
    assert!(
        !replay.contains("first idea"),
        "only the latest block: {replay}"
    );
    assert!(
        !replay.contains("ctrl+o"),
        "the expanded copy needs no hint: {replay}"
    );
}

#[test]
fn ctrl_o_without_reasoning_says_so() {
    let mut app = app();
    app.handle_key(KeyEvent::ctrl(KeyCode::Char('o')));
    assert!(app.transcript_text().contains("no reasoning to show yet"));
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
    let status = app.panel_lines(120, 10).pop().unwrap().text();
    assert!(status.contains("(feature/tui)"), "{status:?}");
    assert!(!status.contains('↑'), "no usage before the first response");

    app.set_busy(true);
    app.push_agent_event(&AgentEvent::UsageUpdated(harness_cli::agent::UsageTotals {
        requests: 1,
        prompt_tokens: 12_300,
        cached_tokens: 6_150,
        completion_tokens: 3_400,
    }));
    app.set_busy(false);
    let status = app.panel_lines(120, 10).pop().unwrap().text();
    assert!(status.contains("↑12.3k ↓3.4k · cache 50%"), "{status:?}");
}

#[test]
fn busy_panel_shows_working_row_with_spacer_and_live_entry() {
    let mut app = app();
    app.push_user_message("long task");
    app.set_busy(true);

    let lines = app.panel_lines(100, 20);
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
    let panel = lines_text(&app.panel_lines(100, 10));
    assert!(!panel.contains("note 0"), "old rows drop off the cap");
    assert!(panel.contains("note 29"), "the newest rows stay visible");
}

// --- scrollback flushing ---

#[test]
fn long_streaming_answer_freezes_prefix_into_scrollback() {
    let mut app = app();
    app.push_user_message("вопрос");
    app.set_busy(true);
    app.take_scrollback(80);

    let mut delta = String::new();
    for i in 0..30 {
        delta.push_str(&format!("параграф {i}\n\n"));
    }
    app.push_agent_event(&AgentEvent::FinalContentDelta(delta));
    app.freeze_streaming_overflow(80, 10);

    // The frozen prefix flushes to scrollback while STILL busy…
    let (frozen, limit) = app.peek_scrollback(80);
    let frozen_text = lines_text(&frozen);
    assert!(
        frozen_text.contains("параграф 0"),
        "frozen prefix must flush: {frozen_text}"
    );
    app.acknowledge_emitted(limit);

    // …the live tail stays within budget and keeps the newest text…
    let live = lines_text(&app.panel_lines(80, 10));
    assert!(
        live.contains("параграф 29"),
        "newest text stays live: {live}"
    );
    assert!(
        !live.contains("параграф 0"),
        "head must not be live: {live}"
    );

    // …and further deltas keep appending to the live tail.
    app.push_agent_event(&AgentEvent::FinalContentDelta("хвост".to_string()));
    let live = lines_text(&app.panel_lines(80, 10));
    assert!(live.contains("хвост"));

    // The continuation renders without a second "● " bullet.
    app.set_busy(false);
    let (tail, _) = app.peek_scrollback(80);
    assert!(
        !lines_text(&tail).contains('●'),
        "continuation must not repeat the marker: {}",
        lines_text(&tail)
    );
}

#[test]
fn freeze_never_splits_inside_a_code_fence() {
    let mut app = app();
    app.set_busy(true);
    let mut text = String::from("intro\n\n```rust\n");
    for i in 0..30 {
        text.push_str(&format!("let x{i} = {i};\n\n"));
    }
    app.push_agent_event(&AgentEvent::FinalContentDelta(text));
    app.freeze_streaming_overflow(80, 10);

    let (frozen, _) = app.peek_scrollback(80);
    let flushed = lines_text(&frozen);
    // The only safe boundary is right after "intro" — blank lines inside
    // the unclosed fence must not be used.
    assert!(flushed.contains("intro"), "{flushed}");
    assert!(
        !flushed.contains("let x0"),
        "split landed inside the fence: {flushed}"
    );
}

#[test]
fn freeze_without_safe_boundary_leaves_the_entry_alone() {
    let mut app = app();
    app.set_busy(true);
    // One giant paragraph: no blank lines at all.
    let text = "слово ".repeat(2000);
    app.push_agent_event(&AgentEvent::FinalContentDelta(text));
    app.freeze_streaming_overflow(80, 10);
    // Nothing to flush — the entry stays live (head-clip is the backstop).
    let (frozen, _) = app.peek_scrollback(80);
    assert!(frozen.is_empty());
}

#[test]
fn peek_scrollback_is_side_effect_free_until_acknowledged() {
    let mut app = app();
    app.push_user_message("hi");
    app.push_system_line("note");

    let (first, limit) = app.peek_scrollback(80);
    assert!(!first.is_empty());
    assert_eq!(app.emitted(), 0, "peek must not advance emitted");

    // A failed terminal write means the same plan is offered again.
    let (second, limit_again) = app.peek_scrollback(80);
    assert_eq!(limit, limit_again);
    assert_eq!(lines_text(&first), lines_text(&second));

    app.acknowledge_emitted(limit);
    assert_eq!(app.emitted(), limit);
    let (third, _) = app.peek_scrollback(80);
    assert!(third.is_empty());
}

#[test]
fn take_scrollback_flushes_user_and_system_while_busy_but_not_running_tool() {
    let mut app = app();
    app.push_user_message("hello");
    app.push_system_line("note");
    app.push_agent_event(&AgentEvent::ToolCallStarted {
        round: 1,
        id: "c1".to_string(),
        name: "file.read".to_string(),
        arguments: json!({"path": "Cargo.toml"}),
    });
    app.set_busy(true);

    let flushed = lines_text(&app.take_scrollback(80));
    assert!(flushed.contains("› hello"), "user turn flushes: {flushed}");
    assert!(flushed.contains("· note"), "system line flushes");
    assert!(
        !flushed.contains("file.read"),
        "a Running tool card must stay live"
    );
    assert_eq!(app.emitted(), 2);

    // Once the run ends everything flushes.
    app.set_busy(false);
    let flushed = lines_text(&app.take_scrollback(80));
    assert!(flushed.contains("file.read"));
    assert_eq!(app.emitted(), app.transcript_len());
}

#[test]
fn take_scrollback_keeps_streaming_assistant_live_while_busy() {
    let mut app = app();
    app.push_user_message("question");
    app.set_busy(true);
    app.push_agent_event(&AgentEvent::FinalContentDelta("partial answer".to_string()));

    let flushed = lines_text(&app.take_scrollback(80));
    assert!(flushed.contains("› question"));
    assert!(
        !flushed.contains("partial answer"),
        "the streaming tail must stay in the live panel"
    );

    app.set_busy(false);
    let flushed = lines_text(&app.take_scrollback(80));
    assert!(flushed.contains("partial answer"));
    assert_eq!(app.emitted(), app.transcript_len());
}

#[test]
fn take_scrollback_appends_a_blank_line_after_each_entry() {
    let mut app = app();
    app.push_user_message("hello");
    app.push_system_line("note");

    let lines = app.take_scrollback(80);
    let texts: Vec<String> = lines.iter().map(|line| line.text()).collect();
    assert_eq!(texts.iter().filter(|text| text.is_empty()).count(), 2);
    assert!(texts.last().unwrap().is_empty(), "trailing blank line");
    // Nothing left to flush on a second call.
    assert!(app.take_scrollback(80).is_empty());
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
    let panel = lines_text(&app.panel_lines(80, 20));
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
fn stale_running_card_from_cancelled_run_does_not_block_next_flush() {
    let mut app = app();
    app.set_busy(true);
    app.push_agent_event(&AgentEvent::ToolCallStarted {
        round: 1,
        id: "c-stale".to_string(),
        name: "file.read".to_string(),
        arguments: json!({"path": "a.txt"}),
    });
    // Cancelled run: no ToolResult ever arrives for the card.
    app.set_busy(false);
    assert!(!app.take_scrollback(80).is_empty());

    // Next turn: finalized entries must keep flushing while busy even
    // though a flushed Running card exists earlier in the transcript.
    app.push_user_message("next question");
    app.set_busy(true);
    app.push_agent_event(&AgentEvent::FinalContentDelta("thinking...".to_string()));
    let flushed = app.take_scrollback(80);
    assert!(
        flushed
            .iter()
            .any(|line| line.text().contains("next question")),
        "user entry must flush during the next busy turn"
    );
}

// --- editing keys ---

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
