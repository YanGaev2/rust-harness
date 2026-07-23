// Burst/paste coalescing is covered by crates/harness-tui/tests/input.rs
// (coalesce_burst); the busy-input policy lives in tests/chat_app.rs
// (busy_action). This file tests the terminal-agnostic ReplSession state
// machine and the line-mode agent-event renderer.
use harness_cli::agent::AgentEvent;
use harness_cli::clipboard::{AttachmentStore, ClipboardItem, StaticClipboard};
use harness_cli::providers::ProviderConfig;
use harness_cli::repl::{
    ReplAction, ReplEvent, ReplModelSelection, ReplSession, render_agent_event,
    resolve_model_selection,
};
use harness_cli::runtime::ToolBatchResult;
use serde_json::json;

#[test]
fn ctrl_v_text_inserts_text_and_keeps_attachment_for_submit() {
    let root = tempfile::tempdir().unwrap();
    let source = StaticClipboard::new(Some(ClipboardItem::Text("clipboard text".to_string())));
    let mut session = ReplSession::new(AttachmentStore::new(root.path()));

    assert_eq!(
        session
            .handle_event(ReplEvent::Text("ask: ".to_string()), &source)
            .unwrap(),
        ReplAction::Continue
    );
    assert_eq!(
        session.handle_event(ReplEvent::CtrlV, &source).unwrap(),
        ReplAction::Continue
    );
    assert_eq!(session.input(), "ask: clipboard text");

    let action = session.handle_event(ReplEvent::Submit, &source).unwrap();
    let ReplAction::Submit(submission) = action else {
        panic!("expected submit action");
    };

    assert_eq!(submission.message, "ask: clipboard text");
    assert_eq!(submission.attachments.len(), 1);
    assert_eq!(submission.attachments[0].kind, "text");
    assert_eq!(
        std::fs::read_to_string(root.path().join(&submission.attachments[0].relative_path))
            .unwrap(),
        "clipboard text"
    );
    assert_eq!(session.input(), "");
}

#[test]
fn ctrl_v_image_appends_prompt_fragment_and_saves_png_attachment() {
    let root = tempfile::tempdir().unwrap();
    let png = vec![137, 80, 78, 71, 13, 10, 26, 10, 9, 8, 7, 6];
    let source = StaticClipboard::new(Some(ClipboardItem::ImagePng(png.clone())));
    let mut session = ReplSession::new(AttachmentStore::new(root.path()));

    session
        .handle_event(ReplEvent::Text("describe ".to_string()), &source)
        .unwrap();
    session.handle_event(ReplEvent::CtrlV, &source).unwrap();

    assert!(session.input().starts_with("describe "));
    assert!(session.input().contains("image file:"));

    let ReplAction::Submit(submission) = session.handle_event(ReplEvent::Submit, &source).unwrap()
    else {
        panic!("expected submit action");
    };

    assert_eq!(submission.attachments.len(), 1);
    assert_eq!(submission.attachments[0].kind, "image");
    assert_eq!(
        std::fs::read(root.path().join(&submission.attachments[0].relative_path)).unwrap(),
        png
    );
}

#[test]
fn bracketed_paste_inserts_multiline_text_without_submitting() {
    let root = tempfile::tempdir().unwrap();
    let source = StaticClipboard::new(None);
    let mut session = ReplSession::new(AttachmentStore::new(root.path()));

    session
        .handle_event(ReplEvent::Text("note: ".to_string()), &source)
        .unwrap();

    // A real terminal in bracketed-paste mode delivers a whole multi-line paste
    // as a single Paste event. It must NOT be treated as Enter/submit, and the
    // embedded newlines must survive intact.
    let action = session
        .handle_event(ReplEvent::Paste("line one\nline two".to_string()), &source)
        .unwrap();
    assert_eq!(
        action,
        ReplAction::Continue,
        "a paste must never submit the prompt"
    );
    assert_eq!(session.input(), "note: line one\nline two");

    // The explicit Submit afterwards sends the whole multi-line message at once.
    let ReplAction::Submit(submission) = session.handle_event(ReplEvent::Submit, &source).unwrap()
    else {
        panic!("expected submit action");
    };
    assert_eq!(submission.message, "note: line one\nline two");
}

#[test]
fn ctrl_c_exits_repl_without_submit() {
    let root = tempfile::tempdir().unwrap();
    let source = StaticClipboard::new(None);
    let mut session = ReplSession::new(AttachmentStore::new(root.path()));

    assert_eq!(
        session.handle_event(ReplEvent::CtrlC, &source).unwrap(),
        ReplAction::Exit
    );
}

#[test]
fn slash_model_command_switches_provider_and_model_without_llm_submit() {
    let root = tempfile::tempdir().unwrap();
    let source = StaticClipboard::new(None);
    let mut session = ReplSession::new(AttachmentStore::new(root.path()));

    session
        .handle_event(
            ReplEvent::Text("/model claude claude-sonnet-4.5".to_string()),
            &source,
        )
        .unwrap();

    let action = session.handle_event(ReplEvent::Submit, &source).unwrap();
    let ReplAction::SwitchModel(selection) = action else {
        panic!("expected model switch action");
    };

    assert_eq!(selection.provider_name, "claude");
    assert_eq!(selection.model, "claude-sonnet-4.5");
    assert_eq!(session.input(), "");
}

#[test]
fn slash_model_single_argument_returns_shorthand_action() {
    let root = tempfile::tempdir().unwrap();
    let source = StaticClipboard::new(None);
    let mut session = ReplSession::new(AttachmentStore::new(root.path()));

    session
        .handle_event(ReplEvent::Text("/model claude".to_string()), &source)
        .unwrap();

    let action = session.handle_event(ReplEvent::Submit, &source).unwrap();
    let ReplAction::SwitchModelShorthand(arg) = action else {
        panic!("expected shorthand switch action, got {action:?}");
    };

    assert_eq!(arg, "claude");
    assert_eq!(session.input(), "");
}

#[test]
fn slash_model_without_arguments_lists_models() {
    let root = tempfile::tempdir().unwrap();
    let source = StaticClipboard::new(None);
    let mut session = ReplSession::new(AttachmentStore::new(root.path()));

    session
        .handle_event(ReplEvent::Text("/model".to_string()), &source)
        .unwrap();

    let action = session.handle_event(ReplEvent::Submit, &source).unwrap();
    assert_eq!(action, ReplAction::ShowModels);
}

#[test]
fn model_selection_resolver_accepts_new_models_for_known_providers() {
    let catalog = vec![
        ProviderConfig::new("local", "http://localhost:11434/v1", "local-key")
            .with_model("qwen3-coder"),
        ProviderConfig::new("claude", "https://api.anthropic.com/v1", "sk-anthropic")
            .with_model("claude-sonnet-4.5"),
    ];

    let resolved = resolve_model_selection(
        &catalog,
        &ReplModelSelection {
            provider_name: "claude".to_string(),
            model: "claude-sonnet-4.5".to_string(),
        },
    )
    .unwrap();
    assert_eq!(resolved.name(), "claude");

    // A model the config has never seen is accepted: the user must be able
    // to type a brand-new model name and have it used as-is.
    let resolved = resolve_model_selection(
        &catalog,
        &ReplModelSelection {
            provider_name: "claude".to_string(),
            model: "claude-opus-5".to_string(),
        },
    )
    .unwrap();
    assert_eq!(resolved.name(), "claude");

    let err = resolve_model_selection(
        &catalog,
        &ReplModelSelection {
            provider_name: "nope".to_string(),
            model: "any".to_string(),
        },
    )
    .unwrap_err();
    assert!(err.contains("unknown provider: nope"));
}

#[test]
fn resolve_model_shorthand_matches_provider_model_or_falls_back_to_active() {
    let catalog = vec![
        (
            "deepseek".to_string(),
            vec![
                "deepseek-v4-pro".to_string(),
                "deepseek-v4-flash".to_string(),
            ],
        ),
        ("glm".to_string(), vec!["glm-5.2".to_string()]),
    ];

    // Provider name → its first model.
    assert_eq!(
        harness_cli::providers::resolve_model_shorthand(&catalog, "deepseek", "glm").unwrap(),
        ("glm".to_string(), "glm-5.2".to_string())
    );
    // Unique model name → its owning provider.
    assert_eq!(
        harness_cli::providers::resolve_model_shorthand(&catalog, "glm", "deepseek-v4-flash")
            .unwrap(),
        ("deepseek".to_string(), "deepseek-v4-flash".to_string())
    );
    // Unknown name → a new model on the active provider.
    assert_eq!(
        harness_cli::providers::resolve_model_shorthand(&catalog, "glm", "glm-5.3").unwrap(),
        ("glm".to_string(), "glm-5.3".to_string())
    );
    // No active provider match at all → error.
    assert!(
        harness_cli::providers::resolve_model_shorthand(&catalog, "missing", "brand-new").is_err()
    );
}

#[test]
fn persist_model_addition_appends_once_and_reports_novelty() {
    use harness_cli::config::ConfigStore;
    use harness_cli::repl::persist_model_addition;

    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("providers.json");
    let store = ConfigStore::new(&path);
    store
        .save_provider(
            ProviderConfig::new("glm", "https://api.z.ai/api/paas/v4", "").with_model("glm-5.2"),
        )
        .unwrap();

    // First switch to a new model appends it to the saved provider.
    assert_eq!(
        persist_model_addition(&path, "glm", "glm-5.2-fast-preview"),
        Ok(true)
    );
    let config = store.load().unwrap();
    let models = config.provider("glm").unwrap().models().to_vec();
    assert_eq!(models, vec!["glm-5.2", "glm-5.2-fast-preview"]);

    // Repeating the switch is a no-op, not a duplicate entry.
    assert_eq!(
        persist_model_addition(&path, "glm", "glm-5.2-fast-preview"),
        Ok(false)
    );
    let config = store.load().unwrap();
    assert_eq!(config.provider("glm").unwrap().models().len(), 2);
}

#[test]
fn slash_history_command_searches_submitted_prompts_most_recent_first() {
    let root = tempfile::tempdir().unwrap();
    let source = StaticClipboard::new(None);
    let mut session = ReplSession::new(AttachmentStore::new(root.path()));

    for message in [
        "write cache notes",
        "summarize routing logs",
        "inspect cache metrics",
    ] {
        session
            .handle_event(ReplEvent::Text(message.to_string()), &source)
            .unwrap();
        assert!(matches!(
            session.handle_event(ReplEvent::Submit, &source).unwrap(),
            ReplAction::Submit(_)
        ));
    }

    session
        .handle_event(ReplEvent::Text("/history CACHE".to_string()), &source)
        .unwrap();

    let action = session.handle_event(ReplEvent::Submit, &source).unwrap();
    let ReplAction::HistorySearch(matches) = action else {
        panic!("expected history search action");
    };

    assert_eq!(
        matches
            .iter()
            .map(|history_match| history_match.message.as_str())
            .collect::<Vec<_>>(),
        vec!["inspect cache metrics", "write cache notes"]
    );
    assert_eq!(matches[0].index, 3);
    assert_eq!(matches[1].index, 1);
    assert_eq!(session.input(), "");
}

#[test]
fn slash_new_command_requests_a_fresh_session() {
    let root = tempfile::tempdir().unwrap();
    let source = StaticClipboard::new(None);
    let mut session = ReplSession::new(AttachmentStore::new(root.path()));

    session
        .handle_event(ReplEvent::Text("/new".to_string()), &source)
        .unwrap();

    let action = session.handle_event(ReplEvent::Submit, &source).unwrap();
    assert_eq!(action, ReplAction::NewSession);
    assert_eq!(session.input(), "");
}

#[test]
fn slash_history_command_reports_usage_error_for_missing_query() {
    let root = tempfile::tempdir().unwrap();
    let source = StaticClipboard::new(None);
    let mut session = ReplSession::new(AttachmentStore::new(root.path()));

    session
        .handle_event(ReplEvent::Text("/history".to_string()), &source)
        .unwrap();

    let action = session.handle_event(ReplEvent::Submit, &source).unwrap();
    let ReplAction::CommandError(error) = action else {
        panic!("expected command error action");
    };

    assert!(error.contains("/history QUERY"));
}

#[test]
fn repl_renders_agent_events_for_streaming_terminal_output() {
    let mut output = Vec::new();

    render_agent_event(
        &AgentEvent::ToolRoundStarted {
            round: 2,
            tool_calls: 3,
        },
        &mut output,
    )
    .unwrap();
    render_agent_event(
        &AgentEvent::ToolResult(ToolBatchResult {
            id: "call-1".to_string(),
            tool_name: "file.write".to_string(),
            ok: true,
            repaired: false,
            content: String::new(),
            metadata: json!({}),
            error: None,
            hint: None,
        }),
        &mut output,
    )
    .unwrap();
    render_agent_event(
        &AgentEvent::FinalContentDelta("streamed answer".to_string()),
        &mut output,
    )
    .unwrap();

    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("tool round 2: running 3 call(s)"));
    assert!(output.contains("tool call-1 file.write ok"));
    assert!(output.ends_with("streamed answer"));
}

// --- resize debounce: eager erase, lazy repaint ---
// Width changes reflow the terminal buffer and smear any painted panel,
// so the loop erases the viewport on every width event but repaints the
// full frame only once the size has been stable for the settle window.

#[test]
fn resize_debouncer_erases_on_width_change_and_repaints_after_settle() {
    use harness_cli::repl::{ResizeAction, ResizeDebouncer};
    use std::time::{Duration, Instant};

    let t0 = Instant::now();
    let mut deb = ResizeDebouncer::new(120, 30, Duration::from_millis(150));
    assert_eq!(deb.observe(100, 30, t0), ResizeAction::Erase);
    assert!(deb.is_pending());
    // Still settling: nothing to do yet.
    assert_eq!(
        deb.observe(100, 30, t0 + Duration::from_millis(50)),
        ResizeAction::None
    );
    // Stable past the settle window: one full repaint, then quiet.
    assert_eq!(
        deb.observe(100, 30, t0 + Duration::from_millis(200)),
        ResizeAction::Repaint
    );
    assert!(!deb.is_pending());
    assert_eq!(
        deb.observe(100, 30, t0 + Duration::from_millis(300)),
        ResizeAction::None
    );
}

#[test]
fn resize_debouncer_height_only_change_resizes_immediately() {
    use harness_cli::repl::{ResizeAction, ResizeDebouncer};
    use std::time::{Duration, Instant};

    let t0 = Instant::now();
    let mut deb = ResizeDebouncer::new(120, 30, Duration::from_millis(150));
    // No reflow on a height-only change: keep the cheap repin path and
    // never schedule a repaint.
    assert_eq!(deb.observe(120, 40, t0), ResizeAction::Resize);
    assert!(!deb.is_pending());
    assert_eq!(
        deb.observe(120, 40, t0 + Duration::from_millis(200)),
        ResizeAction::None
    );
}

#[test]
fn resize_debouncer_restarts_the_settle_window_while_dragging() {
    use harness_cli::repl::{ResizeAction, ResizeDebouncer};
    use std::time::{Duration, Instant};

    let t0 = Instant::now();
    let mut deb = ResizeDebouncer::new(120, 30, Duration::from_millis(150));
    assert_eq!(deb.observe(110, 30, t0), ResizeAction::Erase);
    // The drag continues: erase again, settle timer restarts.
    assert_eq!(
        deb.observe(90, 28, t0 + Duration::from_millis(100)),
        ResizeAction::Erase
    );
    // 100ms after the LAST change is still inside the window…
    assert_eq!(
        deb.observe(90, 28, t0 + Duration::from_millis(200)),
        ResizeAction::None
    );
    // …and 160ms after it the frame repaints once.
    assert_eq!(
        deb.observe(90, 28, t0 + Duration::from_millis(260)),
        ResizeAction::Repaint
    );
}
