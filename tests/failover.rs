use harness_cli::chat_client::ChatClientError;
use harness_cli::failover::{FailureAction, FailureReason, classify};

fn status(code: u16, body: &str, retry_after: Option<u64>) -> ChatClientError {
    ChatClientError::Status {
        code,
        url: "https://api.example/v1".into(),
        body: body.into(),
        retry_after_seconds: retry_after,
    }
}

#[test]
fn transient_429_with_short_retry_after_retries_in_place() {
    let c = classify(&status(429, r#"{"error":"slow down"}"#, Some(3)));
    assert_eq!(c.reason, FailureReason::RateLimit);
    assert_eq!(
        c.action,
        FailureAction::RetrySameProvider { after_seconds: 3 }
    );
}

#[test]
fn quota_exhausted_429_switches_provider() {
    for body in [
        r#"{"error":{"message":"You exceeded your current quota"}}"#,
        r#"{"error":"Rate limit reached for requests: limit 100/day"}"#,
        r#"{"message":"insufficient_quota"}"#,
    ] {
        let c = classify(&status(429, body, None));
        assert_eq!(c.reason, FailureReason::Quota, "body={body}");
        assert_eq!(c.action, FailureAction::SwitchProvider, "body={body}");
    }
}

#[test]
fn plain_429_without_retry_after_switches() {
    // Нет Retry-After и нет quota-маркеров: не знаем, сколько ждать —
    // при наличии цепочки дешевле переключиться.
    let c = classify(&status(429, "", None));
    assert_eq!(c.reason, FailureReason::RateLimit);
    assert_eq!(c.action, FailureAction::SwitchProvider);
}

#[test]
fn long_retry_after_switches_instead_of_sleeping() {
    let c = classify(&status(429, "", Some(1800)));
    assert_eq!(c.action, FailureAction::SwitchProvider);
}

#[test]
fn billing_402_switches() {
    let c = classify(&status(402, r#"{"error":"insufficient balance"}"#, None));
    assert_eq!(c.reason, FailureReason::Billing);
    assert_eq!(c.action, FailureAction::SwitchProvider);
}

#[test]
fn auth_401_fails_hard() {
    // Битый ключ маскировать переключением нельзя: пользователь должен видеть.
    let c = classify(&status(401, r#"{"error":"invalid api key"}"#, None));
    assert_eq!(c.reason, FailureReason::Auth);
    assert_eq!(c.action, FailureAction::Fail);
}

#[test]
fn context_overflow_fails_without_switch() {
    let c = classify(&status(
        400,
        r#"{"error":{"message":"This model's maximum context length is 128000 tokens"}}"#,
        None,
    ));
    assert_eq!(c.reason, FailureReason::ContextOverflow);
    assert_eq!(c.action, FailureAction::Fail);
}

#[test]
fn overloaded_503_switches() {
    let c = classify(&status(
        503,
        r#"{"error":{"type":"overloaded_error"}}"#,
        None,
    ));
    assert_eq!(c.reason, FailureReason::Overloaded);
    assert_eq!(c.action, FailureAction::SwitchProvider);
}

#[test]
fn server_500_switches() {
    let c = classify(&status(500, "internal", None));
    assert_eq!(c.reason, FailureReason::ServerError);
    assert_eq!(c.action, FailureAction::SwitchProvider);
}

#[test]
fn model_not_found_switches() {
    let c = classify(&status(404, r#"{"error":"model not found"}"#, None));
    assert_eq!(c.reason, FailureReason::ModelNotFound);
    assert_eq!(c.action, FailureAction::SwitchProvider);
}

#[test]
fn plain_404_fails() {
    // 404 без model-маркеров — скорее кривой base_url: конфиг-ошибка
    // пользователя, переключение её маскирует.
    let c = classify(&status(404, "<html>Not Found</html>", None));
    assert_eq!(c.action, FailureAction::Fail);
}

#[test]
fn content_policy_fails() {
    let c = classify(&status(
        400,
        r#"{"error":{"code":"content_policy_violation"}}"#,
        None,
    ));
    assert_eq!(c.reason, FailureReason::ContentPolicy);
    assert_eq!(c.action, FailureAction::Fail);
}

#[test]
fn malformed_json_body_is_response_format_fail() {
    let json_err = serde_json::from_str::<serde_json::Value>("{oops").unwrap_err();
    let c = classify(&ChatClientError::Json(json_err));
    assert_eq!(c.reason, FailureReason::ResponseFormat);
    // Наш баг парсинга или битый ответ — переключение это замаскирует.
    assert_eq!(c.action, FailureAction::Fail);
}

#[test]
fn timeout_408_switches() {
    let c = classify(&status(408, "", None));
    assert_eq!(c.reason, FailureReason::Timeout);
    assert_eq!(c.action, FailureAction::SwitchProvider);
}
