//! Per-run loop-guardrails: детект повторяющихся бесплодных тул-вызовов.
//!
//! Чистое состояние без side-effects; решения — Allow/Warn/Block, где Block
//! означает «не исполнять, вернуть модели синтетический результат с советом».
//! Пороги и трёхуровневая схема (exact-failure / same-tool / no-progress)
//! позаимствованы из hermes-agent tool_guardrails. Блок не убивает сессию:
//! модель получает обычный tool-result с объяснением — это продолжение
//! «прощающей» философии рантайма.

use std::collections::HashMap;

use serde_json::Value;

const EXACT_FAILURE_WARN: u32 = 2;
const EXACT_FAILURE_BLOCK: u32 = 5;
const SAME_TOOL_FAILURE_BLOCK: u32 = 8;
const NO_PROGRESS_WARN: u32 = 2;
const NO_PROGRESS_BLOCK: u32 = 5;

/// Read-only тулы: одинаковый результат повторного вызова — «нет прогресса».
/// Мутирующие сюда не входят: два одинаковых file.replace легитимны (замена
/// одного вхождения за вызов).
const IDEMPOTENT_TOOLS: &[&str] = &[
    "file.read",
    "file.list",
    "file.search",
    "file.tail",
    "file.hash",
    "file.stat",
    "attachment.read",
];

pub fn is_idempotent(canonical_tool: &str) -> bool {
    IDEMPOTENT_TOOLS.contains(&canonical_tool)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardrailDecision {
    Allow,
    /// Исполнить, но добавить предупреждение в metadata результата.
    Warn(String),
    /// Не исполнять; вернуть синтетический результат с этим текстом.
    Block(String),
}

/// Канонизация аргументов: `serde_json::Map` сохраняет порядок вставки,
/// поэтому перед хэшированием ключи сортируются рекурсивно — иначе
/// `{"a":1,"b":2}` и `{"b":2,"a":1}` считались бы разными вызовами.
fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let inner: Vec<String> = keys
                .into_iter()
                .map(|key| format!("{key}:{}", canonical_json(&map[key])))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", inner.join(","))
        }
        other => other.to_string(),
    }
}

fn signature(tool: &str, arguments: &Value) -> String {
    blake3::hash(format!("{tool}\u{0}{}", canonical_json(arguments)).as_bytes())
        .to_hex()
        .to_string()
}

#[derive(Default)]
struct SignatureState {
    failure_streak: u32,
    last_ok_hash: Option<String>,
    no_progress_streak: u32,
}

#[derive(Default)]
pub struct GuardrailController {
    by_signature: HashMap<String, SignatureState>,
    tool_failures: HashMap<String, u32>,
}

impl GuardrailController {
    pub fn new() -> Self {
        Self::default()
    }

    /// Решение по вызову ДО исполнения. Не мутирует счётчики — исход
    /// сообщается через [`GuardrailController::record`].
    pub fn check(&mut self, tool: &str, arguments: &Value) -> GuardrailDecision {
        let sig = signature(tool, arguments);
        let state = self.by_signature.entry(sig).or_default();
        let tool_failures = self.tool_failures.get(tool).copied().unwrap_or(0);

        if state.failure_streak >= EXACT_FAILURE_BLOCK
            || tool_failures >= SAME_TOOL_FAILURE_BLOCK
            || state.no_progress_streak >= NO_PROGRESS_BLOCK
        {
            return GuardrailDecision::Block(block_guidance(tool));
        }
        if state.failure_streak >= EXACT_FAILURE_WARN
            || state.no_progress_streak >= NO_PROGRESS_WARN
        {
            return GuardrailDecision::Warn(warn_guidance(tool));
        }
        GuardrailDecision::Allow
    }

    /// Зафиксировать исход исполненного вызова.
    pub fn record(&mut self, tool: &str, arguments: &Value, ok: bool, content: &str) {
        let sig = signature(tool, arguments);
        let state = self.by_signature.entry(sig).or_default();
        if ok {
            state.failure_streak = 0;
            if is_idempotent(tool) {
                let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
                if state.last_ok_hash.as_deref() == Some(hash.as_str()) {
                    state.no_progress_streak += 1;
                } else {
                    // Новый результат = прогресс; серия начинается с единицы —
                    // «результат повторился N раз» включает первое появление.
                    state.no_progress_streak = 1;
                    state.last_ok_hash = Some(hash);
                }
            }
        } else {
            state.failure_streak += 1;
            *self.tool_failures.entry(tool.to_string()).or_default() += 1;
        }
    }
}

fn warn_guidance(tool: &str) -> String {
    format!(
        "guardrail: this exact {tool} call keeps returning the same outcome; \
         change the arguments or use a different tool before retrying"
    )
}

fn block_guidance(tool: &str) -> String {
    format!(
        "guardrail: blocked a repeated {tool} call — the identical call has \
         already failed or returned identical results several times. Do not \
         retry it verbatim. Diagnose first: list the directory (file.list), \
         check the path (file.stat), or take a different approach."
    )
}
