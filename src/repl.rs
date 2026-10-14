use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use harness_tui::fullscreen::FullScreen;
use harness_tui::input::{Event as TuiEvent, KeyCode as TuiKeyCode, Parser, coalesce_burst};
use harness_tui::terminal as tui_terminal;
use harness_tui::theme::Theme;

use crate::agent::{AgentError, AgentEvent, AgentRunner};
use crate::chat::{BusyAction, ChatAction, ChatApp};
use crate::chat_client::WireTap;
use crate::clipboard::{
    AttachmentStore, ClipboardAttachment, ClipboardCapture, ClipboardError, ClipboardSource,
    SystemClipboard,
};
use crate::config::ConfigStore;
use crate::model_client::OpenAiCompatibleModelClient;
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
    /// `/model ARG` with one token — resolved against the provider catalog
    /// (menu number, provider, model, or a brand-new model name) by the
    /// run loop, which owns the catalog.
    SwitchModelShorthand(String),
    /// `/model` with no arguments: list the saved provider/model pairs.
    ShowModels,
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
        match attachment.inline_text() {
            Some(text) => self.input.push_str(text),
            None => self.push_prompt_fragment(&attachment.prompt_fragment),
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
            "/exit" | "/quit" => Some(ReplAction::Exit),
            "/model" => {
                let first = parts.next();
                let second = parts.next();
                match (first, second) {
                    (Some(provider_name), Some(model)) if parts.next().is_none() => {
                        Some(ReplAction::SwitchModel(ReplModelSelection {
                            provider_name: provider_name.to_string(),
                            model: model.to_string(),
                        }))
                    }
                    (Some(arg), None) => Some(ReplAction::SwitchModelShorthand(arg.to_string())),
                    (None, None) => Some(ReplAction::ShowModels),
                    _ => Some(ReplAction::CommandError(
                        "usage: /model [N | MODEL | PROVIDER MODEL]".to_string(),
                    )),
                }
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
    /// Where the provider config lives, so an in-session switch to a
    /// brand-new model name can be saved back for future menus. `None`
    /// (tests, ad-hoc sessions) simply skips persistence.
    pub config_path: Option<PathBuf>,
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
        config_path,
    } = options;
    let mut active_provider = provider;
    let mut active_model = model;
    let mut catalog = provider_catalog;

    // Raw mode needs a real console; a fully piped run (tests, scripts,
    // `echo … | harness repl > out`) has none, and cooked pipe input is
    // exactly what the line reader wants there — so a failure is fine.
    let _raw = RawModeGuard::enable().ok();
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
        "harness repl. Ctrl+V paste, Enter send, Ctrl+C exit, /model to pick a model, /new fresh session."
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
            ReplAction::Continue => continue,
            ReplAction::Exit => {
                writeln!(output)?;
                break;
            }
            ReplAction::CommandError(err) => {
                writeln!(output)?;
                writeln!(output, "{err}")?;
            }
            ReplAction::SwitchModel(selection) => {
                writeln!(output)?;
                match perform_model_switch(&mut catalog, config_path.as_deref(), &selection) {
                    Ok((provider, notes)) => {
                        active_provider = provider;
                        active_model = selection.model;
                        for note in notes {
                            writeln!(output, "{note}")?;
                        }
                    }
                    Err(err) => writeln!(output, "{err}")?,
                }
            }
            ReplAction::SwitchModelShorthand(arg) => {
                writeln!(output)?;
                let pairs = catalog_pairs(&catalog);
                let resolved =
                    crate::providers::resolve_model_arg(&pairs, active_provider.name(), &arg);
                match resolved.and_then(|(provider_name, model)| {
                    let selection = ReplModelSelection {
                        provider_name,
                        model,
                    };
                    perform_model_switch(&mut catalog, config_path.as_deref(), &selection)
                        .map(|switched| (switched, selection.model))
                }) {
                    Ok(((provider, notes), model)) => {
                        active_provider = provider;
                        active_model = model;
                        for note in notes {
                            writeln!(output, "{note}")?;
                        }
                    }
                    Err(err) => writeln!(output, "{err}")?,
                }
            }
            ReplAction::ShowModels => {
                writeln!(output)?;
                writeln!(output, "available models:")?;
                let mut index = 0;
                for (provider_name, models) in catalog_pairs(&catalog) {
                    for model in models {
                        index += 1;
                        let marker =
                            if provider_name == active_provider.name() && model == active_model {
                                " (active)"
                            } else {
                                ""
                            };
                        writeln!(output, "  [{index}] {provider_name}/{model}{marker}")?;
                    }
                }
                writeln!(
                    output,
                    "switch: /model N, /model MODEL, /model PROVIDER MODEL (new model names are saved)"
                )?;
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
            }
            ReplAction::NewSession => {
                writeln!(output)?;
                chat.start_new(active_provider.name(), &active_model);
                writeln!(output, "started new session")?;
                for notice in chat.take_notices() {
                    writeln!(output, "{notice}")?;
                }
            }
            ReplAction::Submit(submission) => {
                writeln!(output)?;
                let mut render_error = None;
                let runner = build_runner(
                    &active_provider,
                    &active_model,
                    &workspace,
                    &chat,
                    max_tool_concurrency,
                    tool_timeout,
                )
                .with_timeout(timeout)
                .with_max_tool_rounds(max_rounds)
                // Same default as the TUI: thinking models need the
                // reasoning echo on every assistant turn.
                .with_retain_thinking(true);

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
            }
        }
        write!(output, "> ")?;
        output.flush()?;
    }

    Ok(())
}

/// Run the interactive chat session full screen on harness-tui: the
/// transcript scrolls inside the window, the editor and status rows stay
/// on the bottom edge, a mouse drag copies transcript text. On exit the
/// terminal's own screen comes back and the conversation is printed into
/// its native scrollback. Used on a real terminal; non-TTY callers stay
/// on `run_terminal_repl`.
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
        config_path,
    } = options;
    let mut active_provider = provider;
    let mut active_model = model;
    let mut catalog = provider_catalog;

    tui_terminal::install_panic_restore();
    let mut screen = FullScreen::stdout().map_err(|err| ReplError::Io(err.into()))?;
    let clipboard = SystemClipboard;
    let mut capture = ClipboardCapture::new(AttachmentStore::new(&workspace));
    let provider_label = format!("{}/{}", active_provider.name(), active_model);
    let theme = Theme::for_background(screen.background());
    let truecolor = tui_terminal::supports_truecolor();
    let theme = if truecolor { theme } else { theme.quantized() };
    // The fade-in blends RGB colors: only with truecolor.
    let (foreground, background) = if truecolor {
        (screen.foreground(), screen.background())
    } else {
        (None, None)
    };
    let mut app = ChatApp::new(provider_label, &workspace)
        .with_catalog(catalog_pairs(&catalog))
        .with_theme(theme)
        .with_animations(true)
        .with_terminal_colors(foreground, background);
    app.push_banner(env!("CARGO_PKG_VERSION"));
    let mut chat = ChatSession::start(
        SessionStore::for_workspace(&workspace),
        &workspace,
        active_provider.name(),
        &active_model,
    );
    render_resumed_history(&mut app, &chat.history());
    app.finish_reveals();
    for notice in chat.take_notices() {
        app.push_system_line(notice);
    }

    let mut pump = InputPump::start();
    let mut window_probe = model_metadata(&active_provider, &active_model, &mut app);
    // The `/provider add` model list request, while it runs.
    let mut model_fetch: Option<mpsc::Receiver<crate::tui::FetchResult>> = None;

    'session: loop {
        take_context_window(&mut window_probe, &mut app);
        if let Some(result) = model_fetch.as_ref().and_then(|fetch| fetch.try_recv().ok()) {
            app.models_loaded(result);
            model_fetch = None;
        }
        draw_chat(&mut screen, &mut app)?;
        // Animations need frames without input; an idle screen does not.
        let poll_window = if app.is_animating() || model_fetch.is_some() {
            FRAME_INTERVAL
        } else {
            Duration::from_millis(100)
        };
        // A prompt queued during the last run goes first, without waiting
        // for input; otherwise poll the keyboard and mouse.
        let inputs: Vec<LoopInput> = match app.take_queued() {
            Some(action) => vec![LoopInput::Queued(Box::new(action))],
            None => pump
                .poll(poll_window)
                .map_err(ReplError::Io)?
                .into_iter()
                .map(LoopInput::Event)
                .collect(),
        };
        for input in inputs {
            let action = match input {
                LoopInput::Queued(action) => *action,
                LoopInput::Event(event) => app.handle_event(event),
            };
            match action {
                ChatAction::Continue => {}
                ChatAction::Exit => break 'session,
                ChatAction::CaptureClipboard => {
                    capture_into_chat(&mut app, &mut capture, &clipboard);
                }
                ChatAction::Copy(text) => screen.copy(&text).map_err(ReplError::Io)?,
                ChatAction::FetchModels(provider) => {
                    model_fetch = Some(crate::tui::spawn_model_fetch(provider));
                }
                ChatAction::SaveProvider { provider, model } => {
                    let name = provider.name().to_string();
                    match &config_path {
                        Some(path) => {
                            match ConfigStore::new(path).save_provider(provider.clone()) {
                                Ok(()) => app.push_system_line(format!(
                                    "saved provider {name} with {} model(s)",
                                    provider.models().len()
                                )),
                                Err(err) => app.push_system_line(format!(
                                    "note: provider {name} not saved to config: {err}"
                                )),
                            }
                        }
                        None => app.push_system_line(format!(
                            "note: no config file — provider {name} lasts for this session only"
                        )),
                    }
                    for saved in provider.models() {
                        app.add_catalog_model(&name, saved);
                    }
                    match catalog.iter_mut().find(|entry| entry.name() == name) {
                        Some(entry) => *entry = provider,
                        None => catalog.push(provider),
                    }
                    switch_model(
                        &mut catalog,
                        config_path.as_deref(),
                        name,
                        model,
                        (&mut active_provider, &mut active_model),
                        &mut app,
                        &mut window_probe,
                    );
                }
                ChatAction::SwitchModel { provider, model } => switch_model(
                    &mut catalog,
                    config_path.as_deref(),
                    provider,
                    model,
                    (&mut active_provider, &mut active_model),
                    &mut app,
                    &mut window_probe,
                ),
                ChatAction::NewSession => {
                    chat.start_new(active_provider.name(), &active_model);
                    app.push_system_line("started new session");
                    for notice in chat.take_notices() {
                        app.push_system_line(notice);
                    }
                }
                ChatAction::Submit(message) => {
                    app.set_busy(true);
                    draw_chat(&mut screen, &mut app)?;
                    let runner = build_runner(
                        &active_provider,
                        &active_model,
                        &workspace,
                        &chat,
                        max_tool_concurrency,
                        tool_timeout,
                    )
                    .with_timeout(timeout)
                    .with_max_tool_rounds(max_rounds)
                    .with_streaming(true)
                    .with_retain_thinking(app.retain_thinking());

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
                        take_context_window(&mut window_probe, &mut app);
                        draw_chat(&mut screen, &mut app)?;
                        // Poll input BEFORE the finished check so a
                        // queued Esc is consumed as a busy cancel and
                        // can never leak into the idle loop as an exit.
                        for event in pump.poll(FRAME_INTERVAL).map_err(ReplError::Io)? {
                            match app.busy_action(&event) {
                                BusyAction::Cancel => cancel.store(true, Ordering::SeqCst),
                                // The user keeps typing, scrolling and
                                // selecting; Enter queues the prompt for
                                // after this run (the app holds the queue).
                                BusyAction::Forward => match app.handle_event(event) {
                                    ChatAction::CaptureClipboard => {
                                        capture_into_chat(&mut app, &mut capture, &clipboard);
                                    }
                                    ChatAction::Copy(text) => {
                                        screen.copy(&text).map_err(ReplError::Io)?;
                                    }
                                    _ => {}
                                },
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
                    if run.is_err() {
                        app.restore_queued_to_editor();
                    }
                    match run {
                        Ok(result) => chat.complete_turn(&result),
                        Err(err @ AgentError::Cancelled { .. }) => {
                            chat.fail_turn(&err);
                            app.push_system_line("Interrupted by user");
                        }
                        Err(err) => {
                            let turn_kept = err.messages().is_some();
                            chat.fail_turn(&err);
                            app.push_system_line(format!("error: {err}"));
                            if turn_kept {
                                app.push_system_line(
                                    "turn kept in history — send a follow-up to resume",
                                );
                            }
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

    // Back on the terminal's own screen: leave the conversation in its
    // scrollback, fully revealed, like a line-mode session would.
    screen.leave().map_err(ReplError::Io)?;
    let width = screen.width() as usize;
    screen
        .print(&app.exit_transcript(width))
        .map_err(ReplError::Io)?;
    Ok(())
}

/// Switch the session to `provider`/`model` and refresh the model's
/// window and prices.
fn switch_model(
    catalog: &mut [ProviderConfig],
    config_path: Option<&Path>,
    provider: String,
    model: String,
    (active_provider, active_model): (&mut ProviderConfig, &mut String),
    app: &mut ChatApp,
    window_probe: &mut Option<mpsc::Receiver<Option<u64>>>,
) {
    let selection = ReplModelSelection {
        provider_name: provider,
        model: model.clone(),
    };
    match perform_model_switch(catalog, config_path, &selection) {
        Ok((resolved, notes)) => {
            *active_provider = resolved;
            *active_model = model;
            app.set_provider_label(format!("{}/{}", active_provider.name(), active_model));
            app.add_catalog_model(active_provider.name(), active_model);
            // A different model, a different window and price.
            *window_probe = model_metadata(active_provider, active_model, app);
            for note in notes {
                app.push_system_line(note);
            }
        }
        Err(err) => app.push_system_line(err),
    }
}

/// Give the app the model's prices and context window. A window saved
/// when the provider was added is used as is; otherwise it is probed.
fn model_metadata(
    provider: &ProviderConfig,
    model: &str,
    app: &mut ChatApp,
) -> Option<mpsc::Receiver<Option<u64>>> {
    app.set_pricing(provider.pricing(model));
    let saved = provider
        .model_info(model)
        .and_then(|info| info.context_window);
    app.set_context_window(saved);
    match saved {
        Some(_) => None,
        None => Some(probe_context_window(provider, model)),
    }
}

/// Ask the provider's model list for `model`'s context window on a worker
/// thread, so a slow or missing `/models` endpoint never delays the chat.
/// The answer is `None` when the list does not report a window.
fn probe_context_window(provider: &ProviderConfig, model: &str) -> mpsc::Receiver<Option<u64>> {
    let (sender, receiver) = mpsc::channel();
    let provider = provider.clone();
    let model = model.to_string();
    thread::spawn(move || {
        let window = OpenAiCompatibleModelClient::new(CONTEXT_PROBE_TIMEOUT)
            .list_models(&provider)
            .ok()
            .and_then(|discovery| discovery.context_window(&model));
        let _ = sender.send(window);
    });
    receiver
}

/// Hand a finished context-window probe to the app. A probe replaced by
/// a newer one (model switch) is dropped with its receiver.
fn take_context_window(probe: &mut Option<mpsc::Receiver<Option<u64>>>, app: &mut ChatApp) {
    let Some(receiver) = probe else {
        return;
    };
    match receiver.try_recv() {
        Ok(window) => {
            app.set_context_window(window);
            *probe = None;
        }
        Err(mpsc::TryRecvError::Disconnected) => *probe = None,
        Err(mpsc::TryRecvError::Empty) => {}
    }
}

/// How long the context-window probe waits for `/models`.
const CONTEXT_PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// Frame interval while something animates (~30 frames per second):
/// smooth for revealed text and the spinner, cheap because unchanged
/// rows diff to nothing.
const FRAME_INTERVAL: Duration = Duration::from_millis(33);

/// One step of the chat loop: a terminal event, or a prompt the user
/// queued while the previous run was busy.
enum LoopInput {
    Event(TuiEvent),
    Queued(Box<ChatAction>),
}

/// One frame: adopt a changed terminal size (the alternate screen has no
/// scrollback to reflow, so a full repaint is the whole story), advance
/// the animations, and paint the app's frame.
fn draw_chat(screen: &mut FullScreen, app: &mut ChatApp) -> Result<(), ReplError> {
    if let Ok((width, height)) = tui_terminal::size()
        && (width != screen.width() || height != screen.height())
    {
        screen.resize(width, height).map_err(ReplError::Io)?;
    }
    app.animate(Instant::now());
    let frame = app.frame(screen.width() as usize, screen.height() as usize);
    screen.draw(frame).map_err(ReplError::Io)
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
            let text = attachment
                .inline_text()
                .unwrap_or(&attachment.prompt_fragment);
            app.apply_clipboard_text(text);
            if attachment.kind != "text" {
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
        // Line mode keeps quiet about usage; the totals live in the trace.
        AgentEvent::UsageUpdated(_) => {}
        AgentEvent::ProviderSwitched {
            from_provider,
            from_model,
            to_provider,
            to_model,
            reason,
        } => {
            writeln!(
                output,
                "⚠ provider switch: {from_provider}/{from_model} → {to_provider}/{to_model} ({reason})"
            )?;
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

    // Any model name is accepted for a known provider: the user must be able
    // to type a model the config has never seen (it is then saved via
    // `persist_model_addition`). A typo fails loudly at request time instead.
    Ok(provider.clone())
}

/// The `(provider, models)` view of the catalog used by shorthand
/// resolution and the `/model` menus.
/// The runner core both front ends share: session history, optional tool
/// limits, and the wire tap into the session's raw log. Callers chain their
/// own timeout/rounds/streaming/retain-thinking on top.
fn build_runner(
    provider: &ProviderConfig,
    model: &str,
    workspace: &Path,
    chat: &ChatSession,
    max_tool_concurrency: Option<usize>,
    tool_timeout: Option<Duration>,
) -> AgentRunner {
    let mut runner = AgentRunner::new(provider.clone(), model.to_string(), workspace)
        .with_history(chat.history());
    if let Some(max_tool_concurrency) = max_tool_concurrency {
        runner = runner.with_max_tool_concurrency(max_tool_concurrency);
    }
    if let Some(tool_timeout) = tool_timeout {
        runner = runner.with_tool_batch_timeout(tool_timeout);
    }
    if let Some(wire_log) = chat.wire_log() {
        // Отказ диска не должен ронять чат — та же политика,
        // что у остальной персистентности.
        runner = runner.with_wire_tap(WireTap::new(move |event| {
            let _ = wire_log.append_event(event);
        }));
    }
    runner
}

fn catalog_pairs(catalog: &[ProviderConfig]) -> Vec<(String, Vec<String>)> {
    catalog
        .iter()
        .map(|provider| (provider.name().to_string(), provider.models().to_vec()))
        .collect()
}

/// Resolve a switch, persist a brand-new model name when the config path is
/// known, and mirror it into the in-memory catalog so menus stay current.
/// Returns the provider to activate plus the lines to show the user.
fn perform_model_switch(
    catalog: &mut [ProviderConfig],
    config_path: Option<&Path>,
    selection: &ReplModelSelection,
) -> Result<(ProviderConfig, Vec<String>), String> {
    let provider = resolve_model_selection(catalog, selection)?;
    let mut notes = vec![format!(
        "switched to {}/{}",
        provider.name(),
        selection.model
    )];
    if !provider.models().contains(&selection.model) {
        if let Some(path) = config_path {
            match persist_model_addition(path, provider.name(), &selection.model) {
                Ok(true) => notes.push(format!(
                    "model {} saved to provider {}",
                    selection.model,
                    provider.name()
                )),
                Ok(false) => {}
                Err(err) => notes.push(format!("note: model not saved to config: {err}")),
            }
        }
        if let Some(entry) = catalog
            .iter_mut()
            .find(|entry| entry.name() == selection.provider_name)
        {
            *entry = entry.clone().with_model(&selection.model);
        }
    }
    Ok((provider, notes))
}

/// Append `model` to the saved provider's model list in the config file.
/// Returns `Ok(true)` when the model was new, `Ok(false)` when it was
/// already present (no write happens).
pub fn persist_model_addition(
    config_path: &Path,
    provider_name: &str,
    model: &str,
) -> Result<bool, String> {
    let store = ConfigStore::new(config_path);
    let config = store.load().map_err(|err| err.to_string())?;
    let Some(provider) = config.provider(provider_name) else {
        return Err(format!("unknown provider: {provider_name}"));
    };
    if provider.models().iter().any(|existing| existing == model) {
        return Ok(false);
    }
    let updated = provider.clone().with_model(model);
    store
        .save_provider(updated)
        .map_err(|err| err.to_string())?;
    Ok(true)
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
            _handle: tui_terminal::raw_mode().map_err(|err| ReplError::Io(err.into()))?,
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
