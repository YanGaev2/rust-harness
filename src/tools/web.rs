//! Web tools: public-web search (DuckDuckGo no-JS endpoint) and bounded
//! page fetch with HTML-to-text extraction. Network-facing counterpart of
//! the workspace-bounded file tools; requests are bounded by timeout,
//! download cap, and returned-text cap.
//!
//! The search side is a scraper of DuckDuckGo's HTML results page — the
//! same endpoint the `ddgs` library and hermes-agent's keyless backend use.
//! Markup drift is expected once every year or two; parse failures must
//! surface as honest errors the model can read, never as silent successes.

use std::error::Error;
use std::fmt;
use std::io::Read;
use std::time::Duration;

use crate::http::http_agent;

/// The DuckDuckGo no-JS results page. The only mainstream engine whose
/// keyless HTML endpoint remains parseable (probe 2026-08-01: qwen3.6-35b
/// suggests this exact endpoint unprompted).
pub const DEFAULT_SEARCH_BASE: &str = "https://html.duckduckgo.com/html/";

pub const DEFAULT_SEARCH_COUNT: usize = 8;
pub const MAX_SEARCH_COUNT: usize = 20;
pub const DEFAULT_FETCH_MAX_CHARS: usize = 20_000;

/// Hard cap on downloaded body bytes; reading stops here mid-stream, so a
/// runaway page cannot flood memory or the agent loop.
const DOWNLOAD_MAX_BYTES: usize = 2 * 1024 * 1024;

/// The no-JS results page is served to browsers; default library user agents
/// are commonly refused, so identify as one.
const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:130.0) Gecko/20100101 Firefox/130.0";

#[derive(Debug, Clone)]
pub struct WebTool {
    timeout: Duration,
    proxy: Option<String>,
    search_base: String,
}

impl Default for WebTool {
    fn default() -> Self {
        Self::new()
    }
}

impl WebTool {
    pub fn new() -> Self {
        Self {
            timeout: Duration::from_secs(20),
            proxy: None,
            search_base: DEFAULT_SEARCH_BASE.to_string(),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Same proxy vocabulary as the chat client: `None`/`"none"` direct,
    /// `"env"` opts into HTTP(S)_PROXY, anything else is a proxy URL.
    pub fn with_proxy(mut self, proxy: Option<String>) -> Self {
        self.proxy = proxy;
        self
    }

    /// Override the search endpoint (tests point this at a mock server).
    pub fn with_search_base(mut self, base: impl Into<String>) -> Self {
        self.search_base = base.into();
        self
    }

    pub fn search(&self, query: &str, count: usize) -> Result<Vec<SearchResult>, WebError> {
        let query = query.trim();
        if query.is_empty() {
            return Err(WebError::Parse("empty search query".to_string()));
        }
        let count = count.clamp(1, MAX_SEARCH_COUNT);
        let agent = http_agent(self.timeout, self.proxy.as_deref());
        let body = format!("q={}", form_urlencode(query));
        let response = agent
            .post(&self.search_base)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("User-Agent", USER_AGENT)
            .send(&body)
            .map_err(|error| WebError::Transport(error.to_string()))?;
        let code = response.status().as_u16();
        let (bytes, _) = read_body_bounded(response.into_body(), DOWNLOAD_MAX_BYTES)?;
        if !(200..300).contains(&code) {
            return Err(WebError::Status {
                code,
                url: self.search_base.clone(),
                body: error_snippet(&bytes),
            });
        }
        let html = String::from_utf8_lossy(&bytes);
        Ok(parse_ddg_results(&html, count))
    }

    pub fn fetch(&self, url: &str, max_chars: usize) -> Result<FetchResult, WebError> {
        let url = url.trim();
        let scheme_ok = {
            let lower = url.to_ascii_lowercase();
            lower.starts_with("http://") || lower.starts_with("https://")
        };
        if !scheme_ok {
            return Err(WebError::InvalidUrl(url.to_string()));
        }
        let agent = http_agent(self.timeout, self.proxy.as_deref());
        let response = agent
            .get(url)
            .header("User-Agent", USER_AGENT)
            .header(
                "Accept",
                "text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.8",
            )
            .call()
            .map_err(|error| WebError::Transport(error.to_string()))?;
        let code = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let (bytes, body_truncated) = read_body_bounded(response.into_body(), DOWNLOAD_MAX_BYTES)?;
        if !(200..300).contains(&code) {
            return Err(WebError::Status {
                code,
                url: url.to_string(),
                body: error_snippet(&bytes),
            });
        }
        let raw = String::from_utf8_lossy(&bytes).into_owned();
        // No content-type header defaults to HTML: better to over-strip an
        // unlabeled text file than hand the model a wall of raw markup.
        let is_html = content_type
            .as_deref()
            .map(|value| value.contains("html") || value.contains("xml"))
            .unwrap_or(true);
        let text = if is_html { html_to_text(&raw) } else { raw };
        let (text, text_truncated) = truncate_chars(text, max_chars.max(1));
        Ok(FetchResult {
            url: url.to_string(),
            content_type,
            text,
            text_truncated,
            bytes_downloaded: bytes.len(),
            body_truncated,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchResult {
    pub url: String,
    pub content_type: Option<String>,
    pub text: String,
    pub text_truncated: bool,
    pub bytes_downloaded: usize,
    pub body_truncated: bool,
}

#[derive(Debug)]
pub enum WebError {
    InvalidUrl(String),
    Transport(String),
    Status {
        code: u16,
        url: String,
        body: String,
    },
    Parse(String),
}

impl fmt::Display for WebError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl(url) => write!(f, "invalid url (only http/https): {url}"),
            Self::Transport(message) => write!(f, "web request failed: {message}"),
            Self::Status { code, url, body } => {
                write!(f, "web request failed: {url}: status {code}: {body}")
            }
            Self::Parse(message) => write!(f, "could not parse search results: {message}"),
        }
    }
}

impl Error for WebError {}

/// Read up to `max_bytes` of the body. Unlike the shell pipes (which must be
/// drained), reading simply stops at the cap — the connection is dropped.
fn read_body_bounded(body: ureq::Body, max_bytes: usize) -> Result<(Vec<u8>, bool), WebError> {
    let mut reader = body.into_reader();
    let mut captured: Vec<u8> = Vec::with_capacity(8192);
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| WebError::Transport(error.to_string()))?;
        if read == 0 {
            return Ok((captured, false));
        }
        let remaining = max_bytes.saturating_sub(captured.len());
        let kept = read.min(remaining);
        captured.extend_from_slice(&buffer[..kept]);
        if kept < read {
            return Ok((captured, true));
        }
    }
}

fn error_snippet(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    collapse_inline_whitespace(&html_to_text(&text))
        .chars()
        .take(200)
        .collect()
}

fn truncate_chars(text: String, max_chars: usize) -> (String, bool) {
    if text.chars().count() <= max_chars {
        return (text, false);
    }
    (text.chars().take(max_chars).collect(), true)
}

/// DuckDuckGo result anchors carry `class="result__a"`; the organic URL is
/// either direct or wrapped in a `//duckduckgo.com/l/?uddg=<encoded>`
/// redirect. The snippet is the following `result__snippet` element.
fn parse_ddg_results(html: &str, count: usize) -> Vec<SearchResult> {
    const ANCHOR_MARKER: &str = "class=\"result__a\"";
    let mut results = Vec::new();
    let mut cursor = 0;
    while results.len() < count {
        let Some(found) = html[cursor..].find(ANCHOR_MARKER) else {
            break;
        };
        let marker = cursor + found;
        let tag_start = html[..marker].rfind("<a").unwrap_or(marker);
        let Some(tag_end) = html[marker..].find('>').map(|i| marker + i) else {
            break;
        };
        let Some(close) = html[tag_end..].find("</a>").map(|i| tag_end + i) else {
            break;
        };
        let href = extract_attr(&html[tag_start..tag_end], "href").unwrap_or_default();
        let title = collapse_inline_whitespace(&html_to_text(&html[tag_end + 1..close]));
        let url = resolve_ddg_href(&href);

        let next_anchor = html[close..]
            .find(ANCHOR_MARKER)
            .map(|i| close + i)
            .unwrap_or(html.len());
        let snippet = html[close..next_anchor]
            .find("result__snippet")
            .and_then(|found| {
                let marker = close + found;
                let tag_end = html[marker..].find('>').map(|i| marker + i)?;
                let close = html[tag_end..].find("</a>").map(|i| tag_end + i)?;
                Some(collapse_inline_whitespace(&html_to_text(
                    &html[tag_end + 1..close],
                )))
            })
            .unwrap_or_default();

        if !url.is_empty() && !title.is_empty() {
            results.push(SearchResult {
                title,
                url,
                snippet,
            });
        }
        cursor = close;
    }
    results
}

fn extract_attr(tag: &str, name: &str) -> Option<String> {
    let pattern = format!("{name}=\"");
    let start = tag.find(&pattern)? + pattern.len();
    let end = tag[start..].find('"')? + start;
    Some(decode_entities(&tag[start..end]))
}

fn resolve_ddg_href(href: &str) -> String {
    let href = href.trim();
    if let Some(found) = href.find("uddg=") {
        let tail = &href[found + "uddg=".len()..];
        let end = tail.find('&').unwrap_or(tail.len());
        return percent_decode(&tail[..end]);
    }
    if let Some(rest) = href.strip_prefix("//") {
        return format!("https://{rest}");
    }
    href.to_string()
}

fn form_urlencode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char)
            }
            b' ' => out.push('+'),
            other => {
                out.push('%');
                out.push_str(&format!("{other:02X}"));
            }
        }
    }
    out
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .and_then(|pair| u8::from_str_radix(pair, 16).ok());
                match hex {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Readable text from an HTML document: script/style/noscript blocks dropped,
/// block-level tags become line breaks, inline tags vanish, entities decoded,
/// whitespace collapsed.
pub fn html_to_text(html: &str) -> String {
    const BLOCK_TAGS: &[&str] = &[
        "p",
        "div",
        "br",
        "li",
        "ul",
        "ol",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "tr",
        "table",
        "td",
        "th",
        "blockquote",
        "pre",
        "section",
        "article",
        "header",
        "footer",
        "nav",
        "form",
        "hr",
        "dt",
        "dd",
        "dl",
        "figure",
        "figcaption",
        "main",
        "aside",
        "title",
    ];

    let stripped = strip_container_blocks(html, &["script", "style", "noscript"]);
    let mut out = String::with_capacity(stripped.len() / 2);
    let mut rest = stripped.as_str();
    loop {
        match rest.find('<') {
            None => {
                out.push_str(rest);
                break;
            }
            Some(pos) => {
                out.push_str(&rest[..pos]);
                let after = &rest[pos + 1..];
                match after.find('>') {
                    // Unterminated tag: drop the malformed remainder.
                    None => break,
                    Some(end) => {
                        let name: String = after[..end]
                            .trim_start_matches('/')
                            .chars()
                            .take_while(|ch| ch.is_ascii_alphanumeric())
                            .collect::<String>()
                            .to_ascii_lowercase();
                        if BLOCK_TAGS.contains(&name.as_str()) {
                            out.push('\n');
                        }
                        rest = &after[end + 1..];
                    }
                }
            }
        }
    }

    collapse_text_whitespace(&decode_entities(&out))
}

/// Remove `<tag …>…</tag>` containers whose content must never surface
/// (scripts, styles). Case-insensitive; an unterminated container drops the
/// rest of the document rather than leaking code into the text.
fn strip_container_blocks(html: &str, tags: &[&str]) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut cursor = 0;
    loop {
        let mut next: Option<(usize, &str)> = None;
        for tag in tags {
            let pattern = format!("<{tag}");
            if let Some(found) = lower[cursor..].find(&pattern) {
                let position = cursor + found;
                if next.is_none_or(|(best, _)| position < best) {
                    next = Some((position, tag));
                }
            }
        }
        let Some((start, tag)) = next else {
            out.push_str(&html[cursor..]);
            break;
        };
        out.push_str(&html[cursor..start]);
        let closing = format!("</{tag}");
        cursor = match lower[start..].find(&closing) {
            Some(found) => {
                let close_start = start + found;
                lower[close_start..]
                    .find('>')
                    .map(|i| close_start + i + 1)
                    .unwrap_or(lower.len())
            }
            None => lower.len(),
        };
    }
    out
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 1..];
        match after.find(';').filter(|semi| *semi <= 32) {
            Some(semi) => match decode_entity(&after[..semi]) {
                Some(decoded) => {
                    out.push_str(&decoded);
                    rest = &after[semi + 1..];
                }
                None => {
                    out.push('&');
                    rest = after;
                }
            },
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn decode_entity(entity: &str) -> Option<String> {
    Some(match entity {
        "amp" => "&".to_string(),
        "lt" => "<".to_string(),
        "gt" => ">".to_string(),
        "quot" => "\"".to_string(),
        "apos" => "'".to_string(),
        "nbsp" => " ".to_string(),
        "mdash" => "\u{2014}".to_string(),
        "ndash" => "\u{2013}".to_string(),
        "hellip" => "\u{2026}".to_string(),
        "laquo" => "\u{AB}".to_string(),
        "raquo" => "\u{BB}".to_string(),
        "lsquo" => "\u{2018}".to_string(),
        "rsquo" => "\u{2019}".to_string(),
        "ldquo" => "\u{201C}".to_string(),
        "rdquo" => "\u{201D}".to_string(),
        "copy" => "\u{A9}".to_string(),
        _ => {
            let code = entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|dec| dec.parse().ok()))?;
            char::from_u32(code)?.to_string()
        }
    })
}

/// Collapse space/tab runs within lines and blank-line runs between them.
fn collapse_text_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_run = 0_usize;
    for line in text.lines() {
        let line = collapse_inline_whitespace(line);
        if line.is_empty() {
            blank_run += 1;
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
            if blank_run > 0 {
                out.push('\n');
            }
        }
        blank_run = 0;
        out.push_str(&line);
    }
    out
}

fn collapse_inline_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
