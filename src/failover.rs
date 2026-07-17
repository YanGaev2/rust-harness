//! Классификация ошибок провайдера -> действие (retry / switch / fail).
//!
//! Таксономия по мотивам hermes-agent error_classifier: ключевое различие —
//! transient rate-limit (переждать на месте) vs quota (переключиться на
//! следующую модель цепочки) vs наши собственные ошибки (fail-fast:
//! переключение их только маскирует — auth, context overflow, битый парс).

use crate::chat_client::ChatClientError;

/// Дольше этого ждать на месте не имеет смысла — при наличии цепочки
/// дешевле переключиться.
const RETRY_IN_PLACE_MAX_SECONDS: u64 = 10;

const QUOTA_MARKERS: &[&str] = &[
    "quota",
    "insufficient_quota",
    "limit reached",
    "/day",
    "/hour",
    "exceeded",
];
const CONTEXT_MARKERS: &[&str] = &[
    "context length",
    "context_length",
    "maximum context",
    "too many tokens",
];
const POLICY_MARKERS: &[&str] = &["content_policy", "content policy", "moderation"];
const MODEL_MARKERS: &[&str] = &[
    "model not found",
    "model_not_found",
    "unknown model",
    "does not exist",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureReason {
    RateLimit,
    Quota,
    Billing,
    Auth,
    ContextOverflow,
    Overloaded,
    ServerError,
    Timeout,
    ContentPolicy,
    ModelNotFound,
    ResponseFormat,
    Transport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureAction {
    RetrySameProvider { after_seconds: u64 },
    SwitchProvider,
    Fail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassifiedError {
    pub reason: FailureReason,
    pub action: FailureAction,
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    let lower = haystack.to_ascii_lowercase();
    needles.iter().any(|needle| lower.contains(needle))
}

pub fn classify(err: &ChatClientError) -> ClassifiedError {
    match err {
        ChatClientError::Status {
            code,
            body,
            retry_after_seconds,
            ..
        } => classify_status(*code, body, *retry_after_seconds),
        // Сеть/хост недоступны: другой base_url цепочки может быть жив.
        ChatClientError::Http(_) | ChatClientError::Io(_) => ClassifiedError {
            reason: FailureReason::Transport,
            action: FailureAction::SwitchProvider,
        },
        // Битый JSON — наш парсер или сломанный провайдер; переключение
        // спрятало бы баг, который надо чинить.
        ChatClientError::Json(_) => ClassifiedError {
            reason: FailureReason::ResponseFormat,
            action: FailureAction::Fail,
        },
    }
}

fn classify_status(code: u16, body: &str, retry_after: Option<u64>) -> ClassifiedError {
    let (reason, action) = match code {
        429 if contains_any(body, QUOTA_MARKERS) => {
            (FailureReason::Quota, FailureAction::SwitchProvider)
        }
        429 => match retry_after {
            Some(seconds) if seconds <= RETRY_IN_PLACE_MAX_SECONDS => (
                FailureReason::RateLimit,
                FailureAction::RetrySameProvider {
                    after_seconds: seconds,
                },
            ),
            _ => (FailureReason::RateLimit, FailureAction::SwitchProvider),
        },
        402 => (FailureReason::Billing, FailureAction::SwitchProvider),
        401 | 403 => (FailureReason::Auth, FailureAction::Fail),
        400 if contains_any(body, CONTEXT_MARKERS) => {
            (FailureReason::ContextOverflow, FailureAction::Fail)
        }
        400 if contains_any(body, POLICY_MARKERS) => {
            (FailureReason::ContentPolicy, FailureAction::Fail)
        }
        404 if contains_any(body, MODEL_MARKERS) => {
            (FailureReason::ModelNotFound, FailureAction::SwitchProvider)
        }
        408 => (FailureReason::Timeout, FailureAction::SwitchProvider),
        503 | 529 => (FailureReason::Overloaded, FailureAction::SwitchProvider),
        500..=599 => (FailureReason::ServerError, FailureAction::SwitchProvider),
        _ => (FailureReason::Transport, FailureAction::Fail),
    };
    ClassifiedError { reason, action }
}
