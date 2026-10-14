//! Setup TUI on the `harness-tui` library: the no-provider onboarding
//! screen. The pure state machines (`TuiApp`, the provider wizard) consume
//! `harness_tui::input` key events, rendering produces
//! `harness_tui::text::Line`s, and the terminal loop repaints a pinned
//! bottom panel via `harness_tui::core::Screen`.

use std::error::Error;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use harness_tui::components::status::status_line;
use harness_tui::core::Screen;
use harness_tui::input::{Event, KeyCode, KeyEvent};
use harness_tui::terminal as tui_terminal;
use harness_tui::text::{Color, Line, Span, Style};
use harness_tui::theme::Theme;

use crate::model_client::OpenAiCompatibleModelClient;
use crate::provider_wizard::{ProviderWizard, WizardAction};
use crate::providers::{ModelDiscovery, ProviderConfig};
use crate::repl::InputPump;

#[derive(Debug, Clone, PartialEq)]
pub enum TuiAction {
    Continue,
    Command(TuiCommand),
    /// Fetch this provider's model list, then call `TuiApp::models_loaded`.
    FetchModels(ProviderConfig),
    /// Save the provider and start the chat with `model`.
    SaveProvider {
        provider: ProviderConfig,
        model: String,
    },
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiCommand {
    ProviderAdd,
    Providers,
    Help,
    Exit,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TuiApp {
    command_name: String,
    config_path: PathBuf,
    workspace: PathBuf,
    input: String,
    status_message: String,
    wizard: Option<ProviderWizard>,
    /// True after a first Esc press at the top level: the next Esc
    /// exits, anything else disarms.
    exit_armed: bool,
}

impl TuiApp {
    pub fn new(
        command_name: impl Into<String>,
        config_path: impl Into<PathBuf>,
        workspace: impl Into<PathBuf>,
    ) -> Self {
        Self {
            command_name: command_name.into(),
            config_path: config_path.into(),
            workspace: workspace.into(),
            input: String::new(),
            status_message: "Type /provider add to configure a provider inside this interface."
                .to_string(),
            wizard: None,
            exit_armed: false,
        }
    }

    pub fn input(&self) -> &str {
        &self.input
    }

    pub fn status_message(&self) -> &str {
        &self.status_message
    }

    pub fn dialog_title(&self) -> Option<&str> {
        self.wizard.as_ref().map(ProviderWizard::title)
    }

    /// Hand the model list result to the open wizard (dropped when the
    /// wizard was closed meanwhile).
    pub fn models_loaded(&mut self, result: Result<(ProviderConfig, ModelDiscovery), String>) {
        if let Some(wizard) = &mut self.wizard {
            wizard.models_loaded(result);
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> TuiAction {
        if key.code != KeyCode::Esc {
            self.exit_armed = false;
        }

        if let Some(wizard) = &mut self.wizard {
            // Esc closes the dialog (as its own hints promise) instead of
            // killing the whole program.
            if key.code == KeyCode::Esc {
                self.wizard = None;
                self.status_message = "Provider setup closed.".to_string();
                return TuiAction::Continue;
            }
            return match wizard.handle_key(key) {
                WizardAction::Continue => TuiAction::Continue,
                WizardAction::Fetch(provider) => TuiAction::FetchModels(provider),
                WizardAction::Save { provider, model } => {
                    self.wizard = None;
                    TuiAction::SaveProvider { provider, model }
                }
            };
        }

        match key.code {
            KeyCode::Char('c') if key.mods.ctrl => {
                self.status_message = "Setup cancelled.".to_string();
                TuiAction::Exit
            }
            KeyCode::Esc => {
                // Two-step exit: a stray Esc must not end the setup.
                if self.exit_armed {
                    self.status_message = "Setup cancelled.".to_string();
                    return TuiAction::Exit;
                }
                self.exit_armed = true;
                self.status_message = "Press Esc again to exit.".to_string();
                TuiAction::Continue
            }
            KeyCode::Enter => self.submit_command(),
            KeyCode::Backspace => {
                self.input.pop();
                TuiAction::Continue
            }
            KeyCode::Char(ch) if !key.mods.ctrl => {
                self.input.push(ch);
                TuiAction::Continue
            }
            _ => TuiAction::Continue,
        }
    }

    /// Insert a bracketed-paste block. Pasting routes into the open dialog's
    /// active field (so an API key can be pasted straight into the wizard) or
    /// into the command line. Newlines are stripped because these are
    /// single-line fields, and the paste never submits.
    pub fn handle_paste(&mut self, text: &str) -> TuiAction {
        let pasted = sanitize_pasted_line(text);
        if pasted.is_empty() {
            return TuiAction::Continue;
        }
        if let Some(wizard) = &mut self.wizard {
            wizard.handle_paste(&pasted);
        } else {
            self.input.push_str(&pasted);
        }
        TuiAction::Continue
    }

    fn submit_command(&mut self) -> TuiAction {
        let command = self.input.trim().to_string();
        self.input.clear();

        let Some(command) = command_registry()
            .iter()
            .find(|entry| entry.matches(&command))
            .map(|entry| entry.command)
        else {
            if !command.is_empty() {
                self.status_message =
                    format!("Unknown command: {command}. Type /help for available commands.");
            }
            return TuiAction::Continue;
        };

        match command {
            TuiCommand::ProviderAdd => {
                self.status_message = "Provider setup opened.".to_string();
                self.wizard = Some(ProviderWizard::new());
                TuiAction::Command(TuiCommand::ProviderAdd)
            }
            TuiCommand::Providers => {
                self.status_message =
                    "Built-in providers: codex, xiaomi, glm, kimi, claude, deepseek.".to_string();
                TuiAction::Command(TuiCommand::Providers)
            }
            TuiCommand::Help => {
                self.status_message =
                    "Commands: /provider add, /providers, /help, /exit.".to_string();
                TuiAction::Command(TuiCommand::Help)
            }
            TuiCommand::Exit => {
                self.status_message = "Setup cancelled.".to_string();
                TuiAction::Exit
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CommandEntry {
    command: TuiCommand,
    names: &'static [&'static str],
}

impl CommandEntry {
    fn matches(&self, input: &str) -> bool {
        self.names.contains(&input)
    }
}

fn command_registry() -> &'static [CommandEntry] {
    &[
        CommandEntry {
            command: TuiCommand::ProviderAdd,
            names: &["/provider add", "/connect"],
        },
        CommandEntry {
            command: TuiCommand::Providers,
            names: &["/providers", "/provider subscriptions"],
        },
        CommandEntry {
            command: TuiCommand::Help,
            names: &["/help"],
        },
        CommandEntry {
            command: TuiCommand::Exit,
            names: &["/exit", "/quit"],
        },
    ]
}

pub fn run_tui(
    command_name: impl Into<String>,
    config_path: impl Into<PathBuf>,
    workspace: impl Into<PathBuf>,
) -> Result<TuiAction, TuiError> {
    tui_terminal::install_panic_restore();
    let mut screen = Screen::stdout().map_err(|err| TuiError::Io(err.into()))?;
    screen.clear_screen().map_err(TuiError::Io)?;
    let mut app = TuiApp::new(command_name, config_path, workspace);
    let mut pump = InputPump::start();
    let mut fetch: Option<mpsc::Receiver<FetchResult>> = None;

    loop {
        if let Some(result) = fetch.as_ref().and_then(|receiver| receiver.try_recv().ok()) {
            app.models_loaded(result);
            fetch = None;
        }
        let panel = setup_lines(&app, screen.width() as usize);
        draw_panel(&mut screen, panel)?;
        // Poll faster while the model list loads so its answer shows at once.
        let poll = if fetch.is_some() { 100 } else { 400 };
        let events = pump
            .poll(Duration::from_millis(poll))
            .map_err(TuiError::Io)?;
        check_resize(&mut screen)?;
        for event in events {
            let action = match event {
                Event::Key(key) => app.handle_key(key),
                Event::Paste(text) => app.handle_paste(&text),
                _ => TuiAction::Continue,
            };
            match action {
                TuiAction::Continue | TuiAction::Command(_) => {}
                TuiAction::FetchModels(provider) => fetch = Some(spawn_model_fetch(provider)),
                action => {
                    let _ = screen.release();
                    return Ok(action);
                }
            }
        }
    }
}

/// What a model list fetch hands back to the wizard.
pub type FetchResult = Result<(ProviderConfig, ModelDiscovery), String>;

/// How long the wizard waits for `/models`.
const MODEL_FETCH_TIMEOUT: Duration = Duration::from_secs(20);

/// Run `discover` on a worker thread so the screen stays responsive.
pub fn spawn_model_fetch(provider: ProviderConfig) -> mpsc::Receiver<FetchResult> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = OpenAiCompatibleModelClient::new(MODEL_FETCH_TIMEOUT)
            .discover(provider)
            .map_err(|err| err.to_string());
        let _ = sender.send(result);
    });
    receiver
}

/// Repaint the pinned panel, capped to the screen height so the panel
/// can never scroll its own top row out of the buffer.
fn draw_panel(screen: &mut Screen, lines: Vec<Line>) -> Result<(), TuiError> {
    let max_panel = (screen.height() as usize).saturating_sub(1).max(1);
    let mut panel = lines;
    if panel.len() > max_panel {
        panel = panel.split_off(panel.len() - max_panel);
    }
    screen.render_panel(panel).map_err(TuiError::Io)
}

/// The terminal delivers no resize signal we listen for — the idle loop
/// polls the size once per tick, which is cheap and cross-platform.
fn check_resize(screen: &mut Screen) -> Result<(), TuiError> {
    if let Ok((width, height)) = tui_terminal::size()
        && (width != screen.width() || height != screen.height())
    {
        screen.resize(width, height).map_err(TuiError::Io)?;
    }
    Ok(())
}

/// Full panel for the `TuiApp` screen. When the provider wizard is open
/// its step content replaces the command list.
pub fn setup_lines(app: &TuiApp, width: usize) -> Vec<Line> {
    let mut lines = header_lines(&app.command_name);
    lines.extend(overview_lines(
        &app.workspace,
        &app.config_path,
        &app.status_message,
    ));
    lines.push(Line::raw(""));
    match &app.wizard {
        Some(wizard) => lines.extend(wizard.lines(&Theme::dark())),
        None => lines.extend(command_lines()),
    }
    lines.push(Line::raw(""));
    lines.push(input_line(&app.input, app.wizard.is_none()));
    lines.push(footer_line(width));
    lines
}

fn header_lines(command_name: &str) -> Vec<Line> {
    vec![
        Line {
            spans: vec![
                Span::styled(
                    format!(" {command_name} "),
                    Style::fg(Color::Ansi(0)).bg(Color::Ansi(6)).bold(),
                ),
                Span::raw(" "),
                Span::styled("no provider configured", Style::fg(Color::Ansi(3)).bold()),
            ],
        },
        Line::raw(""),
    ]
}

fn overview_lines(workspace: &Path, config_path: &Path, status_message: &str) -> Vec<Line> {
    vec![
        Line {
            spans: vec![
                Span::styled("workspace ", Style::fg(Color::Ansi(7)).bold()),
                Span::raw(display_path(workspace)),
            ],
        },
        Line {
            spans: vec![
                Span::styled("config    ", Style::fg(Color::Ansi(7)).bold()),
                Span::raw(display_path(config_path)),
            ],
        },
        Line::raw(""),
        Line::styled("No provider is configured yet.", Style::default().bold()),
        Line::raw("Configure one here, then this launch continues into the REPL."),
        Line::raw(""),
        Line::styled(status_message, Style::fg(Color::Ansi(6))),
    ]
}

fn command_lines() -> Vec<Line> {
    command_registry()
        .iter()
        .map(|entry| Line {
            spans: vec![
                Span::styled(
                    format!("{:<15}", entry.names[0]),
                    Style::fg(Color::Ansi(2)).bold(),
                ),
                Span::raw(command_description(entry.command)),
            ],
        })
        .collect()
}

/// The command prompt row. The caret is a reverse-styled cell at the end
/// of the input; it is hidden while a dialog owns the keyboard.
fn input_line(input: &str, show_caret: bool) -> Line {
    let mut spans = vec![Span::raw(format!("[no provider] > {input}"))];
    if show_caret {
        spans.push(Span::styled(
            " ",
            Style {
                reverse: true,
                ..Style::default()
            },
        ));
    }
    Line { spans }
}

fn footer_line(width: usize) -> Line {
    status_line(
        width,
        Line::default(),
        Line::styled("Esc Esc / Ctrl+C exit", Style::default().dim()),
    )
}

fn command_description(command: TuiCommand) -> &'static str {
    match command {
        TuiCommand::ProviderAdd => "configure a provider",
        TuiCommand::Providers => "list built-in profiles",
        TuiCommand::Help => "show commands",
        TuiCommand::Exit => "quit",
    }
}

/// Pasted clipboard content for a single-line field: drop carriage returns and
/// newlines (clipboards often append a trailing newline) so the paste cannot
/// submit or split the value.
fn sanitize_pasted_line(text: &str) -> String {
    text.chars()
        .filter(|ch| *ch != '\n' && *ch != '\r')
        .collect()
}

fn display_path(path: &Path) -> String {
    path.display().to_string()
}

#[derive(Debug)]
pub enum TuiError {
    Io(io::Error),
}

impl fmt::Display for TuiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl Error for TuiError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
        }
    }
}
