use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use harness_cli::runtime::{RuntimeError, ToolCall, ToolCallResult, ToolExecutor, ToolScheduler};
use serde_json::json;

#[test]
fn scheduler_preserves_batch_order_and_reports_tool_errors_as_results() {
    let root = tempfile::tempdir().unwrap();
    let scheduler = ToolScheduler::new(harness_cli::runtime::ToolRuntime::new(root.path()))
        .with_max_concurrency(2);

    let results = scheduler.execute_batch(vec![
        ToolCall::new(
            "first",
            "write_file",
            json!({"file": "a.txt", "text": "alpha"}),
        ),
        ToolCall::new("bad", "missing_tool", json!({})),
        ToolCall::new(
            "third",
            "write_file",
            json!({"file": "b.txt", "text": "bravo"}),
        ),
    ]);

    assert_eq!(
        results
            .iter()
            .map(|result| result.id.as_str())
            .collect::<Vec<_>>(),
        vec!["first", "bad", "third"]
    );
    assert!(results[0].ok);
    // `write_file` is the advertised prior-aligned name — a clean call.
    assert!(!results[0].repaired);
    assert!(!results[1].ok);
    assert_eq!(results[1].tool_name, "missing_tool");
    assert!(
        results[1]
            .error
            .as_deref()
            .unwrap()
            .contains("unknown tool")
    );
    assert!(results[2].ok);
    assert_eq!(
        std::fs::read_to_string(root.path().join("a.txt")).unwrap(),
        "alpha"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("b.txt")).unwrap(),
        "bravo"
    );
}

#[test]
fn scheduler_attaches_repair_memo_for_repaired_calls() {
    let root = tempfile::tempdir().unwrap();
    let scheduler = ToolScheduler::new(harness_cli::runtime::ToolRuntime::new(root.path()));

    let results = scheduler.execute_batch(vec![
        ToolCall::new(
            "aliased",
            "file_write",
            json!({"file": "a.txt", "text": "alpha"}),
        ),
        ToolCall::new(
            "clean",
            "file.write",
            json!({"path": "b.txt", "content": "bravo"}),
        ),
    ]);

    // A repaired call must carry a memo telling the model the canonical form.
    assert!(results[0].repaired);
    let memo = results[0]
        .hint
        .as_deref()
        .expect("repaired call must include a memo hint");
    assert!(
        memo.contains("write_file"),
        "memo should name the advertised wire tool: {memo}"
    );
    assert!(
        memo.contains("file_write"),
        "memo should reference the requested name: {memo}"
    );

    // A clean call must not be flagged or carry a memo.
    assert!(!results[1].repaired);
    assert!(results[1].hint.is_none());
}

#[test]
fn scheduler_caps_parallel_tool_execution() {
    let state = Arc::new(Mutex::new(ConcurrencyState {
        running: 0,
        max_seen: 0,
    }));
    let scheduler = ToolScheduler::new(ObservedExecutor {
        state: Arc::clone(&state),
    })
    .with_max_concurrency(2);

    let calls = (0..6)
        .map(|index| ToolCall::new(format!("call-{index}"), "observed", json!({})))
        .collect();
    let results = scheduler.execute_batch(calls);

    assert_eq!(results.len(), 6);
    assert!(results.iter().all(|result| result.ok));
    let max_seen = state.lock().unwrap().max_seen;
    assert_eq!(max_seen, 2);
}

#[test]
fn scheduler_marks_unfinished_calls_when_batch_timeout_expires() {
    let scheduler = ToolScheduler::new(ObservedExecutor {
        state: Arc::new(Mutex::new(ConcurrencyState {
            running: 0,
            max_seen: 0,
        })),
    })
    .with_max_concurrency(1)
    .with_timeout(Duration::from_millis(20));

    let started = Instant::now();
    let results = scheduler.execute_batch(vec![
        ToolCall::new("first", "observed", json!({})),
        ToolCall::new("second", "observed", json!({})),
    ]);
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_millis(150),
        "scheduler should return near the batch timeout, elapsed: {elapsed:?}"
    );
    assert_eq!(
        results
            .iter()
            .map(|result| result.id.as_str())
            .collect::<Vec<_>>(),
        vec!["first", "second"]
    );
    assert!(results.iter().all(|result| !result.ok));
    assert!(results.iter().all(|result| {
        result
            .error
            .as_deref()
            .is_some_and(|error| error.contains("timed out"))
    }));
    assert!(results.iter().all(|result| {
        result.metadata["cancelled"] == true && result.metadata["reason"] == "batch_timeout"
    }));
}

#[test]
fn scheduler_serializes_calls_that_mutate_the_same_file() {
    let state = Arc::new(Mutex::new(ConcurrencyState {
        running: 0,
        max_seen: 0,
    }));
    let scheduler = ToolScheduler::new(ObservedExecutor {
        state: Arc::clone(&state),
    })
    .with_max_concurrency(4);

    // Three edits of one file in a single round — the multi_edit bench batch
    // that silently lost two of three changes to a read-modify-write race.
    // Alias spellings and "./" prefixes must not defeat the conflict check.
    let results = scheduler.execute_batch(vec![
        ToolCall::new(
            "first",
            "edit_file",
            json!({"file_path": "app.ini", "old_string": "port = 5432", "new_string": "port = 6543"}),
        ),
        ToolCall::new(
            "second",
            "edit_file",
            json!({"path": "./app.ini", "old_string": "ttl = 60", "new_string": "ttl = 300"}),
        ),
        ToolCall::new(
            "third",
            "edit_file",
            json!({"file": "APP.INI", "old_string": "rate_limit = 100", "new_string": "rate_limit = 250"}),
        ),
    ]);

    assert_eq!(
        results
            .iter()
            .map(|result| result.id.as_str())
            .collect::<Vec<_>>(),
        vec!["first", "second", "third"]
    );
    let max_seen = state.lock().unwrap().max_seen;
    assert_eq!(
        max_seen, 1,
        "same-file mutations must run sequentially, saw {max_seen} in flight"
    );
}

#[test]
fn scheduler_serializes_a_read_racing_a_write_of_the_same_file() {
    let state = Arc::new(Mutex::new(ConcurrencyState {
        running: 0,
        max_seen: 0,
    }));
    let scheduler = ToolScheduler::new(ObservedExecutor {
        state: Arc::clone(&state),
    })
    .with_max_concurrency(4);

    scheduler.execute_batch(vec![
        ToolCall::new("read", "read_file", json!({"file_path": "notes.md"})),
        ToolCall::new(
            "write",
            "write_file",
            json!({"file_path": "notes.md", "content": "new"}),
        ),
    ]);

    assert_eq!(state.lock().unwrap().max_seen, 1);
}

#[test]
fn scheduler_keeps_parallelism_for_disjoint_files_and_pure_reads() {
    let state = Arc::new(Mutex::new(ConcurrencyState {
        running: 0,
        max_seen: 0,
    }));
    let scheduler = ToolScheduler::new(ObservedExecutor {
        state: Arc::clone(&state),
    })
    .with_max_concurrency(4);

    // Mutations of different files plus two reads of one shared file: no
    // write/write or read/write overlap, so concurrency must survive.
    scheduler.execute_batch(vec![
        ToolCall::new(
            "a",
            "edit_file",
            json!({"file_path": "a.txt", "old_string": "x", "new_string": "y"}),
        ),
        ToolCall::new(
            "b",
            "edit_file",
            json!({"file_path": "b.txt", "old_string": "x", "new_string": "y"}),
        ),
        ToolCall::new("r1", "read_file", json!({"file_path": "shared.md"})),
        ToolCall::new("r2", "read_file", json!({"file_path": "shared.md"})),
    ]);

    let max_seen = state.lock().unwrap().max_seen;
    assert!(
        max_seen > 1,
        "disjoint calls should still run in parallel, saw {max_seen}"
    );
}

#[test]
fn concurrent_edits_of_one_file_all_apply() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("app.ini"),
        "port = 5432\nttl = 60\nrate_limit = 100\n",
    )
    .unwrap();
    let scheduler = ToolScheduler::new(harness_cli::runtime::ToolRuntime::new(root.path()))
        .with_max_concurrency(4);

    let results = scheduler.execute_batch(vec![
        ToolCall::new(
            "first",
            "edit_file",
            json!({"file_path": "app.ini", "old_string": "port = 5432", "new_string": "port = 6543"}),
        ),
        ToolCall::new(
            "second",
            "edit_file",
            json!({"file_path": "app.ini", "old_string": "ttl = 60", "new_string": "ttl = 300"}),
        ),
        ToolCall::new(
            "third",
            "edit_file",
            json!({"file_path": "app.ini", "old_string": "rate_limit = 100", "new_string": "rate_limit = 250"}),
        ),
    ]);

    assert!(results.iter().all(|result| result.ok));
    let content = std::fs::read_to_string(root.path().join("app.ini")).unwrap();
    assert_eq!(content, "port = 6543\nttl = 300\nrate_limit = 250\n");
}

#[derive(Debug, Clone)]
struct ObservedExecutor {
    state: Arc<Mutex<ConcurrencyState>>,
}

#[derive(Debug)]
struct ConcurrencyState {
    running: usize,
    max_seen: usize,
}

impl ToolExecutor for ObservedExecutor {
    fn execute(&self, call: ToolCall) -> Result<ToolCallResult, RuntimeError> {
        {
            let mut state = self.state.lock().unwrap();
            state.running += 1;
            state.max_seen = state.max_seen.max(state.running);
        }

        std::thread::sleep(Duration::from_millis(50));

        {
            let mut state = self.state.lock().unwrap();
            state.running -= 1;
        }

        Ok(ToolCallResult {
            id: call.id,
            tool_name: call.name,
            ok: true,
            repaired: false,
            content: "observed".to_string(),
            metadata: json!({}),
        })
    }
}
