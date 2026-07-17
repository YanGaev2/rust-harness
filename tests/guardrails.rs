use harness_cli::guardrails::{GuardrailController, GuardrailDecision, is_idempotent};
use serde_json::json;

#[test]
fn identical_failing_call_warns_then_blocks() {
    let mut g = GuardrailController::new();
    let args = json!({"path": "missing.txt"});
    // Первые две неудачи — allow; после 2-й записанной — warn; после 5-й — block.
    for i in 0..5 {
        let decision = g.check("file.read", &args);
        match i {
            0 | 1 => assert!(
                matches!(decision, GuardrailDecision::Allow),
                "i={i}: {decision:?}"
            ),
            _ => assert!(
                matches!(decision, GuardrailDecision::Warn(_)),
                "i={i}: {decision:?}"
            ),
        }
        g.record("file.read", &args, false, "no such file");
    }
    assert!(matches!(
        g.check("file.read", &args),
        GuardrailDecision::Block(_)
    ));
}

#[test]
fn argument_order_does_not_change_signature() {
    let mut g = GuardrailController::new();
    let a = json!({"path": "x.txt", "lines": 5});
    let b = json!({"lines": 5, "path": "x.txt"});
    for _ in 0..5 {
        g.record("file.read", &a, false, "boom");
    }
    // b — та же сигнатура: канонизация сортирует ключи.
    assert!(matches!(
        g.check("file.read", &b),
        GuardrailDecision::Block(_)
    ));
}

#[test]
fn same_tool_different_args_failures_block_later() {
    let mut g = GuardrailController::new();
    // 8 разных падающих вызовов одного тула -> Block по общему счётчику тула.
    for i in 0..8 {
        let args = json!({ "path": format!("f{i}.txt") });
        g.record("file.read", &args, false, "no such file");
    }
    let decision = g.check("file.read", &json!({"path": "f9.txt"}));
    assert!(matches!(decision, GuardrailDecision::Block(_)));
}

#[test]
fn idempotent_no_progress_blocks_identical_ok_reads() {
    let mut g = GuardrailController::new();
    let args = json!({"path": "same.txt"});
    for _ in 0..5 {
        g.record("file.read", &args, true, "same content");
    }
    assert!(matches!(
        g.check("file.read", &args),
        GuardrailDecision::Block(_)
    ));
}

#[test]
fn mutating_tool_repeats_are_allowed() {
    let mut g = GuardrailController::new();
    // file.replace дважды с теми же аргументами — легитимно (замена по одному
    // вхождению за вызов, подтверждено бенчем replace_across).
    let args = json!({"path": "a.py", "old_string": "old", "new_string": "new"});
    for _ in 0..6 {
        g.record("file.replace", &args, true, "replaced");
    }
    assert!(matches!(
        g.check("file.replace", &args),
        GuardrailDecision::Allow
    ));
}

#[test]
fn successful_read_resets_exact_failure_streak() {
    let mut g = GuardrailController::new();
    let args = json!({"path": "x.txt"});
    for _ in 0..4 {
        g.record("file.read", &args, false, "boom");
    }
    g.record("file.read", &args, true, "now exists");
    // Успех сбрасывает exact-failure серию: снова Allow.
    assert!(matches!(
        g.check("file.read", &args),
        GuardrailDecision::Allow
    ));
}

#[test]
fn changing_read_content_is_progress() {
    let mut g = GuardrailController::new();
    let args = json!({"path": "log.txt"});
    // Один и тот же идемпотентный вызов, но содержимое меняется (файл растёт):
    // прогресса нет только при ИДЕНТИЧНОМ результате.
    for i in 0..6 {
        g.record("file.read", &args, true, &format!("content v{i}"));
    }
    assert!(matches!(
        g.check("file.read", &args),
        GuardrailDecision::Allow
    ));
}

#[test]
fn idempotent_classification() {
    for tool in [
        "file.read",
        "file.list",
        "file.search",
        "file.stat",
        "file.hash",
        "file.tail",
        "attachment.read",
    ] {
        assert!(is_idempotent(tool), "{tool}");
    }
    for tool in [
        "file.write",
        "file.append",
        "file.replace",
        "file.delete",
        "file.move",
        "shell.exec",
    ] {
        assert!(!is_idempotent(tool), "{tool}");
    }
}
