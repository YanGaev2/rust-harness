//! Setup TUI on the `harness-tui` library: the no-provider onboarding
//! screen. The pure state machines (`TuiApp`, the provider wizard) consume
//! `harness_tui::input` key events, rendering produces
//! `harness_tui::text::Line`s, and the terminal loop repaints a pinned
//! bottom panel via `harness_tui::core::Screen`.

use std::error::Error;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use harness_tui::components::select::select_lines;
use harness_tui::components::status::status_line;
use harness_tui::core::Screen;
use harness_tui::input::{Event, KeyCode, KeyEvent};
use harness_tui::terminal as tui_terminal;
use harness_tui::text::{Color, Line, Span, Style};

use crate::providers::BuiltinProvider;
use crate::repl::InputPump;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiAction {
    Continue,
    Command(TuiCommand),
    SaveProvider(TuiProviderDraft),
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiCommand {
    ProviderAdd,
    Providers,
    Help,
    Exit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiProviderDraft {
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiApp {
    command_name: String,
    config_path: PathBuf,
    workspace: PathBuf,
    input: String,
    status_message: String,
    dialog: Option<TuiDialog>,
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
            dialog: None,
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
        self.dialog.as_ref().map(TuiDialog::title)
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> TuiAction {
        if key.code != KeyCode::Esc {
            self.exit_armed = false;
        }

        if let Some(dialog) = &mut self.dialog {
            // Esc closes the dialog (as its own hints promise) instead of
            // killing the whole program.
            if key.code == KeyCode::Esc {
                self.dialog = None;
                self.status_message = "Provider setup closed.".to_string();
                return TuiAction::Continue;
            }
            let action = dialog.handle_key(key);
            if matches!(action, TuiAction::SaveProvider(_) | TuiAction::Exit) {
                self.dialog = None;
            }
            return action;
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
        if let Some(dialog) = &mut self.dialog {
            dialog.handle_paste(&pasted);
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
                self.dialog = Some(TuiDialog::ProviderWizard(ProviderWizard::new()));
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum TuiDialog {
    ProviderWizard(ProviderWizard),
}

impl TuiDialog {
    fn title(&self) -> &str {
        match self {
            Self::ProviderWizard(wizard) => wizard.title(),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> TuiAction {
        match self {
            Self::ProviderWizard(wizard) => wizard.handle_key(key),
        }
    }

    fn handle_paste(&mut self, text: &str) {
        match self {
            Self::ProviderWizard(wizard) => wizard.handle_paste(text),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderWizard {
    step: ProviderWizardStep,
    selected_provider: usize,
    base_url: String,
    api_key: String,
    model: String,
    base_url_edited: bool,
    api_key_edited: bool,
    model_edited: bool,
}

impl ProviderWizard {
    fn new() -> Self {
        Self {
            step: ProviderWizardStep::Provider,
            selected_provider: 0,
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            base_url_edited: false,
            api_key_edited: false,
            model_edited: false,
        }
    }

    fn title(&self) -> &str {
        "Provider setup"
    }

    fn handle_key(&mut self, key: KeyEvent) -> TuiAction {
        match self.step {
            ProviderWizardStep::Provider => self.handle_provider_key(key),
            ProviderWizardStep::BaseUrl => self.handle_text_key(key, TextTarget::BaseUrl),
            ProviderWizardStep::ApiKey => self.handle_text_key(key, TextTarget::ApiKey),
            ProviderWizardStep::Model => self.handle_model_key(key),
        }
    }

    fn handle_provider_key(&mut self, key: KeyEvent) -> TuiAction {
        // Esc never reaches the wizard: TuiApp::handle_key closes the
        // dialog before dispatching here.
        match key.code {
            KeyCode::Up => {
                if self.selected_provider == 0 {
                    self.selected_provider = BuiltinProvider::ALL.len() - 1;
                } else {
                    self.selected_provider -= 1;
                }
                TuiAction::Continue
            }
            KeyCode::Down => {
                self.selected_provider = (self.selected_provider + 1) % BuiltinProvider::ALL.len();
                TuiAction::Continue
            }
            KeyCode::Enter => {
                self.apply_provider_defaults();
                self.step = ProviderWizardStep::BaseUrl;
                TuiAction::Continue
            }
            _ => TuiAction::Continue,
        }
    }

    fn handle_text_key(&mut self, key: KeyEvent, target: TextTarget) -> TuiAction {
        match key.code {
            KeyCode::Enter => {
                if !self.active_text(target).trim().is_empty() {
                    self.step = match target {
                        TextTarget::BaseUrl => ProviderWizardStep::ApiKey,
                        TextTarget::ApiKey => ProviderWizardStep::Model,
                    };
                }
                TuiAction::Continue
            }
            KeyCode::Backspace => {
                self.mark_text_edited(target);
                self.active_text_mut(target).pop();
                TuiAction::Continue
            }
            KeyCode::Char(ch) if !key.mods.ctrl => {
                if !self.text_edited(target) {
                    self.active_text_mut(target).clear();
                    self.mark_text_edited(target);
                }
                self.active_text_mut(target).push(ch);
                TuiAction::Continue
            }
            _ => TuiAction::Continue,
        }
    }

    fn handle_model_key(&mut self, key: KeyEvent) -> TuiAction {
        match key.code {
            KeyCode::Backspace => {
                self.model_edited = true;
                self.model.pop();
                TuiAction::Continue
            }
            KeyCode::Char(ch) if !key.mods.ctrl => {
                if !self.model_edited {
                    self.model.clear();
                    self.model_edited = true;
                }
                self.model.push(ch);
                TuiAction::Continue
            }
            KeyCode::Enter => {
                if self.model.trim().is_empty() {
                    return TuiAction::Continue;
                }
                TuiAction::SaveProvider(TuiProviderDraft {
                    name: BuiltinProvider::ALL[self.selected_provider]
                        .name()
                        .to_string(),
                    base_url: self.base_url.trim().to_string(),
                    api_key: self.api_key.trim().to_string(),
                    model: self.model.trim().to_string(),
                })
            }
            _ => TuiAction::Continue,
        }
    }

    fn handle_paste(&mut self, text: &str) {
        match self.step {
            ProviderWizardStep::Provider => {}
            ProviderWizardStep::BaseUrl => self.paste_into_text(TextTarget::BaseUrl, text),
            ProviderWizardStep::ApiKey => self.paste_into_text(TextTarget::ApiKey, text),
            ProviderWizardStep::Model => {
                if !self.model_edited {
                    self.model.clear();
                    self.model_edited = true;
                }
                self.model.push_str(text);
            }
        }
    }

    fn paste_into_text(&mut self, target: TextTarget, text: &str) {
        if !self.text_edited(target) {
            self.active_text_mut(target).clear();
            self.mark_text_edited(target);
        }
        self.active_text_mut(target).push_str(text);
    }

    fn apply_provider_defaults(&mut self) {
        // Canonical profile data only: families the profiles deliberately
        // leave without a base_url get an empty field the user must fill.
        let profile = BuiltinProvider::ALL[self.selected_provider].profile();
        self.base_url = profile.base_url.unwrap_or_default().to_string();
        self.model = profile
            .model_hints
            .first()
            .copied()
            .unwrap_or_default()
            .to_string();
        self.base_url_edited = false;
        self.api_key_edited = false;
        self.model_edited = false;
    }

    fn active_text(&self, target: TextTarget) -> &str {
        match target {
            TextTarget::BaseUrl => &self.base_url,
            TextTarget::ApiKey => &self.api_key,
        }
    }

    fn active_text_mut(&mut self, target: TextTarget) -> &mut String {
        match target {
            TextTarget::BaseUrl => &mut self.base_url,
            TextTarget::ApiKey => &mut self.api_key,
        }
    }

    fn text_edited(&self, target: TextTarget) -> bool {
        match target {
            TextTarget::BaseUrl => self.base_url_edited,
            TextTarget::ApiKey => self.api_key_edited,
        }
    }

    fn mark_text_edited(&mut self, target: TextTarget) {
        match target {
            TextTarget::BaseUrl => self.base_url_edited = true,
            TextTarget::ApiKey => self.api_key_edited = true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderWizardStep {
    Provider,
    BaseUrl,
    ApiKey,
    Model,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextTarget {
    BaseUrl,
    ApiKey,
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

    loop {
        let panel = setup_lines(&app, screen.width() as usize);
        draw_panel(&mut screen, panel)?;
        let events = pump
            .poll(Duration::from_millis(400))
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
                action => {
                    let _ = screen.release();
                    return Ok(action);
                }
            }
        }
    }
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
    match &app.dialog {
        Some(TuiDialog::ProviderWizard(wizard)) => lines.extend(wizard_lines(wizard)),
        None => lines.extend(command_lines()),
    }
    lines.push(Line::raw(""));
    lines.push(input_line(&app.input, app.dialog.is_none()));
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

fn wizard_lines(wizard: &ProviderWizard) -> Vec<Line> {
    let mut lines = vec![
        Line::styled(wizard.title(), Style::default().bold()),
        Line::raw(""),
    ];
    match wizard.step {
        ProviderWizardStep::Provider => {
            lines.push(Line::styled("Select provider", Style::default().bold()));
            lines.push(Line::raw(""));
            let names: Vec<&str> = BuiltinProvider::ALL
                .iter()
                .map(|provider| provider.name())
                .collect();
            lines.extend(select_lines(&names, wizard.selected_provider));
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "Up/Down move  Enter accept  Esc close",
                Style::default().dim(),
            ));
        }
        ProviderWizardStep::BaseUrl => {
            lines.push(Line::styled("Base URL", Style::default().bold()));
            lines.push(Line::raw(""));
            lines.push(Line::raw(wizard.base_url.clone()));
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "Enter accept  Esc close",
                Style::default().dim(),
            ));
        }
        ProviderWizardStep::ApiKey => {
            lines.push(Line::styled("API key", Style::default().bold()));
            lines.push(Line::raw(""));
            lines.push(Line::raw(mask_secret(&wizard.api_key)));
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "Enter accept  Esc close",
                Style::default().dim(),
            ));
        }
        ProviderWizardStep::Model => {
            lines.push(Line::styled("Model", Style::default().bold()));
            lines.push(Line::raw(""));
            lines.push(Line::raw(wizard.model.clone()));
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "Enter save  Esc close",
                Style::default().dim(),
            ));
        }
    }
    lines
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

fn mask_secret(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    "*".repeat(value.chars().count().min(12))
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
