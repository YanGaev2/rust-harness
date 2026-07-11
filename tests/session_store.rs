use std::fs;
use std::path::PathBuf;

use harness_cli::agent::{AgentRunResult, AgentTrace, AgentTraceEvent, AgentTraceToolCall};
use harness_cli::request::{ChatMessage, MessageToolCall};
use harness_cli::runtime::ToolBatchResult;
use harness_cli::session::{
    ChatSession, SessionStore, TraceWrapper, format_utc_timestamp, workspace_slug,
};
use serde_json::{Value, json};

fn store_in(dir: &tempfile::TempDir) -> SessionStore {
    SessionStore::with_root(dir.path().join("projects").join("test-workspace"))
}

fn canned_trace() -> AgentTrace {
    AgentTrace {
        provider: "deepseek".to_string(),
        model: "deepseek-chat".to_string(),
        workspace: "F:\\rust-harness".to_string(),
        user_message: "почини тест".to_string(),
        events: vec![
            AgentTraceEvent::Thinking {
                content: "смотрю на падающий тест".to_string(),
            },
            AgentTraceEvent::ModelToolCalls {
                round: 1,
                calls: vec![AgentTraceToolCall {
                    id: "call-1".to_string(),
                    name: "file_read".to_string(),
                    arguments: json!({"path": "src/main.rs"}),
                }],
            },
            AgentTraceEvent::ToolResult {
                round: 1,
                result: ToolBatchResult {
                    id: "call-1".to_string(),
                    tool_name: "file.read".to_string(),
                    ok: true,
                    repaired: true,
                    content: "fn main() {}".to_string(),
                    metadata: json!({}),
                    error: None,
                    hint: Some("use file_read".to_string()),
                },
            },
            AgentTraceEvent::FinalContent {
                content: Some("готово".to_string()),
            },
        ],
    }
}

/// The messages `append_run(canned_trace())` must persist, in provider order.
fn canned_trace_messages() -> Vec<ChatMessage> {
    vec![
        ChatMessage::assistant_tool_calls(vec![MessageToolCall::new(
            "call-1",
            "file_read",
            json!({"path": "src/main.rs"}),
        )]),
        ChatMessage::tool_result(
            "call-1",
            serde_json::to_string(&ToolBatchResult {
                id: "call-1".to_string(),
                tool_name: "file.read".to_string(),
                ok: true,
                repaired: true,
                content: "fn main() {}".to_string(),
                metadata: json!({}),
                error: None,
                hint: Some("use file_read".to_string()),
            })
            .unwrap(),
        ),
        ChatMessage::assistant("готово"),
    ]
}

#[test]
fn create_session_writes_meta_header_and_last_pointer() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir);

    let session = store
        .create_session(
            &PathBuf::from("F:\\rust-harness"),
            "deepseek",
            "deepseek-chat",
        )
        .unwrap();

    let last =
        fs::read_to_string(dir.path().join("projects/test-workspace/sessions/last")).unwrap();
    let session_file = dir
        .path()
        .join("projects/test-workspace/sessions")
        .join(last.trim());
    let content = fs::read_to_string(&session_file).unwrap();
    let meta: Value = serde_json::from_str(content.lines().next().unwrap()).unwrap();

    assert_eq!(meta["type"], "meta");
    assert_eq!(meta["session_id"], session.id());
    assert_eq!(meta["workspace"], "F:\\rust-harness");
    assert_eq!(meta["provider"], "deepseek");
    assert_eq!(meta["model"], "deepseek-chat");
    assert_eq!(meta["parent_session"], Value::Null);
    assert!(meta["created"].as_str().unwrap().ends_with('Z'));
    assert!(!session.id().is_empty());
}

#[test]
fn append_user_and_run_persist_messages_and_thinking_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir);
    let mut session = store
        .create_session(
            &PathBuf::from("F:\\rust-harness"),
            "deepseek",
            "deepseek-chat",
        )
        .unwrap();

    session.append_user("почини тест").unwrap();
    session.append_run(&canned_trace()).unwrap();

    let content = fs::read_to_string(session.path()).unwrap();
    let lines: Vec<Value> = content
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();

    let types: Vec<&str> = lines
        .iter()
        .map(|line| line["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        vec![
            "meta", "message", "thinking", "message", "message", "message"
        ]
    );
    assert_eq!(lines[1]["role"], "user");
    assert_eq!(lines[1]["content"], "почини тест");
    assert_eq!(lines[2]["content"], "смотрю на падающий тест");
    assert_eq!(lines[3]["role"], "assistant");
    assert_eq!(lines[3]["tool_calls"][0]["id"], "call-1");
    assert_eq!(lines[4]["role"], "tool");
    assert_eq!(lines[4]["tool_call_id"], "call-1");
    assert_eq!(lines[5]["role"], "assistant");
    assert_eq!(lines[5]["content"], "готово");
}

#[test]
fn replay_messages_filters_thinking_and_matches_original_messages() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir);
    let mut session = store
        .create_session(
            &PathBuf::from("F:\\rust-harness"),
            "deepseek",
            "deepseek-chat",
        )
        .unwrap();
    session.append_user("почини тест").unwrap();
    session.append_run(&canned_trace()).unwrap();

    let mut expected = vec![ChatMessage::user("почини тест")];
    expected.extend(canned_trace_messages());

    assert_eq!(session.replay_messages(), expected);
}

#[test]
fn resume_last_round_trips_the_full_dialog() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir);
    let mut session = store
        .create_session(
            &PathBuf::from("F:\\rust-harness"),
            "deepseek",
            "deepseek-chat",
        )
        .unwrap();
    session.append_user("почини тест").unwrap();
    session.append_run(&canned_trace()).unwrap();
    let written = session.replay_messages();
    let id = session.id().to_string();

    let resumed = store.resume_last().unwrap().expect("session should resume");

    assert_eq!(resumed.id(), id);
    assert_eq!(resumed.replay_messages(), written);
}

#[test]
fn resume_skips_corrupt_and_truncated_lines() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir);
    let mut session = store
        .create_session(
            &PathBuf::from("F:\\rust-harness"),
            "deepseek",
            "deepseek-chat",
        )
        .unwrap();
    session.append_user("до обрыва").unwrap();
    // Simulate a crash mid-write: garbage plus a truncated JSON line.
    let mut raw = fs::read_to_string(session.path()).unwrap();
    raw.push_str("not json at all\n");
    raw.push_str("{\"type\":\"message\",\"role\":\"user\",\"cont");
    fs::write(session.path(), raw).unwrap();

    let resumed = store.resume_last().unwrap().expect("session should resume");

    assert_eq!(
        resumed.replay_messages(),
        vec![ChatMessage::user("до обрыва")]
    );
    assert_eq!(resumed.skipped_lines(), 2);
}

#[test]
fn replay_trims_dangling_tool_calls_without_results() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir);
    let mut session = store
        .create_session(
            &PathBuf::from("F:\\rust-harness"),
            "deepseek",
            "deepseek-chat",
        )
        .unwrap();
    session.append_user("вопрос").unwrap();
    session.append_run(&canned_trace()).unwrap();
    session.append_user("ещё вопрос").unwrap();
    // Crash mid-run: the model asked for a tool, but no result was written.
    let mut trace = canned_trace();
    trace.events.truncate(2); // Thinking + ModelToolCalls, no ToolResult/Final
    session.append_run(&trace).unwrap();

    let resumed = store.resume_last().unwrap().expect("session should resume");
    let messages = resumed.replay_messages();

    // Providers reject assistant tool_calls without matching tool results, so
    // the dangling tail (and the user turn it answered) must be trimmed back
    // to the last complete exchange.
    let mut expected = vec![ChatMessage::user("вопрос")];
    expected.extend(canned_trace_messages());
    expected.push(ChatMessage::user("ещё вопрос"));
    assert_eq!(messages, expected);
}

#[test]
fn resume_last_returns_none_without_pointer() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir);

    assert!(store.resume_last().unwrap().is_none());
}

#[test]
fn write_trace_names_file_from_timestamp_provider_and_turn() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir);

    let wrapper = TraceWrapper {
        ts: "2026-07-11T09-15-42Z".to_string(),
        session_id: "a1b2c3".to_string(),
        turn: 3,
        trace: canned_trace(),
    };
    let path = store.write_trace(&wrapper).unwrap();

    assert_eq!(
        path.file_name().unwrap().to_str().unwrap(),
        "2026-07-11T09-15-42Z_deepseek_r3.json"
    );
    let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(written["session_id"], "a1b2c3");
    assert_eq!(written["turn"], 3);
    assert_eq!(written["trace"]["provider"], "deepseek");
    assert_eq!(written["trace"]["events"][1]["type"], "model_tool_calls");
}

fn canned_run_result() -> AgentRunResult {
    let trace = canned_trace();
    let mut messages = vec![ChatMessage::user("почини тест")];
    messages.extend(canned_trace_messages());
    AgentRunResult {
        final_content: Some("готово".to_string()),
        tool_results: Vec::new(),
        tool_rounds: 1,
        trace,
        messages,
    }
}

#[test]
fn chat_session_persists_turns_and_resumes_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = PathBuf::from("F:\\rust-harness");

    let mut chat = ChatSession::start(
        Some(store_in(&dir)),
        &workspace,
        "deepseek",
        "deepseek-chat",
    );
    assert!(chat.history().is_empty());

    chat.begin_turn("почини тест");
    chat.complete_turn(&canned_run_result());
    assert_eq!(chat.history(), canned_run_result().messages);

    // One trace file per run, linked to the session.
    let traces: Vec<_> = fs::read_dir(dir.path().join("projects/test-workspace/traces"))
        .unwrap()
        .collect();
    assert_eq!(traces.len(), 1);

    // "Restart": a fresh ChatSession over the same store resumes the dialog.
    let mut resumed = ChatSession::start(
        Some(store_in(&dir)),
        &workspace,
        "deepseek",
        "deepseek-chat",
    );
    assert_eq!(resumed.history(), canned_run_result().messages);
    let notices = resumed.take_notices().join("\n");
    assert!(
        notices.contains("resumed session"),
        "expected resume notice, got: {notices}"
    );

    // The turn counter continues from the resumed history (1 user turn so
    // far), so the next run's trace is r2, not a second r1.
    resumed.begin_turn("ещё вопрос");
    resumed.complete_turn(&canned_run_result());
    let trace_names: Vec<String> = fs::read_dir(dir.path().join("projects/test-workspace/traces"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        trace_names.iter().any(|name| name.ends_with("_r2.json")),
        "expected a turn-2 trace after resume, got: {trace_names:?}"
    );
}

#[test]
fn chat_session_start_new_switches_to_a_fresh_session() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = PathBuf::from("F:\\rust-harness");
    let mut chat = ChatSession::start(
        Some(store_in(&dir)),
        &workspace,
        "deepseek",
        "deepseek-chat",
    );
    chat.begin_turn("почини тест");
    chat.complete_turn(&canned_run_result());

    chat.start_new("deepseek", "deepseek-chat");

    assert!(chat.history().is_empty());
    // The next launch resumes the new (empty) session, not the old one.
    let resumed = ChatSession::start(
        Some(store_in(&dir)),
        &workspace,
        "deepseek",
        "deepseek-chat",
    );
    assert!(resumed.history().is_empty());
}

#[test]
fn chat_session_without_store_works_in_memory() {
    let mut chat = ChatSession::start(None, &PathBuf::from("F:\\rust-harness"), "deepseek", "chat");

    chat.begin_turn("почини тест");
    chat.complete_turn(&canned_run_result());

    assert_eq!(chat.history(), canned_run_result().messages);
}

#[test]
fn format_utc_timestamp_handles_leap_years() {
    assert_eq!(format_utc_timestamp(0), "1970-01-01T00-00-00Z");
    assert_eq!(format_utc_timestamp(951_782_400), "2000-02-29T00-00-00Z");
    assert_eq!(
        format_utc_timestamp(1_752_192_000 + 3661),
        "2025-07-11T01-01-01Z"
    );
}

#[test]
fn workspace_slug_sanitizes_path_characters() {
    assert_eq!(
        workspace_slug(&PathBuf::from("F:\\rust-harness")),
        "F--rust-harness"
    );
}
