//! Shared blocking-HTTP agent builder, used by the chat client and the web
//! tools so both follow the same proxy policy.

use std::time::Duration;

/// Build the blocking HTTP agent. Non-2xx statuses come back as regular
/// responses (`http_status_as_error(false)`) so callers can surface the
/// server's own error body instead of a bare status code.
///
/// Proxying is strictly opt-in via the harness config: `None`/"none" go
/// direct (ambient HTTP_PROXY/HTTPS_PROXY are deliberately ignored so the
/// environment can never silently reroute traffic), `"env"` opts into the
/// environment variables, anything else is a proxy URL.
pub fn http_agent(timeout: Duration, proxy: Option<&str>) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .proxy(resolve_proxy(proxy))
        .build()
        .into()
}

/// Agent for SSE streaming: a live stream from a thinking model legitimately
/// runs for many minutes, so `timeout` must never cap the whole request
/// (observed with kimi-k3: `timeout: global` while reasoning tokens were
/// still flowing). Instead it bounds each phase separately:
///
/// - `connect` / `send_request`: connection setup, and — because a configured
///   phase timeout also constrains the immediately following phase from its
///   anchor instant — the wait for response headers after the request is sent;
/// - `recv_body`: re-anchored at "now" on every read, so it acts as an *idle*
///   cap — a flowing stream never trips it, a silently hung one dies.
///
/// `recv_response` stays unset on purpose: ureq keeps checking it during body
/// reads anchored at the header-arrival instant, which is exactly the
/// kill-a-live-stream behaviour this agent exists to avoid.
pub fn streaming_http_agent(timeout: Duration, proxy: Option<&str>) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(timeout))
        .timeout_send_request(Some(timeout))
        .timeout_recv_body(Some(stream_idle_timeout(timeout)))
        .http_status_as_error(false)
        .proxy(resolve_proxy(proxy))
        .build()
        .into()
}

/// Floor for the per-read idle cap of an SSE stream. Some providers buffer a
/// whole tool call server-side (baseten/Kimi K3: zero bytes on the socket —
/// not even SSE comments — while the model writes a large file), so minutes of
/// silence are legitimate mid-stream. The request timeout still bounds
/// connect/header phases; only body reads get the floor. Survey of other
/// harnesses, 2026-08-08: hermes stall-watchdog 180–300s, pi/undici 300s.
pub const STREAM_IDLE_FLOOR: Duration = Duration::from_secs(300);

/// Per-read idle cap for stream bodies: the configured timeout, but never
/// tighter than [`STREAM_IDLE_FLOOR`] (an explicitly larger timeout wins).
pub fn stream_idle_timeout(timeout: Duration) -> Duration {
    timeout.max(STREAM_IDLE_FLOOR)
}

fn resolve_proxy(proxy: Option<&str>) -> Option<ureq::Proxy> {
    match proxy.map(str::trim) {
        None | Some("") | Some("none") => None,
        Some("env") => ureq::Proxy::try_from_env(),
        Some(url) => ureq::Proxy::new(url).ok(),
    }
}
