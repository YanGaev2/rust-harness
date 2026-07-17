//! Классификация ошибок провайдера -> действие (retry / switch / fail).
//!
//! Таксономия по мотивам hermes-agent error_classifier: ключевое различие —
//! transient rate-limit (переждать на месте) vs quota (переключиться на
//! следующую модель цепочки) vs наши собственные ошибки (fail-fast:
//! переключение их только маскирует — auth, context overflow, битый парс).

use std::error::Error;
use std::fmt;
use std::path::Path;

use serde::Deserialize;

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

/// Одна запись цепочки: провайдер из providers.json + имя модели.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FallbackEntry {
    pub provider: String,
    pub model: String,
}

/// Цепочка переключения из `fallback.json` (лежит рядом с providers.json).
/// Отсутствие файла — не ошибка (конвенция ConfigStore: missing = empty);
/// битый JSON — ошибка, молча терять конфиг пользователя нельзя.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FallbackChain {
    #[serde(rename = "chain")]
    pub entries: Vec<FallbackEntry>,
}

impl FallbackChain {
    pub fn load(path: &Path) -> Result<Option<Self>, FailoverError> {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(FailoverError::Io(err)),
        };
        let chain: Self = serde_json::from_str(&raw).map_err(FailoverError::Parse)?;
        Ok(Some(chain))
    }
}

#[derive(Debug)]
pub enum FailoverError {
    Io(std::io::Error),
    Parse(serde_json::Error),
}

impl fmt::Display for FailoverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "fallback config read failed: {err}"),
            Self::Parse(err) => write!(f, "fallback config is not valid JSON: {err}"),
        }
    }
}

impl Error for FailoverError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Parse(err) => Some(err),
        }
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
