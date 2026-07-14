//! Tests for the pure chat state machine (`harness_cli::chat::ChatApp`) built
//! on the in-repo `harness-tui` library. Ports the spirit of the removed
//! `tests/chat_tui.rs` suite onto the new pure line-based API.

use harness_cli::agent::AgentEvent;
use harness_cli::chat::{BusyAction, ChatAction, ChatApp, busy_action};
use harness_cli::runtime::ToolBatchResult;
use harness_tui::input::{Event, KeyCode, KeyEvent};
use harness_tui::text::Line;
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
fn esc_exits_when_no_completion_is_open() {
    let mut app = app();
    assert_eq!(app.handle_key(key(KeyCode::Esc)), ChatAction::Exit);
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

// --- slash commands ---

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
    assert!(panel.contains("→ deepseek-v4-pro [deepseek] ✓"));
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
    assert!(panel.contains("→ glm-5.2 [glm] ✓"));
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
    assert!(panel.contains("Type a message"));
    assert!(panel.contains("deepseek/deepseek-v4-pro"));
    assert!(panel.contains("C:/work/project"));

    // The editor prompt row keeps the "> " prefix once text is typed.
    type_text(&mut app, "next question");
    let panel = lines_text(&app.panel_lines(100, 20));
    assert!(panel.contains("> next question"));
}

#[test]
fn editor_is_framed_in_a_rounded_border() {
    let mut app = app();
    let rows: Vec<String> = app
        .panel_lines(40, 20)
        .iter()
        .map(|line| line.text())
        .collect();
    let top = rows
        .iter()
        .position(|row| row.starts_with('╭'))
        .expect("top border row");
    assert_eq!(rows[top].chars().count(), 40, "border spans the full width");
    assert!(rows[top].ends_with('╮'));
    // The editor row sits inside the frame, padded out to the right edge.
    assert!(
        rows[top + 1].starts_with("│ > "),
        "editor row must be framed: {:?}",
        rows[top + 1]
    );
    assert!(rows[top + 1].ends_with('│'));
    assert_eq!(rows[top + 1].chars().count(), 40, "row padded to the frame");
    assert!(rows[top + 2].starts_with('╰'));
    assert!(rows[top + 2].ends_with('╯'));

    // Typed text stays inside the frame.
    type_text(&mut app, "hello");
    let rows: Vec<String> = app
        .panel_lines(40, 20)
        .iter()
        .map(|line| line.text())
        .collect();
    let row = rows
        .iter()
        .find(|row| row.contains("> hello"))
        .expect("typed row");
    assert!(row.starts_with('│') && row.ends_with('│'), "{row:?}");
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
        .position(|row| row.contains("Working… (0s)"))
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
    assert!(flushed.contains("> hello"), "user turn flushes: {flushed}");
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
    assert!(flushed.contains("> question"));
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
fn busy_action_cancels_on_esc_and_ctrl_c_only() {
    assert_eq!(
        busy_action(&Event::Key(KeyEvent::plain(KeyCode::Esc))),
        BusyAction::Cancel
    );
    assert_eq!(
        busy_action(&Event::Key(KeyEvent::ctrl(KeyCode::Char('c')))),
        BusyAction::Cancel
    );
    assert_eq!(
        busy_action(&Event::Key(KeyEvent::plain(KeyCode::Char('x')))),
        BusyAction::Ignore
    );
    assert_eq!(busy_action(&Event::WheelUp), BusyAction::Ignore);
    assert_eq!(
        busy_action(&Event::Paste("text".to_string())),
        BusyAction::Ignore
    );
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
