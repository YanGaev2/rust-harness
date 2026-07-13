use harness_cli::platform::{OsFamily, ShellProfile};
use harness_cli::runtime::{ToolCall, ToolRuntime};
use serde_json::json;
use std::time::{Duration, Instant};

#[test]
fn shell_tool_description_names_the_detected_dialect() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("powershell.exe"), "stub").unwrap();
    std::fs::write(dir.path().join("cmd.exe"), "stub").unwrap();
    let profile =
        ShellProfile::detect_in(OsFamily::Windows, &[dir.path().to_path_buf()], &|_path| {
            Some(5)
        })
        .unwrap();

    let specs = ToolRuntime::tool_specs_with_shell(Some(&profile));
    let shell_spec = specs
        .iter()
        .find(|spec| spec.name() == "run_shell_command")
        .expect("shell tool must be advertised");

    // Shell probe 2026-07-13: the model writes PowerShell 20/20 when it is
    // told the interpreter is PowerShell, but falls into cmd idioms when the
    // dialect is left unspecified. The description carries that one bit.
    assert!(shell_spec.description().contains("PowerShell 5.1"));
}

#[test]
fn tool_specs_declare_measured_parameter_schemas() {
    // Probes 2026-07-12/13: the model guesses argument names when the spec
    // is silent. The schemas advertise the exact names it already sends in
    // combat (file_path, pattern, old_string/new_string) and pin the
    // timeout unit to seconds — the one place its priors are split.
    let specs = ToolRuntime::tool_specs();
    let schema_of = |name: &str| {
        specs
            .iter()
            .find(|spec| spec.name() == name)
            .unwrap_or_else(|| panic!("missing {name}"))
            .parameters()
            .unwrap_or_else(|| panic!("{name} must declare a schema"))
            .clone()
    };

    let shell = schema_of("run_shell_command");
    assert_eq!(shell["type"], "object");
    assert!(
        shell["required"]
            .as_array()
            .unwrap()
            .contains(&json!("command"))
    );
    let timeout_desc = shell["properties"]["timeout"]["description"]
        .as_str()
        .unwrap();
    assert!(
        timeout_desc.to_lowercase().contains("seconds"),
        "the split prior needs the unit spelled out: {timeout_desc}"
    );

    let read = schema_of("read_file");
    assert!(read["properties"]["file_path"].is_object());
    assert!(
        read["required"]
            .as_array()
            .unwrap()
            .contains(&json!("file_path"))
    );

    let grep = schema_of("grep_search");
    assert!(grep["properties"]["pattern"].is_object());

    let edit = schema_of("edit_file");
    assert!(edit["properties"]["old_string"].is_object());
    assert!(edit["properties"]["new_string"].is_object());

    // Every advertised tool carries a schema — no silent gaps.
    for spec in &specs {
        assert!(
            spec.parameters().is_some(),
            "{} is advertised without a schema",
            spec.name()
        );
    }
}

#[test]
fn shell_timeout_is_clamped_to_the_bounded_maximum() {
    let root = tempfile::tempdir().unwrap();
    let runtime = ToolRuntime::new(root.path());

    // Timeout probe 2026-07-13: in build contexts the model sends
    // milliseconds (120000). Read as seconds that is 33 hours — the shell
    // must stay bounded, so oversized values clamp to one hour with a memo.
    let result = runtime
        .execute(ToolCall::new(
            "clamp",
            "run_shell_command",
            json!({"command": "echo ok", "timeout": 120000}),
        ))
        .unwrap();

    assert!(result.ok, "{result:?}");
    assert!(result.repaired, "clamping is a repair the model must see");
    let note = result.metadata["repair_note"].as_str().unwrap_or_default();
    assert!(note.contains("3600"), "note must teach the bound: {note}");
    assert!(note.to_lowercase().contains("seconds"));
}

#[test]
fn empty_search_result_says_no_matches_instead_of_silence() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.txt"), "alpha\nbeta\n").unwrap();
    let runtime = ToolRuntime::new(root.path());

    // Bench run5: the model got a bare empty string 8 times while probing
    // patterns and had to guess whether the search worked at all.
    let result = runtime
        .execute(ToolCall::new(
            "s-empty",
            "grep_search",
            json!({"pattern": "needle", "path": "."}),
        ))
        .unwrap();

    assert!(result.ok);
    assert!(
        result.content.contains("no matches"),
        "empty result must say so: {:?}",
        result.content
    );
    assert!(result.content.contains("needle"), "{:?}", result.content);
}

#[test]
fn search_returns_context_lines_around_matches() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("log.txt"),
        "one\ntwo\nneedle here\nfour\nfive\n",
    )
    .unwrap();
    let runtime = ToolRuntime::new(root.path());

    // Probe prior: the model expects grep tools to take context_lines.
    // run5 niah_decoy: without context it fell back to reading 18 whole
    // files (~30KB) just to see what surrounds each match.
    let result = runtime
        .execute(ToolCall::new(
            "s-ctx",
            "grep_search",
            json!({"pattern": "needle", "context_lines": 1}),
        ))
        .unwrap();

    assert!(result.ok, "{result:?}");
    assert!(result.content.contains("log.txt:3:needle here"));
    assert!(
        result.content.contains("log.txt-2-two"),
        "{}",
        result.content
    );
    assert!(
        result.content.contains("log.txt-4-four"),
        "{}",
        result.content
    );
    assert!(!result.content.contains("five"), "{}", result.content);
}

#[test]
fn shell_metadata_does_not_duplicate_captured_output() {
    let root = tempfile::tempdir().unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "sh-meta",
            "run_shell_command",
            json!({"command": "echo dedup-check"}),
        ))
        .unwrap();

    assert!(result.ok);
    assert!(result.content.contains("dedup-check"));
    // stdout/stderr already travel in `content`; copying them into the
    // metadata doubles the tokens of every shell result the model reads.
    assert!(
        result.metadata.get("stdout").is_none(),
        "{:?}",
        result.metadata
    );
    assert!(
        result.metadata.get("stderr").is_none(),
        "{:?}",
        result.metadata
    );
    assert_eq!(result.metadata["exit_code"], 0);
    assert_eq!(result.metadata["stdout_truncated"], false);
}

#[test]
fn shell_tool_is_not_advertised_when_no_shell_exists() {
    let specs = ToolRuntime::tool_specs_with_shell(None);

    assert!(
        specs.iter().all(|spec| spec.name() != "run_shell_command"),
        "a shell-less environment must not advertise run_shell_command"
    );
    assert!(
        specs.iter().any(|spec| spec.name() == "read_file"),
        "file tools stay available without a shell"
    );
}

#[test]
fn runtime_repairs_common_file_write_alias_and_argument_names() {
    let root = tempfile::tempdir().unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-1",
            "file_write",
            json!({"file": "notes\\todo.txt", "text": "ship runtime"}),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "file.write");
    // `file_write` is a legacy alias now (the advertised name is
    // `write_file`), so the name itself is the repair.
    assert!(result.repaired);
    assert_eq!(
        std::fs::read_to_string(root.path().join("notes").join("todo.txt")).unwrap(),
        "ship runtime"
    );
}

#[test]
fn runtime_strips_unnecessary_null_argument_and_runs_anyway() {
    let root = tempfile::tempdir().unwrap();
    let runtime = ToolRuntime::new(root.path());

    // The model passed `null` for an optional parameter it should have omitted.
    // The harness must still execute and flag the call as repaired.
    let result = runtime
        .execute(ToolCall::new(
            "call-null",
            "file.write",
            json!({"path": "kept.txt", "content": "stays", "mode": null}),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "file.write");
    assert!(result.repaired);
    assert_eq!(
        std::fs::read_to_string(root.path().join("kept.txt")).unwrap(),
        "stays"
    );
}

#[test]
fn runtime_does_not_flag_api_wire_tool_name_as_repaired() {
    let root = tempfile::tempdir().unwrap();
    let runtime = ToolRuntime::new(root.path());

    // `write_file` is exactly the prior-aligned wire name the harness
    // advertises for the canonical `file.write` tool. Calling it is
    // correct, not a mistake, so it must NOT be flagged as repaired and
    // must not generate a corrective memo.
    let result = runtime
        .execute(ToolCall::new(
            "wire",
            "write_file",
            json!({"path": "wire.txt", "content": "ok"}),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "file.write");
    assert!(
        !result.repaired,
        "the advertised API wire name must not count as a repair"
    );
}

#[test]
fn tools_are_advertised_under_model_prior_names() {
    // Probe of DeepSeek priors (probe_all_tools, 2026-07-12): the model
    // thinks in verb_noun names — advertise the names it already knows
    // instead of making it translate ours.
    let names: Vec<String> = ToolRuntime::tool_specs()
        .iter()
        .map(|spec| spec.name().to_string())
        .collect();
    for expected in [
        "read_file",
        "write_file",
        "append_file",
        "edit_file",
        "list_files",
        "grep_search",
        "tail_file",
        "checksum_file",
        "stat_file",
        "delete_file",
        "move_file",
        "get_image",
        "run_shell_command",
    ] {
        assert!(
            names.contains(&expected.to_string()),
            "missing {expected}: {names:?}"
        );
    }
}

#[test]
fn model_prior_tool_names_resolve_without_repair() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.txt"), "needle").unwrap();
    let runtime = ToolRuntime::new(root.path());

    // The advertised names round-trip clean.
    let result = runtime
        .execute(ToolCall::new(
            "c1",
            "grep_search",
            json!({"query": "needle", "path": "."}),
        ))
        .unwrap();
    assert_eq!(result.tool_name, "file.search");
    assert!(!result.repaired, "advertised name must not be a repair");

    let result = runtime
        .execute(ToolCall::new(
            "c2",
            "run_shell_command",
            json!({"command": "echo ok"}),
        ))
        .unwrap();
    assert_eq!(result.tool_name, "shell.exec");
    assert!(!result.repaired);

    // Extra vocabulary from the probe resolves as aliases (repaired,
    // with a memo teaching the advertised name).
    for (name, args, canonical) in [
        (
            "edit_and_apply",
            json!({"file_path": "a.txt", "old_string": "needle", "new_string": "thread"}),
            "file.replace",
        ),
        ("get_file_info", json!({"path": "a.txt"}), "file.stat"),
    ] {
        let result = runtime.execute(ToolCall::new("c3", name, args)).unwrap();
        assert_eq!(result.tool_name, canonical, "{name}");
        assert!(
            result.repaired,
            "{name} is an alias, not the advertised name"
        );
    }
}

#[test]
fn absolute_path_inside_workspace_is_accepted() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("README.md"), "hello").unwrap();
    let runtime = ToolRuntime::new(root.path());

    // The system prompt tells the model the workspace root; models
    // trained on absolute-path harnesses legitimately send
    // <root>/README.md. That path IS inside the workspace.
    let absolute = root.path().join("README.md").display().to_string();
    let result = runtime
        .execute(ToolCall::new(
            "abs-read",
            "read_file",
            json!({"path": absolute}),
        ))
        .unwrap();
    assert!(
        result.ok,
        "absolute in-workspace path must work: {result:?}"
    );
    assert_eq!(result.content, "hello");

    // Writing through an absolute in-workspace path works too.
    let absolute_new = root.path().join("sub/new.txt").display().to_string();
    let result = runtime
        .execute(ToolCall::new(
            "abs-write",
            "write_file",
            json!({"path": absolute_new, "content": "created"}),
        ))
        .unwrap();
    assert!(result.ok, "{result:?}");
    assert_eq!(
        std::fs::read_to_string(root.path().join("sub/new.txt")).unwrap(),
        "created"
    );
}

#[test]
fn absolute_path_outside_workspace_is_rejected_with_honest_message() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "no").unwrap();
    let runtime = ToolRuntime::new(root.path());

    let absolute = outside.path().join("secret.txt").display().to_string();
    let err = runtime
        .execute(ToolCall::new(
            "abs-out",
            "read_file",
            json!({"path": absolute}),
        ))
        .unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("outside the workspace"),
        "message must be honest: {message}"
    );
    assert!(
        message.contains("relative"),
        "message must teach the fix: {message}"
    );
}

#[test]
fn list_files_hides_dot_directories_and_honors_depth() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".git")).unwrap();
    std::fs::write(root.path().join(".git/config"), "x").unwrap();
    std::fs::create_dir_all(root.path().join("src/deep")).unwrap();
    std::fs::write(root.path().join("src/main.rs"), "fn main() {}").unwrap();
    std::fs::write(root.path().join("src/deep/inner.rs"), "x").unwrap();
    std::fs::write(root.path().join("README.md"), "x").unwrap();
    let runtime = ToolRuntime::new(root.path());

    // Dot directories are noise for the model unless it asks for them.
    let result = runtime
        .execute(ToolCall::new("ls-1", "list_files", json!({"path": "."})))
        .unwrap();
    assert!(!result.content.contains(".git"), "{}", result.content);
    assert!(result.content.contains("src/main.rs"));

    // `depth` is the model's own vocabulary (seen live): 1 = top level only.
    let result = runtime
        .execute(ToolCall::new(
            "ls-2",
            "list_files",
            json!({"path": ".", "depth": 1}),
        ))
        .unwrap();
    assert!(result.content.contains("README.md"));
    assert!(result.content.contains("src/"));
    assert!(
        !result.content.contains("src/main.rs"),
        "depth 1 must not descend: {}",
        result.content
    );

    // Hidden entries are reachable on request.
    let result = runtime
        .execute(ToolCall::new(
            "ls-3",
            "list_files",
            json!({"path": ".", "show_hidden": true}),
        ))
        .unwrap();
    assert!(result.content.contains(".git/config"), "{}", result.content);
}

#[test]
fn bare_timeout_argument_means_seconds_not_milliseconds() {
    let root = tempfile::tempdir().unwrap();
    let runtime = ToolRuntime::new(root.path());

    // Bench run3: the model sent {"timeout": 10} meaning ten SECONDS
    // (the subprocess convention) and we read ten milliseconds — four
    // failed rounds in a row. `timeout_ms` stays milliseconds.
    let sleep = if cfg!(windows) {
        "Start-Sleep -Milliseconds 300; Write-Output done"
    } else {
        "sleep 0.3; echo done"
    };
    let result = runtime
        .execute(ToolCall::new(
            "t-sec",
            "run_shell_command",
            json!({"command": sleep, "timeout": 5}),
        ))
        .unwrap();
    assert!(
        result.ok,
        "5 must be seconds, not ms: {:?}",
        result.metadata
    );
    assert!(result.content.contains("done"));
}

#[test]
fn edit_file_accepts_text_to_replace_argument() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("app.py"), "DEBUG = True\n").unwrap();
    let runtime = ToolRuntime::new(root.path());

    // Seen live in bench run3 — the model's own vocabulary for edit_file.
    let result = runtime
        .execute(ToolCall::new(
            "t-ttr",
            "edit_file",
            json!({
                "path": "app.py",
                "text_to_replace": "DEBUG = True",
                "new_text": "DEBUG = False"
            }),
        ))
        .unwrap();
    assert!(result.ok, "{:?}", result.metadata);
    assert_eq!(
        std::fs::read_to_string(root.path().join("app.py")).unwrap(),
        "DEBUG = False\n"
    );
}

#[test]
fn accepted_argument_aliases_are_not_flagged_as_repaired() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("data.txt"), "needle here").unwrap();
    let runtime = ToolRuntime::new(root.path());

    // `pattern` is the grep-style name models actually send; it is a
    // documented alias for file.search and must not trigger a memo.
    let result = runtime
        .execute(ToolCall::new(
            "call-search-pattern",
            "grep_search",
            json!({"path": ".", "pattern": "needle"}),
        ))
        .unwrap();
    assert!(result.ok);
    assert_eq!(result.tool_name, "file.search");
    assert!(!result.repaired, "valid alias must be first-class");

    // Same for file/text on a canonical write call.
    let result = runtime
        .execute(ToolCall::new(
            "call-write-alias",
            "write_file",
            json!({"file": "out.txt", "text": "ok"}),
        ))
        .unwrap();
    assert!(result.ok);
    assert!(!result.repaired, "valid alias must be first-class");
}

#[test]
fn runtime_repairs_codex_style_file_write_arguments() {
    let root = tempfile::tempdir().unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-codex-write",
            "write_file",
            json!({
                "file_path": "notes/todo.txt",
                "contents": "ship codex-style write"
            }),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "file.write");
    // Advertised name + accepted codex-style aliases — a clean call.
    assert!(!result.repaired);
    assert_eq!(
        std::fs::read_to_string(root.path().join("notes").join("todo.txt")).unwrap(),
        "ship codex-style write"
    );
}

#[test]
fn runtime_can_append_file_with_alias_and_argument_repair() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("notes")).unwrap();
    std::fs::write(root.path().join("notes").join("todo.txt"), "first\n").unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-append",
            "file_append",
            json!({"file": "notes/todo.txt", "text": "second\n"}),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "file.append");
    assert!(result.repaired);
    assert_eq!(result.metadata["created"], false);
    assert_eq!(result.metadata["previous_len"], 6);
    assert_eq!(
        std::fs::read_to_string(root.path().join("notes").join("todo.txt")).unwrap(),
        "first\nsecond\n"
    );
}

#[test]
fn runtime_repairs_codex_style_file_append_arguments() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("notes")).unwrap();
    std::fs::write(root.path().join("notes").join("todo.txt"), "first").unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-codex-append",
            "file.append",
            json!({
                "file_path": "notes/todo.txt",
                "contents": "\nsecond"
            }),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "file.append");
    // Accepted argument aliases are first-class, not repairs: the bench
    // showed corrective memos on valid aliases only add noise the model
    // never learns from (canonical name repairs still get memos).
    assert!(!result.repaired);
    assert_eq!(
        std::fs::read_to_string(root.path().join("notes").join("todo.txt")).unwrap(),
        "first\nsecond"
    );
}

#[test]
fn runtime_repairs_raw_string_file_write_arguments() {
    let root = tempfile::tempdir().unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-raw-write",
            "write_file",
            json!({"_raw_arguments": "file: notes/raw.txt\ntext: repaired from raw"}),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "file.write");
    assert!(result.repaired);
    assert_eq!(
        std::fs::read_to_string(root.path().join("notes").join("raw.txt")).unwrap(),
        "repaired from raw"
    );
}

#[test]
fn runtime_can_read_file_after_repaired_write() {
    let root = tempfile::tempdir().unwrap();
    let runtime = ToolRuntime::new(root.path());

    runtime
        .execute(ToolCall::new(
            "call-1",
            "write_file",
            json!({"filename": "notes/todo.txt", "content": "read me"}),
        ))
        .unwrap();

    let result = runtime
        .execute(ToolCall::new(
            "call-2",
            "read_file",
            json!({"file": "notes/todo.txt"}),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "file.read");
    assert_eq!(result.content, "read me");
}

#[test]
fn runtime_repairs_codex_style_file_read_arguments() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("notes")).unwrap();
    std::fs::write(
        root.path().join("notes").join("todo.txt"),
        "read codex path",
    )
    .unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-codex-read",
            "read_file",
            json!({"file_path": "notes/todo.txt"}),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "file.read");
    // Advertised name + accepted argument alias — a clean call.
    assert!(!result.repaired);
    assert_eq!(result.content, "read codex path");
    assert_eq!(result.metadata["path"], "notes/todo.txt");
}

#[test]
fn runtime_treats_raw_string_shell_arguments_as_command() {
    let root = tempfile::tempdir().unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-raw-shell",
            "run_command",
            json!(native_echo_command()),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "shell.exec");
    assert!(result.repaired);
    assert_eq!(result.content.trim(), "harness-shell");
}

#[test]
fn runtime_file_read_accepts_max_bytes_and_reports_truncation() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("logs")).unwrap();
    std::fs::write(root.path().join("logs").join("big.txt"), "abcdefghij").unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-read-limit",
            "read_file",
            json!({"filename": "logs/big.txt", "max_bytes": 4}),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "file.read");
    assert!(!result.repaired);
    assert_eq!(result.content, "abcd");
    assert_eq!(result.metadata["path"], "logs/big.txt");
    assert_eq!(result.metadata["bytes_read"], 4);
    assert_eq!(result.metadata["total_bytes"], 10);
    assert_eq!(result.metadata["truncated"], true);
}

#[test]
fn runtime_can_tail_file_with_alias_and_argument_repair() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("logs")).unwrap();
    std::fs::write(
        root.path().join("logs").join("app.log"),
        "ignore\nline1\nline2\nline3\nline4\n",
    )
    .unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-tail",
            "file_tail",
            json!({"file": "logs/app.log", "lines": 2, "max_bytes": 1024}),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "file.tail");
    assert!(result.repaired);
    assert_eq!(result.content, "line3\nline4\n");
    assert_eq!(result.metadata["path"], "logs/app.log");
    assert_eq!(result.metadata["bytes_read"], 12);
    assert_eq!(result.metadata["truncated_prefix"], true);
    assert_eq!(result.metadata["max_lines"], 2);
}

#[test]
fn runtime_repairs_codex_style_file_path_for_tail_file() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("logs")).unwrap();
    std::fs::write(
        root.path().join("logs").join("app.log"),
        "one\ntwo\nthree\n",
    )
    .unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-codex-tail",
            "tail_file",
            json!({"file_path": "logs/app.log", "lines": 2}),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "file.tail");
    assert!(!result.repaired);
    assert_eq!(result.content, "two\nthree\n");
    assert_eq!(result.metadata["path"], "logs/app.log");
    assert_eq!(result.metadata["max_lines"], 2);
}

#[test]
fn runtime_coerces_string_numeric_limits_for_file_tail() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("logs")).unwrap();
    std::fs::write(
        root.path().join("logs").join("app.log"),
        "ignore\nline1\nline2\nline3\nline4\n",
    )
    .unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-tail-string-limits",
            "tail_file",
            json!({"file": "logs/app.log", "lines": "2", "max_bytes": "1024"}),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "file.tail");
    assert!(!result.repaired);
    assert_eq!(result.content, "line3\nline4\n");
    assert_eq!(result.metadata["max_bytes"], 1024);
    assert_eq!(result.metadata["max_lines"], 2);
}

#[test]
fn runtime_can_hash_file_with_alias_and_argument_repair() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("artifacts")).unwrap();
    std::fs::write(root.path().join("artifacts").join("data.bin"), b"hash me").unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-hash",
            "hash_file",
            json!({"file": "artifacts/data.bin"}),
        ))
        .unwrap();

    let expected = blake3::hash(b"hash me").to_hex().to_string();
    assert!(result.ok);
    assert_eq!(result.tool_name, "file.hash");
    assert!(result.repaired);
    assert_eq!(result.content, expected);
    assert_eq!(result.metadata["path"], "artifacts/data.bin");
    assert_eq!(result.metadata["bytes"], 7);
    assert_eq!(result.metadata["algorithm"], "blake3");
    assert_eq!(result.metadata["hash"], expected);
}

#[test]
fn runtime_repairs_codex_style_file_path_for_hash_file() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("artifacts")).unwrap();
    std::fs::write(root.path().join("artifacts").join("data.bin"), b"hash me").unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-codex-hash",
            "hash_file",
            json!({"file_path": "artifacts/data.bin"}),
        ))
        .unwrap();

    let expected = blake3::hash(b"hash me").to_hex().to_string();
    assert!(result.ok);
    assert_eq!(result.tool_name, "file.hash");
    assert!(result.repaired);
    assert_eq!(result.content, expected);
    assert_eq!(result.metadata["path"], "artifacts/data.bin");
}

#[test]
fn runtime_can_stat_file_with_alias_and_argument_repair() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("artifacts")).unwrap();
    std::fs::write(root.path().join("artifacts").join("data.bin"), b"metadata").unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-stat",
            "file_stat",
            json!({"file": "artifacts/data.bin"}),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "file.stat");
    assert!(result.repaired);
    assert_eq!(result.metadata["path"], "artifacts/data.bin");
    assert_eq!(result.metadata["is_file"], true);
    assert_eq!(result.metadata["is_dir"], false);
    assert_eq!(result.metadata["len"], 8);
    assert!(result.metadata["modified_unix_seconds"].as_u64().is_some());
}

#[test]
fn runtime_repairs_codex_style_file_path_for_stat_file() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("artifacts")).unwrap();
    std::fs::write(root.path().join("artifacts").join("data.bin"), b"metadata").unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-codex-stat",
            "stat_file",
            json!({"file_path": "artifacts/data.bin"}),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "file.stat");
    assert!(!result.repaired);
    assert_eq!(result.metadata["path"], "artifacts/data.bin");
    assert_eq!(result.metadata["is_file"], true);
    assert_eq!(result.metadata["len"], 8);
}

#[test]
fn runtime_can_read_clipboard_image_prompt_fragment_as_attachment() {
    let root = tempfile::tempdir().unwrap();
    let attachment_dir = root.path().join(".harness").join("attachments");
    std::fs::create_dir_all(&attachment_dir).unwrap();
    let image_path = attachment_dir.join("paste-image.png");
    let png = vec![137, 80, 78, 71, 13, 10, 26, 10, 1, 2, 3, 4];
    std::fs::write(&image_path, &png).unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-image",
            "read_attachment",
            json!({"image": format!("image file: {}", image_path.display())}),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "attachment.read");
    assert!(result.repaired);
    assert_eq!(result.content, "");
    assert_eq!(result.metadata["kind"], "image");
    assert_eq!(result.metadata["mime_type"], "image/png");
    assert_eq!(
        result.metadata["path"],
        ".harness/attachments/paste-image.png"
    );
    assert_eq!(result.metadata["bytes"], png.len());
}

#[test]
fn runtime_rejects_attachment_path_outside_allowed_roots() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(outside.path(), "secret").unwrap();
    let runtime = ToolRuntime::new(root.path());

    let err = runtime
        .execute(ToolCall::new(
            "call-outside",
            "attachment.read",
            json!({"path": outside.path().display().to_string()}),
        ))
        .unwrap_err();

    assert!(err.to_string().contains("outside allowed attachment roots"));
}

#[test]
fn runtime_can_list_files_with_alias_and_result_limit() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src").join("a.txt"), "alpha").unwrap();
    std::fs::write(root.path().join("src").join("b.txt"), "bravo").unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-list",
            "file_list",
            json!({"dir": "src", "max_results": 1}),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "file.list");
    assert!(result.repaired);
    assert!(result.content.contains("src/a.txt"));
    assert_eq!(result.metadata["truncated"], true);
}

#[test]
fn runtime_can_list_workspace_root_with_dot_path() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("seed-target.txt"), "seed marker").unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-list-root",
            "file_list",
            json!({"path": "."}),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "file.list");
    assert!(result.content.contains("seed-target.txt"));
}

#[test]
fn runtime_can_search_workspace_root_when_path_is_omitted() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("seed-target.txt"), "seed marker").unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-search-root",
            "file_search",
            json!({"pattern": "seed"}),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "file.search");
    assert!(result.content.contains("seed-target.txt:1:seed marker"));
}

#[test]
fn runtime_can_search_text_with_grep_alias() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src").join("a.txt"), "alpha\nneedle one\n").unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-search",
            "grep",
            json!({"path": "src", "pattern": "needle"}),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "file.search");
    assert!(result.repaired);
    assert!(result.content.contains("src/a.txt:2:needle one"));
    assert_eq!(result.metadata["matches"], 1);
}

#[test]
fn runtime_shell_exec_accepts_max_output_bytes_and_reports_truncation() {
    let root = tempfile::tempdir().unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-shell-limit",
            "run_command",
            json!({
                "cmd": native_large_stdout_command(70_000),
                "max_output_bytes": 1024
            }),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "shell.exec");
    assert!(result.repaired);
    assert_eq!(result.content.len(), 1024);
    assert!(result.content.chars().all(|ch| ch == 'x'));
    assert_eq!(result.metadata["stdout_truncated"], true);
    assert_eq!(result.metadata["stderr_truncated"], false);
    assert_eq!(result.metadata["max_output_bytes"], 1024);
}

#[test]
fn runtime_shell_exec_accepts_per_call_timeout_ms() {
    let root = tempfile::tempdir().unwrap();
    let runtime = ToolRuntime::new(root.path()).with_shell_timeout(Duration::from_secs(2));
    let started = Instant::now();

    let err = runtime
        .execute(ToolCall::new(
            "call-shell-timeout",
            "run_command",
            json!({
                "cmd": native_sleep_command(),
                "timeout_ms": 50
            }),
        ))
        .unwrap_err();

    assert!(err.to_string().contains("timed out"));
    assert!(
        started.elapsed() < Duration::from_millis(1000),
        "per-call timeout should override longer runtime timeout"
    );
}

#[test]
fn runtime_can_replace_text_with_alias_and_argument_repair() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src").join("notes.txt"), "alpha beta beta").unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-replace",
            "replace_file",
            json!({
                "file": "src/notes.txt",
                "find": "beta",
                "with": "done",
                "limit": 1
            }),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "file.replace");
    assert!(result.repaired);
    assert_eq!(result.metadata["path"], "src/notes.txt");
    assert_eq!(result.metadata["replacements"], 1);
    assert_eq!(
        std::fs::read_to_string(root.path().join("src").join("notes.txt")).unwrap(),
        "alpha done beta"
    );
}

#[test]
fn runtime_repairs_codex_style_file_replace_arguments() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src").join("notes.txt"), "alpha beta beta").unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-codex-replace",
            "replace_text",
            json!({
                "file_path": "src/notes.txt",
                "old_string": "beta",
                "new_string": "done"
            }),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "file.replace");
    assert!(result.repaired);
    assert_eq!(result.metadata["path"], "src/notes.txt");
    assert_eq!(result.metadata["replacements"], 1);
    assert_eq!(
        std::fs::read_to_string(root.path().join("src").join("notes.txt")).unwrap(),
        "alpha done beta"
    );
}

#[cfg(windows)]
fn native_large_stdout_command(bytes: usize) -> String {
    format!("[Console]::Out.Write(('x' * {bytes}))")
}

#[cfg(target_os = "linux")]
fn native_large_stdout_command(bytes: usize) -> String {
    format!("printf '%*s' {bytes} '' | tr ' ' x")
}

#[cfg(windows)]
fn native_echo_command() -> &'static str {
    "Write-Output harness-shell"
}

#[cfg(target_os = "linux")]
fn native_echo_command() -> &'static str {
    "printf harness-shell"
}

#[cfg(windows)]
fn native_sleep_command() -> &'static str {
    "Start-Sleep -Milliseconds 500"
}

#[cfg(target_os = "linux")]
fn native_sleep_command() -> &'static str {
    "sleep 1"
}

#[cfg(windows)]
#[test]
fn runtime_shell_exec_rewrites_double_ampersand_for_powershell() {
    let root = tempfile::tempdir().unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-and-chain",
            "shell.exec",
            json!({"command": "Write-Output one && Write-Output two"}),
        ))
        .unwrap();

    assert!(result.ok, "repaired chain must run: {:?}", result.metadata);
    assert!(result.repaired);
    assert!(result.content.contains("one"));
    assert!(result.content.contains("two"));
    let note = result.metadata["repair_note"].as_str().unwrap();
    assert!(note.contains("&&"), "the memo must explain the rewrite");
}

#[cfg(windows)]
#[test]
fn runtime_shell_exec_double_ampersand_keeps_stop_on_failure_semantics() {
    let root = tempfile::tempdir().unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-and-fail",
            "shell.exec",
            json!({"command": "cmd /c \"exit 5\" && Write-Output should-not-run"}),
        ))
        .unwrap();

    assert!(
        !result.content.contains("should-not-run"),
        "the second command must not run after the first fails"
    );
}

#[cfg(windows)]
#[test]
fn runtime_shell_exec_drops_cd_into_missing_directory() {
    let root = tempfile::tempdir().unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-cd-ghost",
            "shell.exec",
            json!({"command": "cd /mnt/harness-cli && Write-Output alive"}),
        ))
        .unwrap();

    assert!(
        result.ok,
        "the ghost cd must be dropped: {:?}",
        result.metadata
    );
    assert!(result.repaired);
    assert!(result.content.contains("alive"));
    let note = result.metadata["repair_note"].as_str().unwrap();
    assert!(note.contains("cd"), "the memo must mention the dropped cd");
}

#[test]
fn runtime_shell_exec_failure_surfaces_stderr_in_content() {
    let root = tempfile::tempdir().unwrap();
    let runtime = ToolRuntime::new(root.path());

    #[cfg(windows)]
    let command = "cmd /c \"echo boom 1>&2 & exit 3\"";
    #[cfg(target_os = "linux")]
    let command = "echo boom 1>&2; exit 3";

    let result = runtime
        .execute(ToolCall::new(
            "call-stderr",
            "shell.exec",
            json!({"command": command}),
        ))
        .unwrap();

    assert!(!result.ok);
    assert!(
        result.content.contains("boom"),
        "stderr must reach the model-visible content on failure, got: {:?}",
        result.content
    );
}

#[test]
fn runtime_can_delete_file_with_alias_and_argument_repair() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("notes")).unwrap();
    std::fs::write(root.path().join("notes").join("old.txt"), "old").unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-delete",
            "remove_file",
            json!({"file": "notes/old.txt"}),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "file.delete");
    assert!(result.repaired);
    assert_eq!(result.metadata["path"], "notes/old.txt");
    assert_eq!(result.metadata["was_dir"], false);
    assert!(!root.path().join("notes").join("old.txt").exists());
}

#[test]
fn runtime_repairs_codex_style_file_path_for_delete_file() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("notes")).unwrap();
    std::fs::write(root.path().join("notes").join("old.txt"), "old").unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-codex-delete",
            "remove_file",
            json!({"file_path": "notes/old.txt"}),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "file.delete");
    assert!(result.repaired);
    assert_eq!(result.metadata["path"], "notes/old.txt");
    assert!(!root.path().join("notes").join("old.txt").exists());
}

#[test]
fn runtime_can_move_file_with_alias_and_argument_repair() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src").join("draft.txt"), "draft").unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-move",
            "rename_file",
            json!({"from": "src/draft.txt", "to": "notes/final.txt"}),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "file.move");
    assert!(result.repaired);
    assert_eq!(result.metadata["source_path"], "src/draft.txt");
    assert_eq!(result.metadata["target_path"], "notes/final.txt");
    assert!(!root.path().join("src").join("draft.txt").exists());
    assert_eq!(
        std::fs::read_to_string(root.path().join("notes").join("final.txt")).unwrap(),
        "draft"
    );
}

#[test]
fn runtime_repairs_codex_style_file_move_arguments() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src").join("draft.txt"), "draft").unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-codex-move",
            "move_file",
            json!({
                "source_path": "src/draft.txt",
                "target_path": "notes/final.txt"
            }),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "file.move");
    assert!(!result.repaired);
    assert_eq!(result.metadata["source_path"], "src/draft.txt");
    assert_eq!(result.metadata["target_path"], "notes/final.txt");
    assert!(!root.path().join("src").join("draft.txt").exists());
    assert_eq!(
        std::fs::read_to_string(root.path().join("notes").join("final.txt")).unwrap(),
        "draft"
    );
}

#[test]
fn runtime_coerces_string_boolean_for_file_move_overwrite() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::create_dir_all(root.path().join("notes")).unwrap();
    std::fs::write(root.path().join("src").join("draft.txt"), "new").unwrap();
    std::fs::write(root.path().join("notes").join("final.txt"), "old").unwrap();
    let runtime = ToolRuntime::new(root.path());

    let result = runtime
        .execute(ToolCall::new(
            "call-move-overwrite-string",
            "rename_file",
            json!({
                "from": "src/draft.txt",
                "to": "notes/final.txt",
                "overwrite": "true"
            }),
        ))
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.tool_name, "file.move");
    assert!(result.repaired);
    assert_eq!(result.metadata["overwritten"], true);
    assert_eq!(
        std::fs::read_to_string(root.path().join("notes").join("final.txt")).unwrap(),
        "new"
    );
}
