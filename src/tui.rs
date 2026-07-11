use std::error::Error;
use std::fmt;
use std::io::{self, Stdout};
use std::path::{Path, PathBuf};

use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

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
        if key.kind != KeyEventKind::Press {
            return TuiAction::Continue;
        }

        if let Some(dialog) = &mut self.dialog {
            let action = dialog.handle_key(key);
            if matches!(action, TuiAction::SaveProvider(_) | TuiAction::Exit) {
                self.dialog = None;
            }
            return action;
        }

        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
                self.status_message = "Setup cancelled.".to_string();
                TuiAction::Exit
            }
            (KeyCode::Esc, _) => {
                self.status_message = "Setup cancelled.".to_string();
                TuiAction::Exit
            }
            (KeyCode::Enter, _) => self.submit_command(),
            (KeyCode::Backspace, _) => {
                self.input.pop();
                TuiAction::Continue
            }
            (KeyCode::Char(ch), modifiers) if !modifiers.contains(KeyModifiers::CONTROL) => {
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
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
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
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
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
        if key.kind != KeyEventKind::Press {
            return SetupTuiAction::Continue;
        }

        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
                self.status_message = "Setup cancelled.".to_string();
                SetupTuiAction::Exit
            }
            (KeyCode::Esc, _) => {
                self.status_message = "Setup cancelled.".to_string();
                SetupTuiAction::Exit
            }
            (KeyCode::Enter, _) => self.submit_command(),
            (KeyCode::Backspace, _) => {
                self.input.pop();
                SetupTuiAction::Continue
            }
            (KeyCode::Char(ch), modifiers) if !modifiers.contains(KeyModifiers::CONTROL) => {
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
    let mut terminal = SetupTerminal::enter()?;
    let mut app = SetupTuiApp::new(command_name, config_path, workspace);

    loop {
        terminal.draw(&app)?;
        match event::read().map_err(TuiError::Io)? {
            Event::Key(key) => {
                let action = app.handle_key(key);
                if action != SetupTuiAction::Continue {
                    terminal.draw(&app)?;
                    return Ok(action);
                }
            }
            Event::Paste(text) => {
                app.handle_paste(&text);
            }
            _ => {}
        }
    }
}

pub fn run_tui(
    command_name: impl Into<String>,
    config_path: impl Into<PathBuf>,
    workspace: impl Into<PathBuf>,
) -> Result<TuiAction, TuiError> {
    let mut terminal = SetupTerminal::enter()?;
    let mut app = TuiApp::new(command_name, config_path, workspace);

    loop {
        terminal.draw_tui(&app)?;
        match event::read().map_err(TuiError::Io)? {
            Event::Key(key) => match app.handle_key(key) {
                TuiAction::Continue | TuiAction::Command(_) => {}
                action => {
                    terminal.draw_tui(&app)?;
                    return Ok(action);
                }
            },
            Event::Paste(text) => {
                app.handle_paste(&text);
            }
            _ => {}
        }
    }
}

pub fn render_tui(frame: &mut Frame<'_>, app: &TuiApp) {
    let area = frame.area();
    let root = Block::default().style(Style::default().bg(Color::Black));
    frame.render_widget(root, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(5),
            Constraint::Length(1),
        ])
        .split(area);

    render_tui_header(frame, chunks[0], app);
    render_tui_body(frame, chunks[1], app);
    render_tui_input(frame, chunks[2], app);
    render_footer(frame, chunks[3]);

    if let Some(dialog) = &app.dialog {
        render_dialog(frame, area, dialog);
    }
}

pub fn render_setup_tui(frame: &mut Frame<'_>, app: &SetupTuiApp) {
    let area = frame.area();
    let root = Block::default().style(Style::default().bg(Color::Black));
    frame.render_widget(root, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(5),
            Constraint::Length(1),
        ])
        .split(area);

    render_header(frame, chunks[0], app);
    render_body(frame, chunks[1], app);
    render_input(frame, chunks[2], app);
    render_footer(frame, chunks[3]);
}

fn render_tui_header(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let title = Line::from(vec![
        Span::styled(
            format!(" {} ", app.command_name),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            "no provider configured",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    let header = Paragraph::new(title)
        .block(Block::default().borders(Borders::BOTTOM))
        .alignment(Alignment::Left);
    frame.render_widget(header, area);
}

fn render_header(frame: &mut Frame<'_>, area: Rect, app: &SetupTuiApp) {
    let title = Line::from(vec![
        Span::styled(
            format!(" {} ", app.command_name),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            "no provider configured",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    let header = Paragraph::new(title)
        .block(Block::default().borders(Borders::BOTTOM))
        .alignment(Alignment::Left);
    frame.render_widget(header, area);
}

fn render_tui_body(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(area);

    let overview = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("workspace ", label_style()),
            Span::raw(display_path(&app.workspace)),
        ]),
        Line::from(vec![
            Span::styled("config    ", label_style()),
            Span::raw(display_path(&app.config_path)),
        ]),
        Line::raw(""),
        Line::styled("No provider is configured yet.", emphasis_style()),
        Line::raw("Configure one here, then this launch continues into the REPL."),
        Line::raw(""),
        Line::styled(app.status_message.clone(), Style::default().fg(Color::Cyan)),
    ])
    .block(Block::default().title(" Session ").borders(Borders::ALL))
    .wrap(Wrap { trim: false });
    frame.render_widget(overview, columns[0]);

    let commands = Paragraph::new(
        command_registry()
            .iter()
            .map(|entry| {
                Line::from(vec![
                    Span::styled(entry.names[0], command_style()),
                    Span::raw(format!("   {}", command_description(entry.command))),
                ])
            })
            .collect::<Vec<_>>(),
    )
    .block(Block::default().title(" Commands ").borders(Borders::ALL))
    .wrap(Wrap { trim: false });
    frame.render_widget(commands, columns[1]);
}

fn render_body(frame: &mut Frame<'_>, area: Rect, app: &SetupTuiApp) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(area);

    let overview = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("workspace ", label_style()),
            Span::raw(display_path(&app.workspace)),
        ]),
        Line::from(vec![
            Span::styled("config    ", label_style()),
            Span::raw(display_path(&app.config_path)),
        ]),
        Line::raw(""),
        Line::styled("No provider is configured yet.", emphasis_style()),
        Line::raw("Configure one here, then this launch continues into the REPL."),
        Line::raw(""),
        Line::styled(app.status_message.clone(), Style::default().fg(Color::Cyan)),
    ])
    .block(Block::default().title(" Session ").borders(Borders::ALL))
    .wrap(Wrap { trim: false });
    frame.render_widget(overview, columns[0]);

    let commands = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("/provider add", command_style()),
            Span::raw("   configure a provider"),
        ]),
        Line::from(vec![
            Span::styled("/providers", command_style()),
            Span::raw("      list built-in profiles"),
        ]),
        Line::from(vec![
            Span::styled("/help", command_style()),
            Span::raw("           show commands"),
        ]),
        Line::from(vec![
            Span::styled("/exit", command_style()),
            Span::raw("           quit"),
        ]),
    ])
    .block(Block::default().title(" Commands ").borders(Borders::ALL))
    .wrap(Wrap { trim: false });
    frame.render_widget(commands, columns[1]);
}

fn render_tui_input(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let prompt = format!("[no provider] > {}", app.input);
    let input = Paragraph::new(prompt)
        .block(Block::default().title(" Input ").borders(Borders::ALL))
        .style(Style::default().fg(Color::White));
    frame.render_widget(input, area);

    if app.dialog.is_none() {
        let prompt_width = "[no provider] > ".chars().count() as u16;
        let input_width = app.input.chars().count() as u16;
        let cursor_x = area
            .x
            .saturating_add(1)
            .saturating_add(prompt_width)
            .saturating_add(input_width)
            .min(area.x.saturating_add(area.width.saturating_sub(2)));
        let cursor_y = area.y.saturating_add(1);
        frame.set_cursor_position(Position::new(cursor_x, cursor_y));
    }
}

fn render_input(frame: &mut Frame<'_>, area: Rect, app: &SetupTuiApp) {
    let prompt = format!("[no provider] > {}", app.input);
    let input = Paragraph::new(prompt)
        .block(Block::default().title(" Input ").borders(Borders::ALL))
        .style(Style::default().fg(Color::White));
    frame.render_widget(input, area);

    let prompt_width = "[no provider] > ".chars().count() as u16;
    let input_width = app.input.chars().count() as u16;
    let cursor_x = area
        .x
        .saturating_add(1)
        .saturating_add(prompt_width)
        .saturating_add(input_width)
        .min(area.x.saturating_add(area.width.saturating_sub(2)));
    let cursor_y = area.y.saturating_add(1);
    frame.set_cursor_position(Position::new(cursor_x, cursor_y));
}

fn render_footer(frame: &mut Frame<'_>, area: Rect) {
    let footer = Paragraph::new("Esc/Ctrl+C exit")
        .alignment(Alignment::Right)
        .style(Style::default().fg(Color::DarkGray));
    frame.render_widget(footer, area);
}

fn render_dialog(frame: &mut Frame<'_>, area: Rect, dialog: &TuiDialog) {
    let dialog_area = centered_rect(area, 72, 16);
    frame.render_widget(Clear, dialog_area);
    match dialog {
        TuiDialog::ProviderWizard(wizard) => render_provider_wizard(frame, dialog_area, wizard),
    }
}

fn render_provider_wizard(frame: &mut Frame<'_>, area: Rect, wizard: &ProviderWizard) {
    let lines = match wizard.step {
        ProviderWizardStep::Provider => {
            let mut lines = vec![
                Line::styled("Select provider", emphasis_style()),
                Line::raw(""),
            ];
            for (index, name) in BUILTIN_PROVIDER_NAMES.iter().enumerate() {
                let marker = if index == wizard.selected_provider {
                    "> "
                } else {
                    "  "
                };
                let style = if index == wizard.selected_provider {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                lines.push(Line::styled(format!("{marker}{name}"), style));
            }
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "Up/Down move  Enter accept  Esc close",
                Style::default().fg(Color::DarkGray),
            ));
            lines
        }
        ProviderWizardStep::BaseUrl => vec![
            Line::styled("Base URL", emphasis_style()),
            Line::raw(""),
            Line::raw(wizard.base_url.clone()),
            Line::raw(""),
            Line::styled(
                "Enter accept  Esc close",
                Style::default().fg(Color::DarkGray),
            ),
        ],
        ProviderWizardStep::ApiKey => vec![
            Line::styled("API key", emphasis_style()),
            Line::raw(""),
            Line::raw(mask_secret(&wizard.api_key)),
            Line::raw(""),
            Line::styled(
                "Enter accept  Esc close",
                Style::default().fg(Color::DarkGray),
            ),
        ],
        ProviderWizardStep::Model => vec![
            Line::styled("Model", emphasis_style()),
            Line::raw(""),
            Line::raw(wizard.model.clone()),
            Line::raw(""),
            Line::styled(
                "Enter save  Esc close",
                Style::default().fg(Color::DarkGray),
            ),
        ],
    };

    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .title(format!(" {} ", wizard.title()))
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn centered_rect(area: Rect, max_width: u16, height: u16) -> Rect {
    let width = max_width.min(area.width.saturating_sub(4)).max(1);
    let height = height.min(area.height.saturating_sub(2)).max(1);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
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

fn label_style() -> Style {
    Style::default()
        .fg(Color::Gray)
        .add_modifier(Modifier::BOLD)
}

fn emphasis_style() -> Style {
    Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD)
}

fn command_style() -> Style {
    Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::BOLD)
}

fn display_path(path: &Path) -> String {
    path.display().to_string()
}

struct SetupTerminal {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl SetupTerminal {
    fn enter() -> Result<Self, TuiError> {
        enable_raw_mode().map_err(TuiError::Io)?;
        let mut stdout = io::stdout();
        // Enable bracketed paste alongside the alternate screen so multi-line and
        // image-clipboard pastes arrive as a single Event::Paste instead of a run
        // of key presses that would corrupt the prompt or terminal state.
        if let Err(err) = execute!(stdout, EnterAlternateScreen, EnableBracketedPaste) {
            let _ = disable_raw_mode();
            return Err(TuiError::Io(err));
        }
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend).map_err(TuiError::Io)?;
        terminal.clear().map_err(TuiError::Io)?;
        Ok(Self { terminal })
    }

    fn draw(&mut self, app: &SetupTuiApp) -> Result<(), TuiError> {
        self.terminal
            .draw(|frame| render_setup_tui(frame, app))
            .map(|_| ())
            .map_err(TuiError::Io)
    }

    fn draw_tui(&mut self, app: &TuiApp) -> Result<(), TuiError> {
        self.terminal
            .draw(|frame| render_tui(frame, app))
            .map(|_| ())
            .map_err(TuiError::Io)
    }
}

impl Drop for SetupTerminal {
    fn drop(&mut self) {
        let _ = self.terminal.show_cursor();
        let _ = execute!(
            self.terminal.backend_mut(),
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
        let _ = disable_raw_mode();
    }
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
