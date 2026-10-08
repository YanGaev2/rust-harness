use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::json;

use harness_cli::guardrails;
use harness_cli::platform::ShellProfile;
use harness_cli::runtime::{ToolCall, ToolRuntime};
use harness_cli::tools::web::{WebError, WebTool, html_to_text};

fn read_http_request(stream: &mut TcpStream) -> String {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let read = stream.read(&mut chunk).unwrap();
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        let text = String::from_utf8_lossy(&buffer);
        if let Some(header_end) = text.find("\r\n\r\n") {
            let content_length = text
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            if buffer.len() >= header_end + 4 + content_length {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buffer).to_string()
}

fn respond(stream: &mut TcpStream, status_line: &str, content_type: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).unwrap();
}

/// Serve exactly one request with the given response, returning the base URL
/// and a handle whose join yields the raw request the server saw. Accept is
/// polled with a deadline so a client that never connects (a tool bug) fails
/// the test instead of hanging it.
fn serve_once(status_line: &str, content_type: &str, body: String) -> (String, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let status_line = status_line.to_string();
    let content_type = content_type.to_string();
    let handle = thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "mock server saw no request within 10s"
                    );
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("mock server accept failed: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        let request = read_http_request(&mut stream);
        respond(&mut stream, &status_line, &content_type, &body);
        request
    });
    (format!("http://{addr}/"), handle)
}

fn ddg_results_page() -> String {
    concat!(
        "<html><body><div class=\"serp__results\">\n",
        "<div class=\"result results_links results_links_deep web-result\">\n",
        "<h2 class=\"result__title\">\n",
        "<a rel=\"nofollow\" class=\"result__a\" href=\"//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fureq%3Fx%3D1&amp;rut=abc123\">Ureq &amp; timeouts <b>guide</b></a>\n",
        "</h2>\n",
        "<a class=\"result__snippet\" href=\"//duckduckgo.com/l/?uddg=x\">Configure &quot;timeouts&quot; in <b>ureq</b> 3.</a>\n",
        "</div>\n",
        "<div class=\"result results_links results_links_deep web-result\">\n",
        "<h2 class=\"result__title\">\n",
        "<a rel=\"nofollow\" class=\"result__a\" href=\"https://direct.example.org/post\">Direct link result</a>\n",
        "</h2>\n",
        "<a class=\"result__snippet\">Second snippet.</a>\n",
        "</div>\n",
        "</div></body></html>"
    )
    .to_string()
}

#[test]
fn web_search_parses_duckduckgo_results() {
    let (base, server) = serve_once("200 OK", "text/html; charset=utf-8", ddg_results_page());

    let results = WebTool::new()
        .with_timeout(Duration::from_secs(5))
        .with_search_base(&base)
        .search("rust ureq", 8)
        .unwrap();

    let request = server.join().unwrap();
    assert!(
        request.starts_with("POST "),
        "search must POST to the endpoint, got: {}",
        request.lines().next().unwrap_or_default()
    );
    assert!(
        request.contains("q=rust+ureq"),
        "query must be form-urlencoded in the body: {request}"
    );

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].title, "Ureq & timeouts guide");
    assert_eq!(results[0].url, "https://example.com/ureq?x=1");
    assert_eq!(results[0].snippet, "Configure \"timeouts\" in ureq 3.");
    assert_eq!(results[1].title, "Direct link result");
    assert_eq!(results[1].url, "https://direct.example.org/post");
    assert_eq!(results[1].snippet, "Second snippet.");
}

#[test]
fn web_search_respects_count_cap() {
    let (base, server) = serve_once("200 OK", "text/html", ddg_results_page());

    let results = WebTool::new()
        .with_timeout(Duration::from_secs(5))
        .with_search_base(&base)
        .search("rust", 1)
        .unwrap();

    server.join().unwrap();
    assert_eq!(results.len(), 1);
}

#[test]
fn web_search_returns_empty_when_no_results() {
    let (base, server) = serve_once(
        "200 OK",
        "text/html",
        "<html><body><div class=\"no-results\">No results.</div></body></html>".to_string(),
    );

    let results = WebTool::new()
        .with_timeout(Duration::from_secs(5))
        .with_search_base(&base)
        .search("qwzyxq", 8)
        .unwrap();

    server.join().unwrap();
    assert!(results.is_empty());
}

#[test]
fn web_fetch_extracts_readable_text() {
    let page = concat!(
        "<html><head><title>Doc</title><style>body { color: red; }</style></head><body>",
        "<script>console.log(\"noise\");</script>",
        "<h1>Hello &amp; world</h1>",
        "<p>First paragraph.</p><p>Second &#39;paragraph&#39;.</p>",
        "</body></html>"
    );
    let (base, server) = serve_once("200 OK", "text/html; charset=utf-8", page.to_string());

    let result = WebTool::new()
        .with_timeout(Duration::from_secs(5))
        .fetch(&base, 20_000)
        .unwrap();

    server.join().unwrap();
    assert!(
        result.text.contains("Hello & world"),
        "text: {}",
        result.text
    );
    assert!(result.text.contains("Second 'paragraph'."));
    assert!(
        !result.text.contains("console.log"),
        "script must be stripped"
    );
    assert!(
        !result.text.contains("color: red"),
        "style must be stripped"
    );
    assert!(
        !result.text.contains('<'),
        "tags must be stripped: {}",
        result.text
    );
    assert!(result.text.find("First paragraph.").unwrap() < result.text.find("Second").unwrap());
    assert!(!result.text_truncated);
}

#[test]
fn web_fetch_truncates_to_max_chars() {
    let body = format!("<html><body><p>{}</p></body></html>", "word ".repeat(2000));
    let (base, server) = serve_once("200 OK", "text/html", body);

    let result = WebTool::new()
        .with_timeout(Duration::from_secs(5))
        .fetch(&base, 64)
        .unwrap();

    server.join().unwrap();
    assert!(result.text_truncated);
    assert!(
        result.text.chars().count() <= 64,
        "len={}",
        result.text.chars().count()
    );
}

#[test]
fn web_fetch_returns_plain_text_bodies_verbatim() {
    let (base, server) = serve_once(
        "200 OK",
        "text/plain",
        "just plain text, <no> html".to_string(),
    );

    let result = WebTool::new()
        .with_timeout(Duration::from_secs(5))
        .fetch(&base, 20_000)
        .unwrap();

    server.join().unwrap();
    assert_eq!(result.text, "just plain text, <no> html");
}

#[test]
fn web_fetch_rejects_non_http_schemes() {
    let tool = WebTool::new();
    for url in ["file:///etc/passwd", "ftp://example.com/x", "not a url"] {
        match tool.fetch(url, 1000) {
            Err(WebError::InvalidUrl(_)) => {}
            other => panic!("expected InvalidUrl for {url}, got {other:?}"),
        }
    }
}

#[test]
fn web_fetch_surfaces_http_status() {
    let (base, server) = serve_once("404 Not Found", "text/html", "<h1>gone</h1>".to_string());

    let error = WebTool::new()
        .with_timeout(Duration::from_secs(5))
        .fetch(&base, 1000)
        .unwrap_err();

    server.join().unwrap();
    assert!(error.to_string().contains("404"), "error: {error}");
}

#[test]
fn runtime_dispatches_web_search_alias_with_arg_repair() {
    let workspace = tempfile::tempdir().unwrap();
    let (base, server) = serve_once("200 OK", "text/html", ddg_results_page());

    let result = ToolRuntime::new(workspace.path())
        .with_web_search_base(&base)
        .execute(ToolCall::new(
            "call-1",
            "websearch",
            json!({"q": "rust ureq"}),
        ))
        .unwrap();

    server.join().unwrap();
    assert!(result.ok);
    assert_eq!(result.tool_name, "web.search");
    assert!(result.repaired, "alias name + alias arg must flag repaired");
    assert!(result.content.contains("Ureq & timeouts guide"));
    assert!(result.content.contains("https://example.com/ureq?x=1"));
    assert_eq!(result.metadata["results"], json!(2));
}

#[test]
fn runtime_dispatches_web_fetch_wire_name_without_repair() {
    let workspace = tempfile::tempdir().unwrap();
    let (base, server) = serve_once(
        "200 OK",
        "text/html",
        "<html><body><p>page body here</p></body></html>".to_string(),
    );

    let result = ToolRuntime::new(workspace.path())
        .execute(ToolCall::new("call-2", "web_fetch", json!({"url": base})))
        .unwrap();

    server.join().unwrap();
    assert!(result.ok);
    assert_eq!(result.tool_name, "web.fetch");
    assert!(!result.repaired);
    assert!(result.content.contains("page body here"));
}

#[test]
fn runtime_repairs_bare_string_arguments_for_web_tools() {
    let workspace = tempfile::tempdir().unwrap();
    let (base, server) = serve_once(
        "200 OK",
        "text/html",
        "<html><body><p>fetched</p></body></html>".to_string(),
    );

    let result = ToolRuntime::new(workspace.path())
        .execute(ToolCall::new("call-3", "web_fetch", json!(base.clone())))
        .unwrap();

    server.join().unwrap();
    assert!(
        result.ok,
        "bare-string url must be repaired into {{\"url\": …}}"
    );
    assert!(result.repaired);
    assert!(result.content.contains("fetched"));
}

#[test]
fn tool_specs_advertise_web_tools() {
    let specs = ToolRuntime::tool_specs_with_shell(Some(&ShellProfile::native()));
    let search = specs
        .iter()
        .find(|spec| spec.name() == "web_search")
        .expect("web_search spec");
    let fetch = specs
        .iter()
        .find(|spec| spec.name() == "web_fetch")
        .expect("web_fetch spec");

    let search_schema = search.parameters().expect("web_search schema");
    assert_eq!(search_schema["required"], json!(["query"]));
    let fetch_schema = fetch.parameters().expect("web_fetch schema");
    assert_eq!(fetch_schema["required"], json!(["url"]));
    // Probe 2026-08-01: DeepSeek confused web search with grep_search until
    // the descriptions drew the web/workspace line explicitly.
    assert!(search.description().contains("web"));
}

#[test]
fn web_tools_count_as_idempotent_for_loop_guardrails() {
    assert!(guardrails::is_idempotent("web.search"));
    assert!(guardrails::is_idempotent("web.fetch"));
}

#[test]
fn html_to_text_decodes_entities_and_numeric_references() {
    let text = html_to_text(
        "<p>A &amp; B &lt;tag&gt; &quot;q&quot; &#39;s &nbsp;end &#1055;&#x440;ивет</p>",
    );
    assert!(text.contains("A & B <tag> \"q\" 's"), "text: {text}");
    assert!(text.contains("Привет"), "numeric refs must decode: {text}");
}
