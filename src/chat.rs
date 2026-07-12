//! Pure chat-session state machine on the `harness-tui` library.
//!
//! A faithful port of the previous chat TUI state machine onto
//! `harness_tui` primitives: terminal key events become
//! `harness_tui::input` events, styled lines become
//! `harness_tui::text::Line`s, and the hand-rolled transcript scrolling is
//! gone entirely — finished entries are flushed to native terminal
//! scrollback via [`ChatApp::take_scrollback`] while the pinned panel
//! ([`ChatApp::panel_lines`]) shows only the live tail, the editor, and the
//! status row.

use std::path::PathBuf;
use std::time::Instant;

use harness_tui::components::editor::Editor;
use harness_tui::components::menu::{MenuItem, menu_lines};
use harness_tui::components::spinner::spinner_lines;
use harness_tui::components::status::status_line;
use harness_tui::input::{Event, KeyCode, KeyEvent};
use harness_tui::text::{Color, Line, Span, Style, visible_width};

/// What the event loop should do after the app handled an input event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatAction {
    Continue,
    Submit(String),
    SwitchModel {
        provider: String,
        model: String,
    },
    /// Ctrl+V: ask the loop to read the system clipboard (text or PNG image)
    /// and fold it into the prompt. The state machine can't touch I/O itself,
    /// so it delegates the capture and the loop calls back into the app.
    CaptureClipboard,
    /// `/new`: ask the loop to abandon the resumed conversation and start a
    /// fresh session file (the transcript is already cleared by the app).
    NewSession,
    /// `/clear`: the transcript is already cleared by the app; the loop must
    /// wipe the terminal screen and scrollback so old blocks disappear too.
    ClearScreen,
    Exit,
}

/// What to do with an input event that arrives while the agent is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusyAction {
    Cancel,
    Ignore,
}

/// Input policy while the agent is busy: Esc or Ctrl+C cancel the run,
/// everything else is ignored (the wheel scrolls natively now).
pub fn busy_action(event: &Event) -> BusyAction {
    match event {
        Event::Key(key) if key.code == KeyCode::Esc => BusyAction::Cancel,
        Event::Key(key) if key.mods.ctrl && key.code == KeyCode::Char('c') => BusyAction::Cancel,
        _ => BusyAction::Ignore,
    }
}

const EDITOR_PROMPT: &str = "> ";
const EDITOR_PLACEHOLDER: &str = "Type a message · / for commands · Alt+Enter newline";
/// Maximum editor rows shown in the pinned panel.
const EDITOR_MAX_ROWS: usize = 6;
/// Maximum completion-menu rows shown in the pinned panel.
const COMPLETION_MAX_ROWS: usize = 6;

const CYAN: Color = Color::Ansi(6);
const GREEN: Color = Color::Ansi(2);
const RED: Color = Color::Ansi(1);
const LIGHT_GREEN: Color = Color::Ansi(10);

fn dim_style() -> Style {
    Style {
        dim: true,
        ..Style::default()
    }
}

fn fg_style(color: Color) -> Style {
    Style {
        fg: color,
        ..Style::default()
    }
}

fn styled_line(text: impl Into<String>, style: Style) -> Line {
    Line {
        spans: vec![Span::styled(text, style)],
    }
}

/// A slash command exposed in the chat session, used for both dispatch and
/// the `/`-triggered autocomplete menu.
struct ChatCommand {
    name: &'static str,
    usage: &'static str,
    description: &'static str,
}

const CHAT_COMMANDS: &[ChatCommand] = &[
    ChatCommand {
        name: "/model",
        usage: "/model PROVIDER MODEL",
        description: "switch the active provider/model",
    },
    ChatCommand {
        name: "/provider",
        usage: "/provider",
        description: "show the active provider",
    },
    ChatCommand {
        name: "/history",
        usage: "/history QUERY",
        description: "search your past prompts",
    },
    ChatCommand {
        name: "/clear",
        usage: "/clear",
        description: "clear the transcript",
    },
    ChatCommand {
        name: "/new",
        usage: "/new",
        description: "start a fresh chat session",
    },
    ChatCommand {
        name: "/help",
        usage: "/help",
        description: "show the command palette",
    },
    ChatCommand {
        name: "/exit",
        usage: "/exit",
        description: "leave the session",
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolStatus {
    Running,
    Ok,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolEntry {
    id: String,
    name: String,
    args: String,
    status: ToolStatus,
    summary: Option<String>,
}

/// A single block in the transcript. Tool calls are a structured entry so a
/// streamed `ToolResult` can update the matching call in place (Running →
/// Ok/Failed) instead of appending a disconnected line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ChatEntry {
    User(String),
    Assistant(String),
    Thinking(String),
    Tool(ToolEntry),
    System(String),
}

/// The chat session state machine: a transcript of user turns, tool
/// activity, and streamed assistant output, plus a prompt editor. Pure —
/// input events go in, [`ChatAction`]s and rendered lines come out. There is
/// no scroll state: finalized entries flush to native scrollback and only
/// the live tail stays pinned.
pub struct ChatApp {
    provider_label: String,
    workspace: PathBuf,
    editor: Editor,
    transcript: Vec<ChatEntry>,
    /// How many transcript entries are already flushed to scrollback.
    emitted: usize,
    streaming_assistant: bool,
    streaming_thinking: bool,
    history: Vec<String>,
    history_cursor: Option<usize>,
    help_visible: bool,
    /// Highlighted row in the `/`-autocomplete menu.
    completion_index: usize,
    /// Set when Esc dismisses the menu so it stays closed until the query changes.
    completion_dismissed: bool,
    /// True while the agent is running, to drive the spinner / status line.
    busy: bool,
    /// When the current run started, for the elapsed time next to `Working…`.
    busy_since: Option<Instant>,
    spinner_frame: usize,
}

impl ChatApp {
    pub fn new(provider_label: impl Into<String>, workspace: impl Into<PathBuf>) -> Self {
        Self {
            provider_label: provider_label.into(),
            workspace: workspace.into(),
            editor: Editor::new(EDITOR_PROMPT, EDITOR_PLACEHOLDER),
            transcript: Vec::new(),
            emitted: 0,
            streaming_assistant: false,
            streaming_thinking: false,
            history: Vec::new(),
            history_cursor: None,
            help_visible: false,
            completion_index: 0,
            completion_dismissed: false,
            busy: false,
            busy_since: None,
            spinner_frame: 0,
        }
    }

    /// The current compose text.
    pub fn input(&self) -> &str {
        self.editor.text()
    }

    /// Mark the agent as running (or finished) so the view can show a spinner.
    pub fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
        self.busy_since = busy.then(Instant::now);
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    /// Whole seconds the current run has been going (0 when idle).
    pub fn busy_elapsed_secs(&self) -> u64 {
        self.busy_since
            .map(|since| since.elapsed().as_secs())
            .unwrap_or(0)
    }

    /// Advance the spinner animation by one frame.
    pub fn tick(&mut self) {
        self.spinner_frame = self.spinner_frame.wrapping_add(1);
    }

    /// Whether the `/`-autocomplete menu is currently shown.
    pub fn completion_visible(&self) -> bool {
        !self.completion_dismissed && !self.help_visible && !self.completion_matches().is_empty()
    }

    /// Command names currently offered by the autocomplete menu.
    pub fn completion_suggestions(&self) -> Vec<String> {
        self.completion_matches()
            .iter()
            .map(|command| command.name.to_string())
            .collect()
    }

    /// The command name the autocomplete menu would accept on Tab.
    pub fn completion_selected(&self) -> Option<String> {
        let matches = self.completion_matches();
        if matches.is_empty() {
            return None;
        }
        let index = self.completion_index.min(matches.len() - 1);
        Some(matches[index].name.to_string())
    }

    /// Whether the command palette / help overlay is open.
    pub fn help_visible(&self) -> bool {
        self.help_visible
    }

    /// Update the status-row label after an in-session model switch.
    pub fn set_provider_label(&mut self, label: impl Into<String>) {
        self.provider_label = label.into();
    }

    /// How many transcript entries have been flushed to scrollback.
    pub fn emitted(&self) -> usize {
        self.emitted
    }

    pub fn transcript_len(&self) -> usize {
        self.transcript.len()
    }

    /// The transcript flattened to text, used by tests and non-TTY fallbacks.
    pub fn transcript_text(&self) -> String {
        self.transcript
            .iter()
            .flat_map(entry_plain_lines)
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ChatAction {
        // Ctrl+C always exits, even with the palette open.
        if key.mods.ctrl && key.code == KeyCode::Char('c') {
            return ChatAction::Exit;
        }

        // Ctrl+V delegates a system-clipboard capture (text or image) to the loop.
        if key.mods.ctrl && key.code == KeyCode::Char('v') {
            return ChatAction::CaptureClipboard;
        }

        // While the command palette is open, keys dismiss it instead of editing.
        if self.help_visible {
            self.help_visible = false;
            return ChatAction::Continue;
        }

        let completion_open = self.completion_visible();

        match key.code {
            KeyCode::Esc => {
                if completion_open {
                    self.completion_dismissed = true;
                    ChatAction::Continue
                } else {
                    ChatAction::Exit
                }
            }
            // Alt+Enter composes a newline instead of submitting, so a prompt
            // can span multiple lines without sending early. (Shift+Enter is
            // indistinguishable from Enter in a VT byte stream, so Alt+Enter
            // replaces it; Ctrl+J is the classic fallback.)
            KeyCode::Enter if key.mods.alt => {
                self.editor.insert_char('\n');
                ChatAction::Continue
            }
            KeyCode::Char('j') if key.mods.ctrl => {
                self.editor.insert_char('\n');
                ChatAction::Continue
            }
            KeyCode::Enter => self.submit_input(),
            KeyCode::Tab if completion_open => {
                self.complete_selection();
                ChatAction::Continue
            }
            KeyCode::Backspace => {
                self.editor.backspace();
                self.reset_completion();
                ChatAction::Continue
            }
            KeyCode::Left => {
                self.editor.move_left();
                ChatAction::Continue
            }
            KeyCode::Right => {
                self.editor.move_right();
                ChatAction::Continue
            }
            KeyCode::Home => {
                self.editor.move_home();
                ChatAction::Continue
            }
            KeyCode::End => {
                self.editor.move_end();
                ChatAction::Continue
            }
            KeyCode::Up => {
                if completion_open {
                    self.completion_index = self.completion_index.saturating_sub(1);
                } else {
                    self.recall_previous();
                }
                ChatAction::Continue
            }
            KeyCode::Down => {
                if completion_open {
                    let last = self.completion_matches().len().saturating_sub(1);
                    self.completion_index = (self.completion_index + 1).min(last);
                } else {
                    self.recall_next();
                }
                ChatAction::Continue
            }
            // Native terminal scrollback replaces the hand-written transcript
            // scrolling, so these keys are deliberately inert.
            KeyCode::PageUp | KeyCode::PageDown => ChatAction::Continue,
            KeyCode::Char(ch) if !key.mods.ctrl => {
                self.editor.insert_char(ch);
                self.reset_completion();
                ChatAction::Continue
            }
            _ => ChatAction::Continue,
        }
    }

    pub fn handle_paste(&mut self, text: &str) -> ChatAction {
        // A chat message may legitimately span lines, so the paste is inserted
        // verbatim at the caret; it never submits because it is a single event.
        self.editor.insert_str(text);
        self.reset_completion();
        ChatAction::Continue
    }

    /// Insert clipboard text at the caret (used by the Ctrl+V capture path).
    pub fn apply_clipboard_text(&mut self, prompt_text: &str) {
        self.editor.insert_str(prompt_text);
        self.reset_completion();
    }

    fn reset_completion(&mut self) {
        self.completion_dismissed = false;
        let len = self.completion_matches().len();
        if len == 0 {
            self.completion_index = 0;
        } else {
            self.completion_index = self.completion_index.min(len - 1);
        }
    }

    /// The candidate commands for the current input, or empty if not typing a
    /// `/command` token (no leading slash, or a space already typed).
    fn completion_matches(&self) -> Vec<&'static ChatCommand> {
        let query = self.editor.text();
        if !query.starts_with('/') || query.chars().any(char::is_whitespace) {
            return Vec::new();
        }
        let query = query.to_ascii_lowercase();
        CHAT_COMMANDS
            .iter()
            .filter(|command| command.name.starts_with(&query))
            .collect()
    }

    fn complete_selection(&mut self) {
        let Some(name) = self.completion_selected() else {
            return;
        };
        self.editor.set_text(format!("{name} "));
        // The trailing space closes the menu; keep it dismissed until further edits.
        self.completion_dismissed = true;
    }

    fn submit_input(&mut self) -> ChatAction {
        let text = self.editor.text().trim().to_string();
        self.editor.clear();
        self.history_cursor = None;
        self.completion_dismissed = false;
        self.completion_index = 0;
        if text.is_empty() {
            return ChatAction::Continue;
        }
        if let Some(command) = text.strip_prefix('/') {
            return self.run_slash_command(command);
        }
        self.history.push(text.clone());
        self.push_user_message(&text);
        ChatAction::Submit(text)
    }

    fn run_slash_command(&mut self, command: &str) -> ChatAction {
        let mut parts = command.split_whitespace();
        match parts.next() {
            Some("help") | Some("?") => {
                self.help_visible = true;
                ChatAction::Continue
            }
            Some("clear") => {
                self.transcript.clear();
                self.emitted = 0;
                ChatAction::ClearScreen
            }
            Some("new") => {
                self.transcript.clear();
                self.emitted = 0;
                ChatAction::NewSession
            }
            Some("exit") | Some("quit") => ChatAction::Exit,
            Some("provider") => {
                let label = self.provider_label.clone();
                self.push_system_line(format!("active provider: {label}"));
                ChatAction::Continue
            }
            Some("model") => {
                let provider = parts.next();
                let model = parts.next();
                match (provider, model) {
                    (Some(provider), Some(model)) if parts.next().is_none() => {
                        ChatAction::SwitchModel {
                            provider: provider.to_string(),
                            model: model.to_string(),
                        }
                    }
                    _ => {
                        self.push_system_line("usage: /model PROVIDER MODEL");
                        ChatAction::Continue
                    }
                }
            }
            Some("history") => {
                let query = parts.collect::<Vec<_>>().join(" ");
                if query.trim().is_empty() {
                    self.push_system_line("usage: /history QUERY");
                    return ChatAction::Continue;
                }
                let needle = query.to_ascii_lowercase();
                let matches: Vec<String> = self
                    .history
                    .iter()
                    .enumerate()
                    .rev()
                    .filter(|(_, message)| message.to_ascii_lowercase().contains(&needle))
                    .map(|(index, message)| format!("  [{}] {}", index + 1, message))
                    .collect();
                self.push_system_line(format!("history '{query}': {} match(es)", matches.len()));
                for line in matches {
                    self.transcript.push(ChatEntry::System(line));
                }
                ChatAction::Continue
            }
            Some(other) => {
                self.push_system_line(format!("unknown command: /{other} (try /help)"));
                ChatAction::Continue
            }
            None => ChatAction::Continue,
        }
    }

    fn recall_previous(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = match self.history_cursor {
            None => self.history.len() - 1,
            Some(0) => 0,
            Some(index) => index - 1,
        };
        self.history_cursor = Some(next);
        self.editor.set_text(self.history[next].clone());
    }

    fn recall_next(&mut self) {
        let Some(index) = self.history_cursor else {
            return;
        };
        if index + 1 < self.history.len() {
            self.history_cursor = Some(index + 1);
            self.editor.set_text(self.history[index + 1].clone());
        } else {
            self.history_cursor = None;
            self.editor.clear();
        }
    }

    pub fn push_user_message(&mut self, message: &str) {
        self.transcript.push(ChatEntry::User(message.to_string()));
        self.streaming_assistant = false;
        self.streaming_thinking = false;
        self.tick();
    }

    /// Append a system/status line (welcome banner, error notice, etc.).
    pub fn push_system_line(&mut self, text: impl Into<String>) {
        self.transcript.push(ChatEntry::System(text.into()));
        self.streaming_assistant = false;
        self.streaming_thinking = false;
        self.tick();
    }

    /// Fold a streamed agent event into the transcript so the view updates as
    /// the agent works through tool rounds and the final answer.
    pub fn push_agent_event(&mut self, event: &crate::agent::AgentEvent) {
        use crate::agent::AgentEvent;
        self.tick();
        match event {
            AgentEvent::Thinking(text) => {
                // Coalesce streamed reasoning fragments into one growing block.
                if self.streaming_thinking
                    && let Some(ChatEntry::Thinking(existing)) = self.transcript.last_mut()
                {
                    existing.push_str(text);
                } else {
                    self.transcript.push(ChatEntry::Thinking(text.clone()));
                    self.streaming_thinking = true;
                }
                self.streaming_assistant = false;
            }
            AgentEvent::ToolRoundStarted { round, tool_calls } => {
                // A single-call round would just duplicate its tool card; only
                // multi-call rounds earn a banner line.
                if *tool_calls > 1 {
                    self.transcript.push(ChatEntry::System(format!(
                        "tool round {round}: {tool_calls} call(s)"
                    )));
                }
                self.streaming_assistant = false;
                self.streaming_thinking = false;
            }
            AgentEvent::ToolCallStarted {
                id,
                name,
                arguments,
                ..
            } => {
                self.transcript.push(ChatEntry::Tool(ToolEntry {
                    id: id.clone(),
                    name: display_tool_name(name),
                    args: compact_json_args(arguments),
                    status: ToolStatus::Running,
                    summary: None,
                }));
                self.streaming_assistant = false;
                self.streaming_thinking = false;
            }
            AgentEvent::ToolResult(result) => {
                self.apply_tool_result(result);
                self.streaming_assistant = false;
                self.streaming_thinking = false;
            }
            AgentEvent::FinalContentDelta(delta) => {
                self.streaming_thinking = false;
                if self.streaming_assistant
                    && let Some(ChatEntry::Assistant(text)) = self.transcript.last_mut()
                {
                    text.push_str(delta);
                } else {
                    self.transcript.push(ChatEntry::Assistant(delta.clone()));
                    self.streaming_assistant = true;
                }
            }
        }
    }

    /// Update the matching in-flight tool card (by id) to its terminal status,
    /// or append a finished card if no `ToolCallStarted` preceded this result.
    fn apply_tool_result(&mut self, result: &crate::runtime::ToolBatchResult) {
        let status = if result.ok {
            ToolStatus::Ok
        } else {
            ToolStatus::Failed
        };
        let summary = tool_result_summary(result);
        let pending = self
            .transcript
            .iter_mut()
            .rev()
            .find_map(|entry| match entry {
                ChatEntry::Tool(tool)
                    if tool.id == result.id && tool.status == ToolStatus::Running =>
                {
                    Some(tool)
                }
                _ => None,
            });
        match pending {
            Some(tool) => {
                tool.status = status;
                tool.summary = summary;
            }
            None => self.transcript.push(ChatEntry::Tool(ToolEntry {
                id: result.id.clone(),
                name: display_tool_name(&result.tool_name),
                args: String::new(),
                status,
                summary,
            })),
        }
        // Surface the self-correction memo so the user sees how the next call
        // should be shaped (the "forgiving tools" feedback loop).
        if let Some(hint) = &result.hint {
            self.transcript
                .push(ChatEntry::System(format!("memo: {hint}")));
        }
    }

    /// The pinned bottom panel, top to bottom: the live (not yet flushed)
    /// transcript tail, the busy spinner, the command palette, the editor,
    /// the completion menu, and the status row.
    pub fn panel_lines(&self, width: usize, max_live_rows: usize) -> Vec<Line> {
        self.panel_lines_after(width, max_live_rows, self.emitted)
    }

    /// `panel_lines` with an explicit commit boundary — the draw loop
    /// passes the boundary from `peek_scrollback` so the rows being
    /// committed in the same frame are not also rendered as live.
    pub fn panel_lines_after(&self, width: usize, max_live_rows: usize, from: usize) -> Vec<Line> {
        let mut lines = Vec::new();

        let mut live: Vec<Line> = Vec::new();
        for entry in &self.transcript[from.min(self.transcript.len())..] {
            if !live.is_empty() {
                live.push(Line::default());
            }
            live.extend(entry_lines(entry, width));
        }
        // Over the cap, keep the tail — the newest rows matter while streaming.
        if live.len() > max_live_rows {
            live.drain(..live.len() - max_live_rows);
        }
        let has_live = !live.is_empty();
        lines.extend(live);

        if self.busy {
            // Two rows: a blank spacer, then ` ⠋ Working… (5s)`.
            lines.extend(spinner_lines(self.spinner_frame, self.busy_elapsed_secs()));
        } else if has_live {
            lines.push(Line::default());
        }

        if self.help_visible {
            lines.extend(palette_lines());
        }

        let editor_rows = self.editor.render(width.saturating_sub(2), EDITOR_MAX_ROWS);
        lines.extend(framed_lines(editor_rows, width));

        if self.completion_visible() {
            let items: Vec<MenuItem> = self
                .completion_matches()
                .iter()
                .map(|command| MenuItem {
                    name: command.name.to_string(),
                    usage: command.usage.to_string(),
                    description: command.description.to_string(),
                })
                .collect();
            let selected = self.completion_index.min(items.len().saturating_sub(1));
            lines.extend(menu_lines(
                &items,
                self.editor.text(),
                selected,
                COMPLETION_MAX_ROWS,
            ));
        }

        lines.push(self.status_row(width));
        lines
    }

    /// Flush finalized transcript entries to native scrollback: returns their
    /// rendered lines (one blank line after each entry) and advances
    /// `emitted`. While the agent is busy, only entries strictly before both
    /// the last entry (it may still be streaming) and the first Running tool
    /// card are final; once the run ends everything flushes.
    pub fn take_scrollback(&mut self, width: usize) -> Vec<Line> {
        let (lines, limit) = self.peek_scrollback(width);
        self.acknowledge_emitted(limit);
        lines
    }

    /// Side-effect-free flush plan: the finalized rows ready for native
    /// scrollback and the transcript index they cover. `emitted` moves
    /// only in `acknowledge_emitted` — AFTER the terminal write succeeds
    /// — so a failed write offers the same rows again instead of losing
    /// them.
    pub fn peek_scrollback(&self, width: usize) -> (Vec<Line>, usize) {
        let mut limit = self.transcript.len();
        if self.busy {
            limit = limit.saturating_sub(1);
            // Only unflushed Running cards hold the prefix back — a
            // stale card from a cancelled run was already emitted and
            // must not freeze every later turn's flush.
            if let Some(running) = self.transcript[self.emitted..].iter().position(
                |entry| matches!(entry, ChatEntry::Tool(tool) if tool.status == ToolStatus::Running),
            ) {
                limit = limit.min(self.emitted + running);
            }
        }
        if limit <= self.emitted {
            return (Vec::new(), self.emitted);
        }
        let mut lines = Vec::new();
        for entry in &self.transcript[self.emitted..limit] {
            lines.extend(entry_lines(entry, width));
            lines.push(Line::default());
        }
        (lines, limit)
    }

    /// Advance the commit boundary after the rows from `peek_scrollback`
    /// actually reached the terminal.
    pub fn acknowledge_emitted(&mut self, through: usize) {
        self.emitted = self.emitted.max(through.min(self.transcript.len()));
    }

    /// Bottom status row: provider/workspace on the left, key hints on the
    /// right; the hints are dropped whole on narrow terminals.
    fn status_row(&self, width: usize) -> Line {
        let left = Line {
            spans: vec![
                Span::styled(format!(" {}", self.provider_label), fg_style(CYAN)),
                Span::styled(format!(" · {}", self.workspace.display()), dim_style()),
            ],
        };
        let hint = if self.completion_visible() {
            "↑↓ select · Tab complete · Esc close "
        } else {
            "Enter send · Alt+Enter newline · / commands · Ctrl+V paste · Esc exit "
        };
        let right = Line {
            spans: vec![Span::styled(hint, dim_style())],
        };
        status_line(width, left, right)
    }
}

/// Wrap rows in a rounded full-width frame: `╭─╮` / `│ … │` / `╰─╯`.
/// Rows are clipped and padded to the inner width so the right border
/// always lines up.
fn framed_lines(inner: Vec<Line>, width: usize) -> Vec<Line> {
    let width = width.max(4);
    let inner_width = width - 2;
    let horizontal = "─".repeat(inner_width);
    let mut lines = Vec::with_capacity(inner.len() + 2);
    lines.push(styled_line(format!("╭{horizontal}╮"), dim_style()));
    for row in inner {
        let (mut spans, used) = clip_spans(row, inner_width);
        spans.insert(0, Span::styled("│", dim_style()));
        if used < inner_width {
            spans.push(Span::raw(" ".repeat(inner_width - used)));
        }
        spans.push(Span::styled("│", dim_style()));
        lines.push(Line { spans });
    }
    lines.push(styled_line(format!("╰{horizontal}╯"), dim_style()));
    lines
}

/// Truncate a row to `max_width` columns, keeping span styles; returns
/// the surviving spans and the columns they occupy.
fn clip_spans(row: Line, max_width: usize) -> (Vec<Span>, usize) {
    let mut spans = Vec::new();
    let mut used = 0usize;
    for span in row.spans {
        if used >= max_width {
            break;
        }
        let span_width = visible_width(&span.text);
        if used + span_width <= max_width {
            used += span_width;
            spans.push(span);
            continue;
        }
        let mut clipped = String::new();
        for ch in span.text.chars() {
            let mut buf = [0u8; 4];
            let ch_width = visible_width(ch.encode_utf8(&mut buf));
            if used + ch_width > max_width {
                break;
            }
            clipped.push(ch);
            used += ch_width;
        }
        if !clipped.is_empty() {
            spans.push(Span {
                text: clipped,
                style: span.style,
            });
        }
        break;
    }
    (spans, used)
}

/// The `/help` palette as plain panel lines.
fn palette_lines() -> Vec<Line> {
    let mut lines = vec![styled_line(
        "Commands",
        Style {
            bold: true,
            ..Style::default()
        },
    )];
    for command in CHAT_COMMANDS {
        lines.push(Line {
            spans: vec![
                Span::raw(format!("  {:<22}", command.usage)),
                Span::styled(command.description, dim_style()),
            ],
        });
    }
    lines.push(styled_line(
        "Up/Down recall · Ctrl+V paste · Esc close",
        dim_style(),
    ));
    lines
}

/// Render one transcript entry as styled lines wrapped to `width` columns
/// with a hanging indent under the entry's marker.
fn entry_lines(entry: &ChatEntry, width: usize) -> Vec<Line> {
    entry_styled_lines(entry)
        .into_iter()
        .flat_map(|line| wrap_styled_line(line, width))
        .collect()
}

/// Render one transcript entry as one or more styled lines: a `>` echo on a
/// highlight strip for user turns, `●`-marked assistant/tool blocks, and
/// unlabeled dim reasoning.
fn entry_styled_lines(entry: &ChatEntry) -> Vec<Line> {
    match entry {
        ChatEntry::User(text) => {
            // The user's turn sits on a subtle highlight strip (256-color dark
            // gray; legacy consoles map it to the nearest base color), so it
            // reads as "input" at a glance and never blends into reasoning.
            let strip = Style {
                bg: Color::Indexed(236),
                ..Style::default()
            };
            let marker_style = Style { fg: CYAN, ..strip };
            text.split('\n')
                .enumerate()
                .map(|(index, raw)| {
                    let prefix = if index == 0 { "> " } else { "  " };
                    Line {
                        spans: vec![
                            Span::styled(prefix, marker_style),
                            Span::styled(format!("{raw} "), strip),
                        ],
                    }
                })
                .collect()
        }
        ChatEntry::Assistant(text) => {
            // The accent marker leads the first markdown line; continuations
            // are indented to the same column so the block hangs together.
            let mut lines = Vec::new();
            for (index, line) in markdown_lines(text).into_iter().enumerate() {
                if index == 0 {
                    let mut spans = vec![Span::styled("● ", fg_style(CYAN))];
                    spans.extend(line.spans);
                    lines.push(Line { spans });
                } else {
                    lines.push(indent_line(line, "  "));
                }
            }
            lines
        }
        ChatEntry::Thinking(text) => {
            // Reasoning renders as plain dim italic text with no badge — the
            // model's inner voice should not carry a label in the transcript.
            let style = Style {
                dim: true,
                italic: true,
                ..Style::default()
            };
            text.split('\n')
                .map(|raw| Line {
                    spans: vec![Span::raw("  "), Span::styled(raw, style)],
                })
                .collect()
        }
        ChatEntry::System(text) => {
            let style = if text.starts_with("memo:") {
                Style {
                    fg: CYAN,
                    italic: true,
                    ..Style::default()
                }
            } else {
                dim_style()
            };
            vec![Line {
                spans: vec![Span::styled("· ", style), Span::styled(text.clone(), style)],
            }]
        }
        ChatEntry::Tool(tool) => tool_card_lines(tool),
    }
}

/// Prepend a fixed indent to a styled line (used to nest markdown under a badge).
fn indent_line(line: Line, pad: &str) -> Line {
    let mut spans = vec![Span::raw(pad)];
    spans.extend(line.spans);
    Line { spans }
}

fn tool_card_lines(tool: &ToolEntry) -> Vec<Line> {
    // The `●` marker carries the status color; the tool name stays neutral
    // bold so the eye scans markers, not a rainbow of glyphs.
    let marker_style = match tool.status {
        ToolStatus::Running => dim_style(),
        ToolStatus::Ok => fg_style(GREEN),
        ToolStatus::Failed => fg_style(RED),
    };
    let mut spans = vec![
        Span::styled("● ", marker_style),
        Span::styled(
            tool.name.clone(),
            Style {
                bold: true,
                ..Style::default()
            },
        ),
    ];
    if !tool.args.is_empty() {
        spans.push(Span::styled(format!("({})", tool.args), dim_style()));
    }
    let mut lines = vec![Line { spans }];
    if let Some(summary) = &tool.summary {
        lines.push(styled_line(format!("  ⎿ {summary}"), dim_style()));
    }
    lines
}

/// Word-wrap one styled line to `width` columns, preserving span styles.
/// Breaks at the last space inside the window when there is one, else
/// mid-word. Continuation rows hang-indent to the block's text column, so a
/// wrapped turn stays visually inside its `>`/`●`/gutter block instead of
/// snapping to x=0.
fn wrap_styled_line(line: Line, width: usize) -> Vec<Line> {
    let width = width.max(1);
    let chars: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|span| {
            span.text
                .chars()
                .map(|ch| (ch, span.style))
                .collect::<Vec<_>>()
        })
        .collect();
    if chars.len() <= width {
        return vec![line];
    }

    let indent = hanging_indent(&chars).min(width / 2);
    let pad = " ".repeat(indent);
    let mut rows = Vec::new();
    let mut start = 0;
    let mut first = true;
    while start < chars.len() {
        if !first {
            // Drop leftover spaces at a break so the indent stays exact.
            while start < chars.len() && chars[start].0 == ' ' {
                start += 1;
            }
            if start == chars.len() {
                break;
            }
        }
        let avail = if first { width } else { width - indent };
        let row_chars = if chars.len() - start <= avail {
            let row = &chars[start..];
            start = chars.len();
            row
        } else {
            let window = &chars[start..start + avail];
            match window.iter().rposition(|(ch, _)| *ch == ' ') {
                Some(pos) if pos > 0 => {
                    let row = &chars[start..start + pos];
                    start += pos + 1; // the break space itself is consumed
                    row
                }
                _ => {
                    start += avail;
                    window
                }
            }
        };
        let mut row = line_from_chars(row_chars);
        if !first {
            row.spans.insert(0, Span::raw(pad.clone()));
        }
        rows.push(row);
        first = false;
    }
    rows
}

/// The hanging-indent width of a block line: its leading spaces and marker
/// glyphs (`>`, `●`, `·`, `⎿`, `•`, `│`), i.e. the column where the text starts.
fn hanging_indent(chars: &[(char, Style)]) -> usize {
    chars
        .iter()
        .take_while(|(ch, _)| *ch == ' ' || matches!(ch, '>' | '●' | '·' | '⎿' | '•' | '│'))
        .count()
}

/// Rebuild a styled line from `(char, style)` pairs, merging equal-style runs.
fn line_from_chars(chars: &[(char, Style)]) -> Line {
    let mut spans: Vec<Span> = Vec::new();
    let mut buf = String::new();
    let mut current: Option<Style> = None;
    for (ch, style) in chars {
        if current != Some(*style) {
            if let Some(style) = current
                && !buf.is_empty()
            {
                spans.push(Span::styled(std::mem::take(&mut buf), style));
            }
            current = Some(*style);
        }
        buf.push(*ch);
    }
    if let Some(style) = current
        && !buf.is_empty()
    {
        spans.push(Span::styled(buf, style));
    }
    Line { spans }
}

/// Render a block of (lightweight) Markdown to styled lines: ATX headings,
/// `-`/`*` bullets, fenced code blocks, horizontal rules, tables, and inline
/// `**bold**`, `*italic*`, and `` `code` ``. Deliberately a pragmatic subset,
/// not CommonMark.
fn markdown_lines(text: &str) -> Vec<Line> {
    // Terminal-default foreground so the transcript respects light/dark themes.
    let base = Style::default();
    let raw_lines: Vec<&str> = text.split('\n').collect();
    let mut lines = Vec::new();
    let mut in_code = false;
    let mut i = 0;

    while i < raw_lines.len() {
        let raw = raw_lines[i];
        let trimmed = raw.trim_start();

        if trimmed.starts_with("```") {
            in_code = !in_code;
            lines.push(styled_line("  ┄┄┄", dim_style()));
            i += 1;
            continue;
        }
        if in_code {
            lines.push(Line {
                spans: vec![
                    Span::styled("│ ", dim_style()),
                    Span::styled(raw, code_style()),
                ],
            });
            i += 1;
            continue;
        }

        // A run of `| … |` rows is a table: gather the whole block and lay it
        // out as aligned columns instead of leaking raw pipes.
        if is_table_row(trimmed) {
            let start = i;
            while i < raw_lines.len() && is_table_row(raw_lines[i].trim_start()) {
                i += 1;
            }
            lines.extend(table_lines(&raw_lines[start..i], base));
            continue;
        }

        if let Some(rest) = heading_text(trimmed) {
            let (level, body) = rest;
            // One accent color for headings; deeper levels drop the accent and
            // keep only the bold weight.
            let style = if level <= 2 {
                Style {
                    fg: CYAN,
                    bold: true,
                    ..Style::default()
                }
            } else {
                Style {
                    bold: true,
                    ..Style::default()
                }
            };
            lines.push(styled_line(body, style));
            i += 1;
            continue;
        }

        if is_horizontal_rule(trimmed) || is_table_separator(trimmed) {
            lines.push(styled_line("─".repeat(24), dim_style()));
            i += 1;
            continue;
        }

        if let Some((indent, marker_len)) = bullet_prefix(raw) {
            let rest = &raw[indent + marker_len..];
            let mut spans = vec![Span::styled(
                format!("{}• ", " ".repeat(indent)),
                fg_style(CYAN),
            )];
            spans.extend(inline_spans(rest, base));
            lines.push(Line { spans });
            i += 1;
            continue;
        }

        lines.push(Line {
            spans: inline_spans(raw, base),
        });
        i += 1;
    }

    lines
}

/// A Markdown table row: starts with `|` and has at least one more pipe.
/// Separator rows (`|---|---|`) also match — `table_lines` filters them out.
fn is_table_row(line: &str) -> bool {
    line.starts_with('|') && line[1..].contains('|')
}

/// Lay a block of `| … |` rows out as padded columns: cells keep their inline
/// markdown, the header (a row directly above a `|---|` separator) is bold
/// and underlined by a rule sized to the real column widths.
fn table_lines(rows: &[&str], base: Style) -> Vec<Line> {
    const GAP: usize = 2;

    let mut body: Vec<(bool, Vec<Vec<Span>>)> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let trimmed = row.trim();
        if is_table_separator(trimmed) {
            // The row above the first separator is the header.
            if index == 1
                && let Some(first) = body.first_mut()
            {
                first.0 = true;
            }
            continue;
        }
        let cells: Vec<Vec<Span>> = trimmed
            .trim_start_matches('|')
            .trim_end_matches('|')
            .split('|')
            .map(|cell| inline_spans(cell.trim(), base))
            .collect();
        body.push((false, cells));
    }

    let span_width =
        |spans: &[Span]| -> usize { spans.iter().map(|s| s.text.chars().count()).sum() };
    let columns = body.iter().map(|(_, cells)| cells.len()).max().unwrap_or(0);
    let mut widths = vec![0usize; columns];
    for (_, cells) in &body {
        for (column, cell) in cells.iter().enumerate() {
            widths[column] = widths[column].max(span_width(cell));
        }
    }

    let mut lines = Vec::new();
    for (is_header, cells) in body {
        let mut spans: Vec<Span> = Vec::new();
        let last = cells.len().saturating_sub(1);
        let mut cell_widths = Vec::new();
        for (column, cell) in cells.into_iter().enumerate() {
            let width = span_width(&cell);
            cell_widths.push(width);
            for span in cell {
                let span = if is_header {
                    Span::styled(
                        span.text,
                        Style {
                            bold: true,
                            ..span.style
                        },
                    )
                } else {
                    span
                };
                spans.push(span);
            }
            if column < last {
                spans.push(Span::raw(
                    " ".repeat(widths[column].saturating_sub(width) + GAP),
                ));
            }
        }
        lines.push(Line { spans });
        if is_header {
            // Underline each header cell only as far as its own text: rules
            // sized to full column widths overflow the viewport on wide cells
            // and wrap into a wall of dashes.
            let rule = cell_widths
                .iter()
                .enumerate()
                .map(|(column, cell_width)| {
                    let pad = widths
                        .get(column)
                        .map_or(0, |width| width.saturating_sub(*cell_width));
                    format!("{}{}", "─".repeat(*cell_width), " ".repeat(pad))
                })
                .collect::<Vec<_>>()
                .join(&" ".repeat(GAP));
            lines.push(styled_line(rule.trim_end(), dim_style()));
        }
    }
    lines
}

/// If `line` is an ATX heading (`#`..`######` + space), return its level and text.
fn heading_text(line: &str) -> Option<(usize, &str)> {
    let hashes = line.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) && line[hashes..].starts_with(' ') {
        Some((hashes, line[hashes..].trim_start()))
    } else {
        None
    }
}

fn is_horizontal_rule(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= 3
        && (trimmed.chars().all(|c| c == '-')
            || trimmed.chars().all(|c| c == '*')
            || trimmed.chars().all(|c| c == '_'))
}

/// A Markdown table delimiter row, e.g. `|---|:--:|` or `--- | ---`.
fn is_table_separator(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.contains('-')
        && trimmed.chars().all(|c| matches!(c, '-' | '|' | ':' | ' '))
        && trimmed.chars().any(|c| c == '|')
}

/// If `line` begins a bullet (`- ` or `* ` after optional indent), return the
/// indent width and the marker length (always 2: the glyph plus its space).
fn bullet_prefix(line: &str) -> Option<(usize, usize)> {
    let indent = line.len() - line.trim_start().len();
    let rest = &line[indent..];
    if rest.starts_with("- ") || rest.starts_with("* ") {
        Some((indent, 2))
    } else {
        None
    }
}

/// Parse inline Markdown emphasis/code in `text` into styled spans.
fn inline_spans(text: &str, base: Style) -> Vec<Span> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans: Vec<Span> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        // Inline code: `code`
        if c == '`'
            && let Some(end) = find_char(&chars, i + 1, '`')
        {
            push_buf(&mut buf, &mut spans, base);
            spans.push(Span::styled(
                chars[i + 1..end].iter().collect::<String>(),
                code_style(),
            ));
            i = end + 1;
            continue;
        }
        // Bold: **text**
        if c == '*'
            && i + 1 < chars.len()
            && chars[i + 1] == '*'
            && let Some(end) = find_seq(&chars, i + 2, '*')
        {
            push_buf(&mut buf, &mut spans, base);
            spans.push(Span::styled(
                chars[i + 2..end].iter().collect::<String>(),
                Style { bold: true, ..base },
            ));
            i = end + 2;
            continue;
        }
        // Italic: *text* or _text_
        if (c == '*' || c == '_')
            && let Some(end) = find_char(&chars, i + 1, c)
            && end > i + 1
        {
            push_buf(&mut buf, &mut spans, base);
            spans.push(Span::styled(
                chars[i + 1..end].iter().collect::<String>(),
                Style {
                    italic: true,
                    ..base
                },
            ));
            i = end + 1;
            continue;
        }
        buf.push(c);
        i += 1;
    }

    push_buf(&mut buf, &mut spans, base);
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base));
    }
    spans
}

fn push_buf(buf: &mut String, spans: &mut Vec<Span>, style: Style) {
    if !buf.is_empty() {
        spans.push(Span::styled(std::mem::take(buf), style));
    }
}

fn find_char(chars: &[char], from: usize, target: char) -> Option<usize> {
    (from..chars.len()).find(|&i| chars[i] == target)
}

/// Find the next `target target` pair (e.g. `**`) starting at `from`.
fn find_seq(chars: &[char], from: usize, target: char) -> Option<usize> {
    (from..chars.len().saturating_sub(1)).find(|&i| chars[i] == target && chars[i + 1] == target)
}

fn code_style() -> Style {
    fg_style(LIGHT_GREEN)
}

/// Plain-text projection of an entry for `transcript_text` (tests, non-TTY).
fn entry_plain_lines(entry: &ChatEntry) -> Vec<String> {
    match entry {
        ChatEntry::User(text) => vec![format!("you: {text}")],
        ChatEntry::Assistant(text) => vec![text.clone()],
        ChatEntry::Thinking(text) => vec![format!("thinking: {text}")],
        ChatEntry::System(text) => vec![text.clone()],
        ChatEntry::Tool(tool) => {
            let glyph = match tool.status {
                ToolStatus::Running => "→",
                ToolStatus::Ok => "✓",
                ToolStatus::Failed => "✗",
            };
            let mut line = format!("{glyph} {}", tool.name);
            if !tool.args.is_empty() {
                line.push(' ');
                line.push_str(&tool.args);
            }
            if let Some(summary) = &tool.summary {
                line.push_str(" — ");
                line.push_str(summary);
            }
            vec![line]
        }
    }
}

/// Compact one-line rendering of tool-call arguments for a card; empty
/// objects collapse to nothing and long blobs are clipped.
fn compact_json_args(value: &serde_json::Value) -> String {
    if value.as_object().is_some_and(|map| map.is_empty()) || value.is_null() {
        return String::new();
    }
    clip_text(&value.to_string(), 80)
}

/// The tool name shown on a card: the canonical `file.*`/`shell.exec` form
/// when the runtime accepts the model's alias, else the alias verbatim.
fn display_tool_name(name: &str) -> String {
    crate::runtime::canonical_tool_name(name).unwrap_or_else(|| name.to_string())
}

fn tool_result_summary(result: &crate::runtime::ToolBatchResult) -> Option<String> {
    if !result.ok {
        let error = result.error.clone().unwrap_or_else(|| "failed".to_string());
        return Some(clip_text(&error, 100));
    }
    let mut non_empty = result
        .content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let first = non_empty.next()?;
    // A single first line misrepresents list-shaped results (a 200-entry
    // directory listing looked like one folder), so surface the line count.
    let rest = non_empty.count();
    if rest == 0 {
        Some(clip_text(first, 100))
    } else {
        Some(format!("{} lines · {}", rest + 1, clip_text(first, 80)))
    }
}

fn clip_text(text: &str, max: usize) -> String {
    let flattened = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flattened.chars().count() > max {
        let clipped: String = flattened.chars().take(max.saturating_sub(1)).collect();
        format!("{clipped}…")
    } else {
        flattened
    }
}
