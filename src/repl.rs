use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use harness_tui::core::Screen;
use harness_tui::input::{Event as TuiEvent, KeyCode as TuiKeyCode, Parser, coalesce_burst};
use harness_tui::terminal as tui_terminal;

use crate::agent::{AgentError, AgentEvent, AgentRunner};
use crate::chat::{BusyAction, ChatAction, ChatApp, busy_action};
use crate::clipboard::{
    AttachmentStore, ClipboardAttachment, ClipboardCapture, ClipboardError, ClipboardSource,
    SystemClipboard,
};
use crate::providers::ProviderConfig;
use crate::request::ChatMessage;
use crate::session::{ChatSession, SessionStore};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplEvent {
    Text(String),
    /// A bracketed-paste block delivered atomically by the terminal. Newlines are
    /// preserved and the paste never counts as a submit.
    Paste(String),
    Backspace,
    CtrlV,
    CtrlC,
    Submit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplAction {
    Continue,
    Submit(ReplSubmission),
    SwitchModel(ReplModelSelection),
    HistorySearch(Vec<ReplHistoryMatch>),
    CommandError(String),
    /// `/new`: abandon the resumed conversation and start a fresh session file.
    NewSession,
    Exit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplHistoryMatch {
    pub index: usize,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplModelSelection {
    pub provider_name: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplSubmission {
    pub message: String,
    pub attachments: Vec<ClipboardAttachment>,
}

#[derive(Debug, Clone)]
pub struct ReplSession {
    input: String,
    attachments: Vec<ClipboardAttachment>,
    history: VecDeque<String>,
    capture: ClipboardCapture,
}

const MAX_REPL_HISTORY: usize = 200;

impl ReplSession {
    pub fn new(store: AttachmentStore) -> Self {
        Self {
            input: String::new(),
            attachments: Vec::new(),
            history: VecDeque::new(),
            capture: ClipboardCapture::new(store),
        }
    }

    pub fn input(&self) -> &str {
        &self.input
    }

    pub fn handle_event(
        &mut self,
        event: ReplEvent,
        clipboard: &impl ClipboardSource,
    ) -> Result<ReplAction, ReplError> {
        match event {
            ReplEvent::Text(text) => {
                self.input.push_str(&text);
                Ok(ReplAction::Continue)
            }
            ReplEvent::Paste(text) => {
                // Insert the pasted block verbatim, newlines and all. Because it
                // arrives as one event it cannot be misread as a series of Enter
                // key presses that would submit the prompt mid-paste.
                self.input.push_str(&text);
                Ok(ReplAction::Continue)
            }
            ReplEvent::Backspace => {
                self.input.pop();
                Ok(ReplAction::Continue)
            }
            ReplEvent::CtrlV => {
                if let Some(attachment) = self.capture.capture(clipboard)? {
                    self.apply_attachment(&attachment);
                    self.attachments.push(attachment);
                }
                Ok(ReplAction::Continue)
            }
            ReplEvent::CtrlC => Ok(ReplAction::Exit),
            ReplEvent::Submit => {
                let message = self.input.trim().to_string();
                self.input.clear();
                if let Some(action) = self.parse_slash_command(&message) {
                    self.attachments.clear();
                    return Ok(action);
                }

                let submission = ReplSubmission {
                    message,
                    attachments: std::mem::take(&mut self.attachments),
                };
                if submission.message.is_empty() && submission.attachments.is_empty() {
                    Ok(ReplAction::Continue)
                } else {
                    if !submission.message.is_empty() {
                        self.push_history(submission.message.clone());
                    }
                    Ok(ReplAction::Submit(submission))
                }
            }
        }
    }

    fn apply_attachment(&mut self, attachment: &ClipboardAttachment) {
        if attachment.kind == "text" {
            if let Some(text) = attachment.prompt_fragment.strip_prefix("clipboard text:\n") {
                self.input.push_str(text);
            } else {
                self.push_prompt_fragment(&attachment.prompt_fragment);
            }
        } else {
            self.push_prompt_fragment(&attachment.prompt_fragment);
        }
    }

    fn push_prompt_fragment(&mut self, fragment: &str) {
        if !self.input.is_empty() && !self.input.ends_with(char::is_whitespace) {
            self.input.push('\n');
        }
        self.input.push_str(fragment);
    }

    fn push_history(&mut self, message: String) {
        self.history.push_back(message);
        while self.history.len() > MAX_REPL_HISTORY {
            self.history.pop_front();
        }
    }

    fn search_history(&self, query: &str) -> Vec<ReplHistoryMatch> {
        let query = query.to_ascii_lowercase();
        self.history
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, message)| message.to_ascii_lowercase().contains(&query))
            .map(|(index, message)| ReplHistoryMatch {
                index: index + 1,
                message: message.clone(),
            })
            .collect()
    }

    fn parse_slash_command(&self, input: &str) -> Option<ReplAction> {
        if !input.starts_with('/') {
            return None;
        }

        let mut parts = input.split_whitespace();
        let command = parts.next().unwrap_or_default();
        match command {
            "/history" => {
                let query = parts.collect::<Vec<_>>().join(" ");
                if query.trim().is_empty() {
                    return Some(ReplAction::CommandError(
                        "usage: /history QUERY".to_string(),
                    ));
                }
                Some(ReplAction::HistorySearch(self.search_history(&query)))
            }
            "/new" => Some(ReplAction::NewSession),
            "/model" => {
                let provider_name = parts.next();
                let model = parts.next();
                if provider_name.is_none() || model.is_none() || parts.next().is_some() {
                    return Some(ReplAction::CommandError(
                        "usage: /model PROVIDER MODEL".to_string(),
                    ));
                }

                Some(ReplAction::SwitchModel(ReplModelSelection {
                    provider_name: provider_name
                        .expect("provider presence checked")
                        .to_string(),
                    model: model.expect("model presence checked").to_string(),
                }))
            }
            other => Some(ReplAction::CommandError(format!(
                "unknown command: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReplOptions {
    pub provider: ProviderConfig,
    pub provider_catalog: Vec<ProviderConfig>,
    pub model: String,
    pub workspace: PathBuf,
    pub timeout: Duration,
    pub max_rounds: usize,
    pub max_tool_concurrency: Option<usize>,
    pub tool_timeout: Option<Duration>,
}

pub fn run_terminal_repl<W: Write>(options: ReplOptions, output: &mut W) -> Result<(), ReplError> {
    let ReplOptions {
        provider,
        provider_catalog,
        model,
        workspace,
        timeout,
        max_rounds,
        max_tool_concurrency,
        tool_timeout,
    } = options;
    let mut active_provider = provider;
    let mut active_model = model;
    let catalog = if provider_catalog.is_empty() {
        vec![active_provider.clone()]
    } else {
        provider_catalog
    };

    let _raw = RawModeGuard::enable()?;
    let clipboard = SystemClipboard;
    let mut session = ReplSession::new(AttachmentStore::new(&workspace));
    let mut chat = ChatSession::start(
        SessionStore::for_workspace(&workspace),
        &workspace,
        active_provider.name(),
        &active_model,
    );

    writeln!(
        output,
        "harness repl. Ctrl+V paste, Enter send, Ctrl+C exit, /model PROVIDER MODEL switch, /new fresh session."
    )?;
    for notice in chat.take_notices() {
        writeln!(output, "{notice}")?;
    }
    write!(output, "> ")?;
    output.flush()?;

    let mut reader = ReplEventReader::new();
    loop {
        let repl_event = reader.next()?;
        let action = session.handle_event(repl_event.clone(), &clipboard)?;
        render_event_feedback(&repl_event, session.input(), output)?;

        match action {
            ReplAction::Continue => {}
            ReplAction::Exit => {
                writeln!(output)?;
                break;
            }
            ReplAction::CommandError(err) => {
                writeln!(output)?;
                writeln!(output, "{err}")?;
                write!(output, "> ")?;
                output.flush()?;
            }
            ReplAction::SwitchModel(selection) => {
                writeln!(output)?;
                match resolve_model_selection(&catalog, &selection) {
                    Ok(provider) => {
                        active_provider = provider;
                        active_model = selection.model;
                        writeln!(
                            output,
                            "switched to {}/{}",
                            active_provider.name(),
                            active_model
                        )?;
                    }
                    Err(err) => writeln!(output, "{err}")?,
                }
                write!(output, "> ")?;
                output.flush()?;
            }
            ReplAction::HistorySearch(matches) => {
                writeln!(output)?;
                if matches.is_empty() {
                    writeln!(output, "no history matches")?;
                } else {
                    for history_match in matches {
                        writeln!(
                            output,
                            "[{}] {}",
                            history_match.index, history_match.message
                        )?;
                    }
                }
                write!(output, "> ")?;
                output.flush()?;
            }
            ReplAction::NewSession => {
                writeln!(output)?;
                chat.start_new(active_provider.name(), &active_model);
                writeln!(output, "started new session")?;
                for notice in chat.take_notices() {
                    writeln!(output, "{notice}")?;
                }
                write!(output, "> ")?;
                output.flush()?;
            }
            ReplAction::Submit(submission) => {
                writeln!(output)?;
                let mut render_error = None;
                let mut runner =
                    AgentRunner::new(active_provider.clone(), active_model.clone(), &workspace)
                        .with_timeout(timeout)
                        .with_max_tool_rounds(max_rounds)
                        .with_history(chat.history());
                if let Some(max_tool_concurrency) = max_tool_concurrency {
                    runner = runner.with_max_tool_concurrency(max_tool_concurrency);
                }
                if let Some(tool_timeout) = tool_timeout {
                    runner = runner.with_tool_batch_timeout(tool_timeout);
                }

                chat.begin_turn(&submission.message);
                let run = runner.run_with_events(submission.message, |event| {
                    if render_error.is_none()
                        && let Err(err) = render_agent_event(&event, output)
                    {
                        render_error = Some(err);
                    }
                });
                match run {
                    Ok(result) => chat.complete_turn(&result),
                    Err(err) => {
                        chat.fail_turn(&err);
                        return Err(err.into());
                    }
                }
                if let Some(err) = render_error {
                    return Err(ReplError::Io(err));
                }
                for notice in chat.take_notices() {
                    writeln!(output, "{notice}")?;
                }
                writeln!(output)?;
                write!(output, "> ")?;
                output.flush()?;
            }
        }
    }

    Ok(())
}

/// Run the interactive chat session on harness-tui: finished chat blocks
/// flow into the terminal's native scrollback (wheel-scrollable,
/// selectable, they survive exit) and only the bottom panel — live
/// blocks, spinner, prompt editor, completions, status — is repainted.
/// Used on a real terminal; non-TTY callers stay on `run_terminal_repl`.
pub fn run_chat_tui(options: ReplOptions) -> Result<(), ReplError> {
    let ReplOptions {
        provider,
        provider_catalog,
        model,
        workspace,
        timeout,
        max_rounds,
        max_tool_concurrency,
        tool_timeout,
    } = options;
    let mut active_provider = provider;
    let mut active_model = model;
    let catalog = if provider_catalog.is_empty() {
        vec![active_provider.clone()]
    } else {
        provider_catalog
    };

    tui_terminal::install_panic_restore();
    let mut screen = Screen::stdout().map_err(tui_io_error)?;
    let clipboard = SystemClipboard;
    let mut capture = ClipboardCapture::new(AttachmentStore::new(&workspace));
    // Key hints live in the persistent status line, so no welcome banner is
    // needed in the transcript.
    let mut app = ChatApp::new(
        format!("{}/{}", active_provider.name(), active_model),
        &workspace,
    );
    let mut chat = ChatSession::start(
        SessionStore::for_workspace(&workspace),
        &workspace,
        active_provider.name(),
        &active_model,
    );
    render_resumed_history(&mut app, &chat.history());
    for notice in chat.take_notices() {
        app.push_system_line(notice);
    }

    let mut pump = InputPump::start();

    'session: loop {
        draw_chat(&mut screen, &mut app)?;
        let events = pump
            .poll(Duration::from_millis(400))
            .map_err(ReplError::Io)?;
        check_resize(&mut screen)?;
        for event in events {
            let action = match event {
                TuiEvent::Paste(text) => app.handle_paste(&text),
                TuiEvent::Key(key) => app.handle_key(key),
                _ => ChatAction::Continue,
            };
            match action {
                ChatAction::Continue => {}
                ChatAction::Exit => break 'session,
                ChatAction::CaptureClipboard => {
                    capture_into_chat(&mut app, &mut capture, &clipboard);
                }
                ChatAction::SwitchModel { provider, model } => {
                    match resolve_model_selection(
                        &catalog,
                        &ReplModelSelection {
                            provider_name: provider,
                            model: model.clone(),
                        },
                    ) {
                        Ok(resolved) => {
                            active_provider = resolved;
                            active_model = model;
                            app.set_provider_label(format!(
                                "{}/{}",
                                active_provider.name(),
                                active_model
                            ));
                            app.push_system_line(format!(
                                "switched to {}/{}",
                                active_provider.name(),
                                active_model
                            ));
                        }
                        Err(err) => app.push_system_line(err),
                    }
                }
                ChatAction::NewSession => {
                    chat.start_new(active_provider.name(), &active_model);
                    // The app already dropped its transcript; wipe the terminal
                    // too so the old conversation does not linger on screen.
                    screen.clear().map_err(ReplError::Io)?;
                    app.push_system_line("started new session");
                    for notice in chat.take_notices() {
                        app.push_system_line(notice);
                    }
                }
                ChatAction::ClearScreen => {
                    screen.clear().map_err(ReplError::Io)?;
                }
                ChatAction::Submit(message) => {
                    app.set_busy(true);
                    draw_chat(&mut screen, &mut app)?;
                    let mut runner =
                        AgentRunner::new(active_provider.clone(), active_model.clone(), &workspace)
                            .with_timeout(timeout)
                            .with_max_tool_rounds(max_rounds)
                            .with_streaming(true)
                            .with_history(chat.history());
                    if let Some(max_tool_concurrency) = max_tool_concurrency {
                        runner = runner.with_max_tool_concurrency(max_tool_concurrency);
                    }
                    if let Some(tool_timeout) = tool_timeout {
                        runner = runner.with_tool_batch_timeout(tool_timeout);
                    }

                    // The run executes on a worker thread while this loop keeps
                    // draining agent events into the transcript and polling the
                    // keyboard, so Esc/Ctrl+C can interrupt a busy agent.
                    let cancel = Arc::new(AtomicBool::new(false));
                    let runner = runner.with_cancel_flag(cancel.clone());
                    chat.begin_turn(&message);
                    let (event_tx, event_rx) = mpsc::channel();
                    let worker = thread::spawn(move || {
                        runner.run_with_events(message, move |event| {
                            let _ = event_tx.send(event);
                        })
                    });
                    let run = loop {
                        while let Ok(event) = event_rx.try_recv() {
                            app.push_agent_event(&event);
                        }
                        app.tick();
                        check_resize(&mut screen)?;
                        draw_chat(&mut screen, &mut app)?;
                        // Poll input BEFORE the finished check so a
                        // queued Esc is consumed as a busy cancel and
                        // can never leak into the idle loop as an exit.
                        for event in pump
                            .poll(Duration::from_millis(50))
                            .map_err(ReplError::Io)?
                        {
                            if matches!(busy_action(&event), BusyAction::Cancel) {
                                cancel.store(true, Ordering::SeqCst);
                            }
                        }
                        if worker.is_finished() {
                            break worker.join().expect("agent worker panicked");
                        }
                    };
                    while let Ok(event) = event_rx.try_recv() {
                        app.push_agent_event(&event);
                    }
                    app.set_busy(false);
                    match run {
                        Ok(result) => chat.complete_turn(&result),
                        Err(err @ AgentError::Cancelled { .. }) => {
                            chat.fail_turn(&err);
                            app.push_system_line("Interrupted by user");
                        }
                        Err(err) => {
                            chat.fail_turn(&err);
                            app.push_system_line(format!("error: {err}"));
                        }
                    }
                    for notice in chat.take_notices() {
                        app.push_system_line(notice);
                    }
                    draw_chat(&mut screen, &mut app)?;
                }
            }
        }
    }

    let _ = screen.release();
    Ok(())
}

fn tui_io_error(err: tui_terminal::TerminalError) -> ReplError {
    ReplError::Io(io::Error::other(err.to_string()))
}

/// Flush finalized transcript blocks into native scrollback, then
/// repaint the pinned panel, capped to the screen height.
fn draw_chat(screen: &mut Screen, app: &mut ChatApp) -> Result<(), ReplError> {
    let width = screen.width() as usize;
    let height = screen.height() as usize;
    let scrollback = app.take_scrollback(width);
    if !scrollback.is_empty() {
        screen.emit(&scrollback).map_err(ReplError::Io)?;
    }
    // Reserve rows for editor (≤6) + menu/palette + spinner + status; the
    // live block area gets the rest and shows its tail when over budget.
    let max_live_rows = height.saturating_sub(12).max(3);
    let mut panel = app.panel_lines(width, max_live_rows);
    let max_panel = height.saturating_sub(1).max(1);
    if panel.len() > max_panel {
        panel = panel.split_off(panel.len() - max_panel);
    }
    screen.render_panel(panel).map_err(ReplError::Io)
}

/// The terminal delivers no resize signal we listen for — the idle loop
/// polls the size once per tick, which is cheap and cross-platform.
fn check_resize(screen: &mut Screen) -> Result<(), ReplError> {
    if let Ok((width, height)) = tui_terminal::size()
        && (width != screen.width() || height != screen.height())
    {
        screen.resize(width, height).map_err(ReplError::Io)?;
    }
    Ok(())
}

/// Reads raw stdin bytes on a background thread; the UI thread polls
/// decoded events with a timeout so it can animate the spinner, notice
/// resizes, and keep draining agent events while a run is busy. Shared
/// with the setup TUI (`crate::tui`), which runs the same poll loop.
pub(crate) struct InputPump {
    parser: Parser,
}

/// One process-wide stdin reader: front ends hand off within a single
/// process (setup TUI -> chat TUI), and two blocking readers would race
/// for stdin and drop the winner's bytes. The thread is spawned once;
/// every InputPump polls the same channel (consumers are sequential).
static STDIN_CHUNKS: std::sync::OnceLock<std::sync::Mutex<mpsc::Receiver<io::Result<Vec<u8>>>>> =
    std::sync::OnceLock::new();

impl InputPump {
    pub(crate) fn start() -> Self {
        STDIN_CHUNKS.get_or_init(|| {
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let mut buf = [0u8; 1024];
                loop {
                    match tui_terminal::read_input(&mut buf) {
                        Ok(0) => {
                            let _ = tx.send(Err(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "stdin closed",
                            )));
                            break;
                        }
                        Ok(n) => {
                            if tx.send(Ok(buf[..n].to_vec())).is_err() {
                                break;
                            }
                        }
                        Err(err) => {
                            let _ = tx.send(Err(err));
                            break;
                        }
                    }
                }
            });
            std::sync::Mutex::new(rx)
        });
        InputPump {
            parser: Parser::new(),
        }
    }

    /// Wait up to `timeout` for input, then drain the short burst that
    /// follows and coalesce it into paste blocks - the classic defense
    /// against legacy-console paste keystreams. EOF and read errors
    /// surface as Err so the caller exits instead of spinning idle.
    pub(crate) fn poll(&mut self, timeout: Duration) -> io::Result<Vec<TuiEvent>> {
        let rx = STDIN_CHUNKS
            .get()
            .expect("InputPump::start spawns the reader")
            .lock()
            .expect("stdin pump lock");
        match rx.recv_timeout(timeout) {
            Ok(Ok(chunk)) => {
                let mut events = self.parser.feed(&chunk);
                while let Ok(Ok(chunk)) = rx.recv_timeout(Duration::from_millis(3)) {
                    events.extend(self.parser.feed(&chunk));
                }
                // The burst window doubles as the Esc disambiguation
                // timeout: a lone Esc resolves right here.
                events.extend(self.parser.flush());
                Ok(coalesce_burst(events))
            }
            Ok(Err(err)) => Err(err),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(self.parser.flush()),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "stdin reader stopped",
            )),
        }
    }
}

/// Replay a resumed conversation into the TUI transcript. Only the human-facing
/// turns are rendered — tool exchanges stay in the session file (and traces)
/// for offline analysis rather than flooding the transcript on startup.
fn render_resumed_history(app: &mut ChatApp, history: &[ChatMessage]) {
    for message in history {
        match message.role() {
            "user" => app.push_user_message(message.content()),
            "assistant" if !message.content().is_empty() => {
                app.push_agent_event(&AgentEvent::FinalContentDelta(
                    message.content().to_string(),
                ));
            }
            _ => {}
        }
    }
}

/// Capture the system clipboard for Ctrl+V: text is inserted at the caret, an
/// image is saved to `.harness/attachments` and referenced in the prompt.
fn capture_into_chat(
    app: &mut ChatApp,
    capture: &mut ClipboardCapture,
    clipboard: &impl ClipboardSource,
) {
    match capture.capture(clipboard) {
        Ok(Some(attachment)) => {
            if attachment.kind == "text" {
                if let Some(text) = attachment.prompt_fragment.strip_prefix("clipboard text:\n") {
                    app.apply_clipboard_text(text);
                } else {
                    app.apply_clipboard_text(&attachment.prompt_fragment);
                }
            } else {
                app.apply_clipboard_text(&attachment.prompt_fragment);
                app.push_system_line(format!("attached {} from clipboard", attachment.kind));
            }
        }
        Ok(None) => app.push_system_line("clipboard is empty"),
        Err(err) => app.push_system_line(format!("clipboard error: {err}")),
    }
}

pub fn render_agent_event<W: Write>(event: &AgentEvent, output: &mut W) -> io::Result<()> {
    match event {
        AgentEvent::Thinking(text) => {
            writeln!(output, "thinking: {}", summarize_one_line(text, 120))?;
        }
        AgentEvent::ToolRoundStarted { round, tool_calls } => {
            writeln!(output, "tool round {round}: running {tool_calls} call(s)")?;
        }
        AgentEvent::ToolCallStarted {
            name, arguments, ..
        } => {
            writeln!(
                output,
                "  → {name} {}",
                summarize_one_line(&arguments.to_string(), 120)
            )?;
        }
        AgentEvent::ToolResult(result) => {
            let status = if result.ok { "ok" } else { "error" };
            writeln!(output, "tool {} {} {}", result.id, result.tool_name, status)?;
        }
        AgentEvent::FinalContentDelta(delta) => {
            write!(output, "{delta}")?;
        }
    }
    output.flush()
}

/// Flatten text to a single line and clip it to `max` chars for compact status
/// lines (thinking previews, tool-argument echoes).
pub fn summarize_one_line(text: &str, max: usize) -> String {
    let flattened = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flattened.chars().count() > max {
        let clipped: String = flattened.chars().take(max.saturating_sub(1)).collect();
        format!("{clipped}…")
    } else {
        flattened
    }
}

pub fn resolve_model_selection(
    catalog: &[ProviderConfig],
    selection: &ReplModelSelection,
) -> Result<ProviderConfig, String> {
    let provider = catalog
        .iter()
        .find(|provider| provider.name() == selection.provider_name)
        .ok_or_else(|| format!("unknown provider: {}", selection.provider_name))?;

    if !provider.models().is_empty() && !provider.models().contains(&selection.model) {
        return Err(format!(
            "model {} is not configured for provider {}",
            selection.model, selection.provider_name
        ));
    }

    Ok(provider.clone())
}

#[derive(Debug)]
pub enum ReplError {
    Clipboard(ClipboardError),
    Agent(AgentError),
    Io(io::Error),
}

impl fmt::Display for ReplError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Clipboard(err) => write!(f, "{err}"),
            Self::Agent(err) => write!(f, "{err}"),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl Error for ReplError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Clipboard(err) => Some(err),
            Self::Agent(err) => Some(err),
            Self::Io(err) => Some(err),
        }
    }
}

impl From<ClipboardError> for ReplError {
    fn from(value: ClipboardError) -> Self {
        Self::Clipboard(value)
    }
}

impl From<AgentError> for ReplError {
    fn from(value: AgentError) -> Self {
        Self::Agent(value)
    }
}

impl From<io::Error> for ReplError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// Raw mode + bracketed paste for the line-mode REPL (visible cursor,
/// no panel). Bracketed paste makes the terminal wrap pasted input in
/// escape markers so the parser hands it back as one `Event::Paste`
/// instead of a stream of characters and Enter presses - without it,
/// pasting multi-line text would submit on the first newline.
struct RawModeGuard {
    _handle: tui_terminal::RawModeHandle,
}

impl RawModeGuard {
    fn enable() -> Result<Self, ReplError> {
        Ok(Self {
            _handle: tui_terminal::raw_mode().map_err(tui_io_error)?,
        })
    }
}

/// Blocking event reader for the line-mode REPL: reads raw stdin bytes,
/// decodes them with the harness-tui parser, and queues the results.
struct ReplEventReader {
    parser: Parser,
    queue: VecDeque<ReplEvent>,
}

impl ReplEventReader {
    fn new() -> Self {
        ReplEventReader {
            parser: Parser::new(),
            queue: VecDeque::new(),
        }
    }

    fn next(&mut self) -> Result<ReplEvent, ReplError> {
        loop {
            if let Some(event) = self.queue.pop_front() {
                return Ok(event);
            }
            let mut buf = [0u8; 1024];
            let n = tui_terminal::read_input(&mut buf).map_err(ReplError::Io)?;
            if n == 0 {
                // EOF: treat like Ctrl+C so the session ends cleanly.
                return Ok(ReplEvent::CtrlC);
            }
            for event in self.parser.feed(&buf[..n]) {
                if let Some(repl_event) = tui_event_to_repl(event) {
                    self.queue.push_back(repl_event);
                }
            }
        }
    }
}

fn tui_event_to_repl(event: TuiEvent) -> Option<ReplEvent> {
    match event {
        TuiEvent::Paste(text) => Some(ReplEvent::Paste(text)),
        TuiEvent::Key(key) => match key.code {
            TuiKeyCode::Char('c') if key.mods.ctrl => Some(ReplEvent::CtrlC),
            TuiKeyCode::Char('v') if key.mods.ctrl => Some(ReplEvent::CtrlV),
            TuiKeyCode::Enter => Some(ReplEvent::Submit),
            TuiKeyCode::Backspace => Some(ReplEvent::Backspace),
            TuiKeyCode::Char(ch) if !key.mods.ctrl => Some(ReplEvent::Text(ch.to_string())),
            _ => None,
        },
        _ => None,
    }
}

fn render_event_feedback<W: Write>(
    event: &ReplEvent,
    current_input: &str,
    output: &mut W,
) -> Result<(), ReplError> {
    match event {
        ReplEvent::Text(text) => write!(output, "{text}")?,
        ReplEvent::Paste(_) => {
            // The pasted text may span lines, so redraw the prompt with the full
            // current input rather than echoing raw bytes into the terminal.
            write!(output, "\r> {current_input}")?;
        }
        ReplEvent::Backspace => {
            write!(output, "\r> {current_input} ")?;
            write!(output, "\r> {current_input}")?;
        }
        ReplEvent::CtrlV => {
            write!(output, "\r> {current_input}")?;
        }
        ReplEvent::Submit | ReplEvent::CtrlC => {}
    }
    output.flush()?;
    Ok(())
}
