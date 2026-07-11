//! Setup TUI on the `harness-tui` library: the no-provider onboarding
//! screen. The pure state machines (`TuiApp`, `SetupTuiApp`, the provider
//! wizard) consume `harness_tui::input` key events, rendering produces
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
use harness_tui::terminal::{self as tui_terminal, TerminalError};
use harness_tui::text::{Color, Line, Span, Style};

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
        if let Some(dialog) = &mut self.dialog {
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
                self.status_message = "Setup cancelled.".to_string();
                TuiAction::Exit
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
        match key.code {
            KeyCode::Esc => TuiAction::Exit,
            KeyCode::Up => {
                if self.selected_provider == 0 {
                    self.selected_provider = BUILTIN_PROVIDER_NAMES.len() - 1;
                } else {
                    self.selected_provider -= 1;
                }
                TuiAction::Continue
            }
            KeyCode::Down => {
                self.selected_provider =
                    (self.selected_provider + 1) % BUILTIN_PROVIDER_NAMES.len();
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
            KeyCode::Esc => TuiAction::Exit,
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
            KeyCode::Esc => TuiAction::Exit,
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
                    name: BUILTIN_PROVIDER_NAMES[self.selected_provider].to_string(),
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
        let defaults = provider_defaults(BUILTIN_PROVIDER_NAMES[self.selected_provider]);
        self.base_url = defaults.base_url.to_string();
        self.model = defaults.model.to_string();
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

const BUILTIN_PROVIDER_NAMES: &[&str] = &["codex", "xiaomi", "glm", "kimi", "claude", "deepseek"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProviderDefaults {
    base_url: &'static str,
    model: &'static str,
}

fn provider_defaults(name: &str) -> ProviderDefaults {
    match name {
        "codex" => ProviderDefaults {
            base_url: "https://api.openai.com/v1",
            model: "gpt-5-codex",
        },
        "xiaomi" => ProviderDefaults {
            base_url: "https://api.xiaomi.com/v1",
            model: "xiaomi-lm",
        },
        "glm" => ProviderDefaults {
            base_url: "https://open.bigmodel.cn/api/paas/v4",
            model: "glm-4.5",
        },
        "kimi" => ProviderDefaults {
            base_url: "https://api.moonshot.ai/v1",
            model: "kimi-k2",
        },
        "claude" => ProviderDefaults {
            base_url: "https://api.anthropic.com/v1",
            model: "claude-sonnet-4.5",
        },
        "deepseek" => ProviderDefaults {
            base_url: "https://api.deepseek.com/v1",
            model: "deepseek-v4-pro",
        },
        _ => ProviderDefaults {
            base_url: "",
            model: "",
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupTuiAction {
    Continue,
    ProviderAdd,
    Exit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupTuiApp {
    command_name: String,
    config_path: PathBuf,
    workspace: PathBuf,
    input: String,
    status_message: String,
}

impl SetupTuiApp {
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
        }
    }

    pub fn input(&self) -> &str {
        &self.input
    }

    pub fn status_message(&self) -> &str {
        &self.status_message
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> SetupTuiAction {
        match key.code {
            KeyCode::Char('c') if key.mods.ctrl => {
                self.status_message = "Setup cancelled.".to_string();
                SetupTuiAction::Exit
            }
            KeyCode::Esc => {
                self.status_message = "Setup cancelled.".to_string();
                SetupTuiAction::Exit
            }
            KeyCode::Enter => self.submit_command(),
            KeyCode::Backspace => {
                self.input.pop();
                SetupTuiAction::Continue
            }
            KeyCode::Char(ch) if !key.mods.ctrl => {
                self.input.push(ch);
                SetupTuiAction::Continue
            }
            _ => SetupTuiAction::Continue,
        }
    }

    pub fn handle_paste(&mut self, text: &str) -> SetupTuiAction {
        self.input.push_str(&sanitize_pasted_line(text));
        SetupTuiAction::Continue
    }

    fn submit_command(&mut self) -> SetupTuiAction {
        let command = self.input.trim().to_string();
        self.input.clear();

        match command.as_str() {
            "" => SetupTuiAction::Continue,
            "/provider add" => {
                self.status_message = "Provider setup starting...".to_string();
                SetupTuiAction::ProviderAdd
            }
            "/providers" | "/provider subscriptions" => {
                self.status_message =
                    "Built-in providers: codex, xiaomi, glm, kimi, claude, deepseek.".to_string();
                SetupTuiAction::Continue
            }
            "/help" => {
                self.status_message =
                    "Commands: /provider add, /providers, /help, /exit.".to_string();
                SetupTuiAction::Continue
            }
            "/exit" | "/quit" => {
                self.status_message = "Setup cancelled.".to_string();
                SetupTuiAction::Exit
            }
            other => {
                self.status_message =
                    format!("Unknown command: {other}. Type /help for available commands.");
                SetupTuiAction::Continue
            }
        }
    }
}

pub fn run_setup_tui(
    command_name: impl Into<String>,
    config_path: impl Into<PathBuf>,
    workspace: impl Into<PathBuf>,
) -> Result<SetupTuiAction, TuiError> {
    tui_terminal::install_panic_restore();
    let mut screen = Screen::stdout().map_err(terminal_error)?;
    let mut app = SetupTuiApp::new(command_name, config_path, workspace);
    let mut pump = InputPump::start();

    loop {
        let panel = setup_tui_lines(&app, screen.width() as usize);
        draw_panel(&mut screen, panel)?;
        let events = pump
            .poll(Duration::from_millis(400))
            .map_err(TuiError::Io)?;
        check_resize(&mut screen)?;
        for event in events {
            let action = match event {
                Event::Key(key) => app.handle_key(key),
                Event::Paste(text) => app.handle_paste(&text),
                _ => SetupTuiAction::Continue,
            };
            if action != SetupTuiAction::Continue {
                let _ = screen.release();
                return Ok(action);
            }
        }
    }
}

pub fn run_tui(
    command_name: impl Into<String>,
    config_path: impl Into<PathBuf>,
    workspace: impl Into<PathBuf>,
) -> Result<TuiAction, TuiError> {
    tui_terminal::install_panic_restore();
    let mut screen = Screen::stdout().map_err(terminal_error)?;
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

fn terminal_error(err: TerminalError) -> TuiError {
    TuiError::Io(io::Error::other(err.to_string()))
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

/// Full panel for the simpler `SetupTuiApp` screen (no dialog).
pub fn setup_tui_lines(app: &SetupTuiApp, width: usize) -> Vec<Line> {
    let mut lines = header_lines(&app.command_name);
    lines.extend(overview_lines(
        &app.workspace,
        &app.config_path,
        &app.status_message,
    ));
    lines.push(Line::raw(""));
    lines.extend(command_lines());
    lines.push(Line::raw(""));
    lines.push(input_line(&app.input, true));
    lines.push(footer_line(width));
    lines
}

fn header_lines(command_name: &str) -> Vec<Line> {
    vec![
        Line {
            spans: vec![
                Span::styled(format!(" {command_name} "), title_style()),
                Span::raw(" "),
                Span::styled("no provider configured", warn_style()),
            ],
        },
        Line::raw(""),
    ]
}

fn overview_lines(workspace: &Path, config_path: &Path, status_message: &str) -> Vec<Line> {
    vec![
        Line {
            spans: vec![
                Span::styled("workspace ", label_style()),
                Span::raw(display_path(workspace)),
            ],
        },
        Line {
            spans: vec![
                Span::styled("config    ", label_style()),
                Span::raw(display_path(config_path)),
            ],
        },
        Line::raw(""),
        styled_line("No provider is configured yet.", emphasis_style()),
        Line::raw("Configure one here, then this launch continues into the REPL."),
        Line::raw(""),
        styled_line(status_message, status_style()),
    ]
}

fn command_lines() -> Vec<Line> {
    command_registry()
        .iter()
        .map(|entry| Line {
            spans: vec![
                Span::styled(format!("{:<15}", entry.names[0]), command_style()),
                Span::raw(command_description(entry.command)),
            ],
        })
        .collect()
}

fn wizard_lines(wizard: &ProviderWizard) -> Vec<Line> {
    let mut lines = vec![styled_line(wizard.title(), emphasis_style()), Line::raw("")];
    match wizard.step {
        ProviderWizardStep::Provider => {
            lines.push(styled_line("Select provider", emphasis_style()));
            lines.push(Line::raw(""));
            lines.extend(select_lines(
                BUILTIN_PROVIDER_NAMES,
                wizard.selected_provider,
            ));
            lines.push(Line::raw(""));
            lines.push(styled_line(
                "Up/Down move  Enter accept  Esc close",
                hint_style(),
            ));
        }
        ProviderWizardStep::BaseUrl => {
            lines.push(styled_line("Base URL", emphasis_style()));
            lines.push(Line::raw(""));
            lines.push(Line::raw(wizard.base_url.clone()));
            lines.push(Line::raw(""));
            lines.push(styled_line("Enter accept  Esc close", hint_style()));
        }
        ProviderWizardStep::ApiKey => {
            lines.push(styled_line("API key", emphasis_style()));
            lines.push(Line::raw(""));
            lines.push(Line::raw(mask_secret(&wizard.api_key)));
            lines.push(Line::raw(""));
            lines.push(styled_line("Enter accept  Esc close", hint_style()));
        }
        ProviderWizardStep::Model => {
            lines.push(styled_line("Model", emphasis_style()));
            lines.push(Line::raw(""));
            lines.push(Line::raw(wizard.model.clone()));
            lines.push(Line::raw(""));
            lines.push(styled_line("Enter save  Esc close", hint_style()));
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
        styled_line("Esc/Ctrl+C exit", hint_style()),
    )
}

fn styled_line(text: impl Into<String>, style: Style) -> Line {
    Line {
        spans: vec![Span::styled(text, style)],
    }
}

fn title_style() -> Style {
    Style {
        fg: Color::Ansi(0),
        bg: Color::Ansi(6),
        bold: true,
        ..Style::default()
    }
}

fn warn_style() -> Style {
    Style {
        fg: Color::Ansi(3),
        bold: true,
        ..Style::default()
    }
}

fn label_style() -> Style {
    Style {
        fg: Color::Ansi(7),
        bold: true,
        ..Style::default()
    }
}

fn emphasis_style() -> Style {
    Style {
        bold: true,
        ..Style::default()
    }
}

fn command_style() -> Style {
    Style {
        fg: Color::Ansi(2),
        bold: true,
        ..Style::default()
    }
}

fn status_style() -> Style {
    Style {
        fg: Color::Ansi(6),
        ..Style::default()
    }
}

fn hint_style() -> Style {
    Style {
        dim: true,
        ..Style::default()
    }
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
