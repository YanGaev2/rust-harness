//! Pure chat-session state machine on the `harness-tui` library.
//!
//! Input events go in ([`ChatApp::handle_event`]), [`ChatAction`]s and
//! whole screen frames come out ([`ChatApp::frame`]). The chat runs full
//! screen: the transcript scrolls inside the frame (wheel, PgUp/PgDn),
//! the editor and status rows stay on the bottom edge, and a mouse drag
//! selects transcript text for the clipboard. Rendered rows of finished
//! entries are cached per entry; only entries that still change (a
//! streaming answer, a running tool, live reasoning) render every frame.
//! Streamed text is revealed at a steady pace with a soft fade-in.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use harness_tui::components::editor::Editor;
use harness_tui::components::menu::{MenuItem, menu_lines};
use harness_tui::components::reveal::{Reveal, fade_in};
use harness_tui::components::spinner::{FRAMES, format_elapsed, working_line};
use harness_tui::components::status::status_line;
use harness_tui::input::{Event, KeyCode, KeyEvent, MouseEvent, MouseKind};
use harness_tui::text::{
    Line, Span, Style, columns_text, highlight, truncate, visible_width, wrap_hanging,
};
use harness_tui::theme::Theme;

use crate::provider_wizard::{ProviderWizard, WizardAction};
use crate::providers::{ModelDiscovery, ModelPricing, ProviderConfig};

/// What the event loop should do after the app handled an input event.
#[derive(Debug, Clone, PartialEq)]
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
    /// A mouse selection ended: put this text on the system clipboard.
    Copy(String),
    /// `/provider add`: fetch this provider's model list, then call
    /// `ChatApp::models_loaded`.
    FetchModels(ProviderConfig),
    /// `/provider add` finished: save the provider and switch to `model`.
    SaveProvider {
        provider: ProviderConfig,
        model: String,
    },
    Exit,
}

/// What to do with an input event that arrives while the agent is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusyAction {
    /// Interrupt the run.
    Cancel,
    /// Hand the event to the app: the user keeps composing (and queueing)
    /// the next message while the agent works.
    Forward,
}

const EDITOR_PROMPT: &str = "› ";
const EDITOR_PLACEHOLDER: &str = "Ask anything · / for commands";
const EDITOR_PLACEHOLDER_BUSY: &str = "Queue a follow-up · Esc to interrupt";
/// Maximum editor rows shown in the pinned panel.
const EDITOR_MAX_ROWS: usize = 6;
/// Maximum completion-menu rows shown in the pinned panel.
const COMPLETION_MAX_ROWS: usize = 6;

/// Status note for a stream that stopped producing events while the run is
/// still alive. Providers that buffer whole tool calls (baseten/Kimi) go
/// legitimately silent for minutes; the note tells the user the harness is
/// still listening. `None` under 15s so gaps between deltas never flicker.
pub fn stream_idle_note(idle_seconds: u64) -> Option<String> {
    (idle_seconds >= 15).then(|| {
        format!("stream silent {idle_seconds}s — model may still be generating; Esc interrupts")
    })
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
        usage: "/model [N | MODEL | PROVIDER MODEL]",
        description: "list models or switch (new names are saved)",
    },
    ChatCommand {
        name: "/provider",
        usage: "/provider [add]",
        description: "list saved providers, or add one from its /models list",
    },
    ChatCommand {
        name: "/paste",
        usage: "/paste",
        description: "capture the system clipboard (text or image)",
    },
    ChatCommand {
        name: "/copy",
        usage: "/copy",
        description: "copy the last answer as raw markdown",
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
        name: "/cost",
        usage: "/cost",
        description: "show session token usage and estimated cost",
    },
    ChatCommand {
        name: "/thinking",
        usage: "/thinking",
        description: "show or hide reasoning (Ctrl+O expands it)",
    },
    ChatCommand {
        name: "/retain-thinking",
        usage: "/retain-thinking",
        description: "toggle replaying reasoning back to the model",
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

/// One row of the autocomplete menu: what Tab inserts plus the hint columns.
/// Owned (not `&'static ChatCommand`) because model-argument rows are built
/// from the runtime catalog.
struct CompletionItem {
    insert: String,
    usage: String,
    description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolStatus {
    Running,
    Ok,
    Failed,
    /// The run ended (Esc, error) before this call reported a result.
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolEntry {
    id: String,
    name: String,
    /// Compact JSON of the arguments (plain projection).
    args: String,
    /// What a human scans for: the command, query, path or URL.
    headline: String,
    arguments: serde_json::Value,
    status: ToolStatus,
    started: Instant,
    elapsed: Option<Duration>,
    /// One-line result (plain projection; the card's fallback detail).
    summary: Option<String>,
    detail: ToolDetail,
}

/// Result preview drawn under a tool card.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ToolDetail {
    /// Only the one-line summary.
    Summary,
    /// The newest output rows of a command and how many earlier rows were
    /// cut; a non-zero exit code is shown under them.
    Output {
        tail: Vec<String>,
        hidden: usize,
        exit_code: Option<i64>,
    },
    /// An edit: the replaced text and its replacement.
    Diff {
        removed: Vec<String>,
        added: Vec<String>,
    },
}

/// A reasoning block: a `∴ Thinking… Ns` header while it streams
/// (`elapsed` unset), `∴ Thought for Ns` once closed, over its newest
/// rows — or all rows while Ctrl+O expands reasoning.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ThinkingEntry {
    text: String,
    started: Instant,
    elapsed: Option<Duration>,
    reveal: Reveal,
}

/// A single block in the transcript. Tool calls are a structured entry so a
/// streamed `ToolResult` can update the matching call in place (Running →
/// Ok/Failed) instead of appending a disconnected line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ChatEntry {
    User(String),
    /// The startup banner: version, active model, workspace.
    Banner {
        version: String,
        model: String,
        workspace: String,
    },
    /// Streamed assistant output and how much of it is revealed.
    Assistant {
        text: String,
        reveal: Reveal,
    },
    Thinking(ThinkingEntry),
    Tool(ToolEntry),
    System(String),
}

/// Where the transcript sat in the last frame: screen row 0 shows
/// transcript row `top`, and the transcript fills `rows` screen rows.
/// Maps mouse rows to transcript rows.
#[derive(Debug, Clone, Copy, Default)]
struct View {
    top: usize,
    rows: usize,
}

/// A mouse selection in transcript coordinates — (row, column), both
/// ends inclusive. `dragging` until the button is released.
#[derive(Debug, Clone, Copy)]
struct Selection {
    anchor: (usize, usize),
    head: (usize, usize),
    dragging: bool,
}

impl Selection {
    /// The two ends in reading order.
    fn ordered(&self) -> ((usize, usize), (usize, usize)) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }
}

/// An 8-bit-per-channel color as terminals report it.
type Rgb = (u8, u8, u8);

/// How long a status notice (`copied 42 chars`) stays visible.
const NOTICE_TTL: Duration = Duration::from_secs(2);
/// Rows the mouse wheel scrolls per notch.
const WHEEL_ROWS: usize = 3;
/// One spinner frame, and one sweep of the `Working…` shimmer.
const SPINNER_FRAME_MS: u128 = 80;
const SHIMMER_PERIOD_MS: u128 = 1800;

/// The chat session state machine: a transcript of user turns, tool
/// activity, and streamed assistant output, plus a prompt editor. Pure —
/// input events go in, [`ChatAction`]s and whole screen frames come out.
pub struct ChatApp {
    provider_label: String,
    workspace: PathBuf,
    theme: Theme,
    /// Current git branch of the workspace for the footer; refreshed
    /// after every run (the agent may switch branches).
    git_branch: Option<String>,
    editor: Editor,
    transcript: Vec<ChatEntry>,
    /// Rendered rows of settled entries, by transcript index, for
    /// `cache_width` columns. Unsettled entries are `None`.
    cache: Vec<Option<Vec<Line>>>,
    cache_width: usize,
    /// Transcript rows hidden below the screen; 0 follows the newest row.
    scroll: usize,
    /// Transcript height in the last frame, to keep a scrolled view
    /// still while new rows arrive below it.
    last_total: usize,
    view: View,
    selection: Option<Selection>,
    /// A short status message and when it was set.
    notice: Option<(String, Instant)>,
    streaming_assistant: bool,
    streaming_thinking: bool,
    /// Ctrl+O: show every reasoning row instead of the newest few.
    thinking_expanded: bool,
    /// `/thinking`: leave reasoning out of the transcript.
    thinking_hidden: bool,
    /// Name the keys in rendered hints (`ctrl+o to expand`); off for the
    /// transcript printed on exit, where the keys no longer work.
    key_hints: bool,
    /// Pace streamed text and fade it in (the REPL turns this on; tests
    /// see every text at once).
    animations: bool,
    /// The terminal's (text, background) colors: the fade-in blends
    /// between them.
    terminal_colors: Option<(Rgb, Rgb)>,
    /// Animation clock: `now` is the frame time set by `animate`.
    clock: Instant,
    now: Instant,
    retain_thinking: bool,
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
    /// Last agent event of the current run: feeds the "stream silent Ns"
    /// note, so a provider silently buffering a big tool call (baseten/Kimi)
    /// is distinguishable from a frozen run.
    stream_activity: Option<Instant>,
    /// Cumulative token usage across the whole REPL session (all runs),
    /// updated from `AgentEvent::UsageUpdated`; shown by `/cost`.
    session_usage: crate::agent::UsageTotals,
    /// Usage at the start of the current run, so per-run deltas fold into
    /// the session total without double-counting.
    run_usage_base: crate::agent::UsageTotals,
    /// The active model's context window, once the provider reported it.
    context_window: Option<u64>,
    /// Size of the conversation the next request carries (the latest
    /// request's prompt + completion); unknown until a provider reports
    /// usage in this session.
    context_tokens: Option<u64>,
    /// Saved providers and their models, for the `/model` menu, shorthand
    /// switching, and argument completions. Empty when the host never
    /// attached one (tests, bare sessions) — `/model` then shows usage only.
    catalog: Vec<(String, Vec<String>)>,
    /// Interactive model picker opened by bare `/model` (arrow keys +
    /// type-to-filter, like the reference agents' model selectors).
    model_picker: Option<ModelPicker>,
    provider_wizard: Option<ProviderWizard>,
    /// Prices of the active model (saved from `/models` or built in).
    pricing: Option<ModelPricing>,
    /// True after a first Esc press: the status row asks for a second
    /// Esc to confirm the exit; any other input disarms it.
    exit_armed: bool,
    /// Collapsed pastes awaiting submit: the editor shows only the
    /// `[Pasted text #N …]` placeholder, the full text lives here.
    pasted_blocks: Vec<PastedBlock>,
    /// Monotonic paste counter feeding the `#N` in placeholders.
    paste_seq: usize,
    /// Prompts submitted while the agent was busy, oldest first.
    queued: VecDeque<QueuedPrompt>,
}

/// A prompt submitted while the agent was busy; it is sent once the run
/// ends, or handed back to the editor if the run is interrupted.
struct QueuedPrompt {
    text: String,
    blocks: Vec<PastedBlock>,
}

/// A collapsed paste: the placeholder inserted into the editor and the
/// full text substituted back in when the message is submitted.
struct PastedBlock {
    placeholder: String,
    text: String,
}

/// Pastes at or above either bound collapse into a placeholder instead
/// of flooding the editor (the reference-harness `[Pasted …]` UX).
const PASTE_COLLAPSE_MIN_LINES: usize = 3;
const PASTE_COLLAPSE_MIN_CHARS: usize = 700;

/// State of the interactive `/model` picker: the full pair list, the cursor
/// position inside the *filtered* view, and the type-to-filter text.
struct ModelPicker {
    pairs: Vec<(String, String)>,
    cursor: usize,
    filter: String,
}

impl ModelPicker {
    /// Pairs surviving the current filter (case-insensitive substring match
    /// against the provider, the model, or `provider/model`).
    fn filtered(&self) -> Vec<(String, String)> {
        if self.filter.is_empty() {
            return self.pairs.clone();
        }
        let needle = self.filter.to_ascii_lowercase();
        self.pairs
            .iter()
            .filter(|(provider, model)| {
                format!("{provider}/{model}")
                    .to_ascii_lowercase()
                    .contains(&needle)
            })
            .cloned()
            .collect()
    }
}

impl ChatApp {
    pub fn new(provider_label: impl Into<String>, workspace: impl Into<PathBuf>) -> Self {
        let workspace = workspace.into();
        let theme = Theme::default();
        let mut editor = Editor::new(EDITOR_PROMPT, EDITOR_PLACEHOLDER);
        editor.set_styles(Style::fg(theme.accent), Style::fg(theme.muted));
        let now = Instant::now();
        Self {
            provider_label: provider_label.into(),
            git_branch: git_branch(&workspace),
            workspace,
            theme,
            editor,
            transcript: Vec::new(),
            cache: Vec::new(),
            cache_width: 0,
            scroll: 0,
            last_total: 0,
            view: View::default(),
            selection: None,
            notice: None,
            streaming_assistant: false,
            streaming_thinking: false,
            thinking_expanded: false,
            thinking_hidden: false,
            key_hints: true,
            animations: false,
            terminal_colors: None,
            clock: now,
            now,
            // On by default: the thinking models this harness targets
            // (DeepSeek V4 / Kimi / MiMo) require the reasoning echo, and it
            // is what lets an interrupted turn resume with its thinking.
            retain_thinking: true,
            history: Vec::new(),
            history_cursor: None,
            help_visible: false,
            completion_index: 0,
            completion_dismissed: false,
            busy: false,
            busy_since: None,
            stream_activity: None,
            session_usage: crate::agent::UsageTotals::default(),
            run_usage_base: crate::agent::UsageTotals::default(),
            context_window: None,
            context_tokens: None,
            catalog: Vec::new(),
            model_picker: None,
            provider_wizard: None,
            pricing: None,
            exit_armed: false,
            pasted_blocks: Vec::new(),
            paste_seq: 0,
            queued: VecDeque::new(),
        }
    }

    /// Whether the interactive `/model` picker is open.
    pub fn model_picker_visible(&self) -> bool {
        self.model_picker.is_some()
    }

    /// Render with `theme` (picked by the host from the terminal's
    /// background and color depth).
    pub fn with_theme(mut self, theme: Theme) -> Self {
        self.theme = theme;
        self.editor
            .set_styles(Style::fg(theme.accent), Style::fg(theme.muted));
        self
    }

    /// Pace streamed text instead of showing every delta at once; the
    /// host drives the clock with [`ChatApp::animate`].
    pub fn with_animations(mut self, on: bool) -> Self {
        self.animations = on;
        self
    }

    /// The terminal's text and background colors (OSC 10/11). With both
    /// known, newly revealed text fades in from the background.
    pub fn with_terminal_colors(
        mut self,
        foreground: Option<Rgb>,
        background: Option<Rgb>,
    ) -> Self {
        self.terminal_colors = foreground.zip(background);
        self
    }

    /// Advance animations to `now`: streamed text reveals, the spinner
    /// and the shimmer move, an old notice expires.
    pub fn animate(&mut self, now: Instant) {
        self.now = now;
        if self
            .notice
            .as_ref()
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) >= NOTICE_TTL)
        {
            self.notice = None;
        }
        if !self.animations {
            return;
        }
        for entry in &mut self.transcript {
            match entry {
                ChatEntry::Assistant { text, reveal }
                | ChatEntry::Thinking(ThinkingEntry { text, reveal, .. })
                    if !reveal.is_done(text) =>
                {
                    reveal.advance(text, now);
                }
                _ => {}
            }
        }
    }

    /// True while frames still change without input: a run is going, text
    /// is being revealed, or a notice waits to expire. The host polls
    /// faster then.
    pub fn is_animating(&self) -> bool {
        if self.busy || self.notice.is_some() {
            return true;
        }
        self.animations
            && self.transcript.iter().any(|entry| match entry {
                ChatEntry::Assistant { text, reveal }
                | ChatEntry::Thinking(ThinkingEntry { text, reveal, .. }) => {
                    reveal.is_animating(text, self.now)
                }
                _ => false,
            })
    }

    /// Show all text at once: resumed history, or the transcript printed
    /// on exit.
    pub fn finish_reveals(&mut self) {
        for entry in &mut self.transcript {
            if let ChatEntry::Assistant { text, reveal }
            | ChatEntry::Thinking(ThinkingEntry { text, reveal, .. }) = entry
            {
                reveal.finish(text);
            }
        }
    }

    /// Push the startup banner (version, active model, workspace) into the
    /// transcript as its first entry.
    pub fn push_banner(&mut self, version: &str) {
        self.transcript.push(ChatEntry::Banner {
            version: version.to_string(),
            model: self.provider_label.clone(),
            workspace: display_path(&self.workspace),
        });
    }

    /// Attach the saved provider/model catalog that `/model` and `/provider`
    /// menus are built from.
    pub fn with_catalog(mut self, catalog: Vec<(String, Vec<String>)>) -> Self {
        self.catalog = catalog;
        self
    }

    /// Record a model that was added to a provider mid-session (e.g. the
    /// user switched to a brand-new model name) so menus and completions
    /// pick it up immediately.
    pub fn add_catalog_model(&mut self, provider: &str, model: &str) {
        if let Some((_, models)) = self.catalog.iter_mut().find(|(name, _)| name == provider) {
            if !models.iter().any(|existing| existing == model) {
                models.push(model.to_string());
            }
        } else {
            self.catalog
                .push((provider.to_string(), vec![model.to_string()]));
        }
    }

    /// Flat `[provider, model]` pairs in menu order — the numbering shown by
    /// `/model` and accepted by `/model N`.
    fn model_menu_pairs(&self) -> Vec<(String, String)> {
        self.catalog
            .iter()
            .flat_map(|(provider, models)| {
                models
                    .iter()
                    .map(move |model| (provider.clone(), model.clone()))
            })
            .collect()
    }

    /// The provider half of the active `provider/model` label.
    fn active_provider_name(&self) -> &str {
        self.provider_label
            .split_once('/')
            .map(|(provider, _)| provider)
            .unwrap_or(&self.provider_label)
    }

    /// The current compose text.
    pub fn input(&self) -> &str {
        self.editor.text()
    }

    /// Mark the agent as running (or finished) so the view can show a spinner.
    pub fn set_busy(&mut self, busy: bool) {
        if busy {
            // UsageUpdated events carry per-run cumulative totals; remember
            // where this run starts so the session counter never double-counts.
            self.run_usage_base = self.session_usage;
        }
        self.busy = busy;
        self.busy_since = busy.then(Instant::now);
        self.stream_activity = busy.then(Instant::now);
        self.editor.set_placeholder(if busy {
            EDITOR_PLACEHOLDER_BUSY
        } else {
            EDITOR_PLACEHOLDER
        });
        if !busy {
            self.close_thinking();
            self.cancel_running_tools();
            // No more deltas: the last answer settles and its rows cache.
            self.streaming_assistant = false;
            self.streaming_thinking = false;
            self.git_branch = git_branch(&self.workspace);
        }
    }

    /// Input policy while the agent runs: Esc or Ctrl+C cancel the run —
    /// except that Esc first closes an open menu or palette — and every
    /// other event goes to the editor.
    pub fn busy_action(&self, event: &Event) -> BusyAction {
        match event {
            Event::Key(key) if key.code == KeyCode::Esc && self.overlay_open() => {
                BusyAction::Forward
            }
            Event::Key(key) if key.code == KeyCode::Esc => BusyAction::Cancel,
            Event::Key(key) if key.mods.ctrl && key.code == KeyCode::Char('c') => {
                BusyAction::Cancel
            }
            _ => BusyAction::Forward,
        }
    }

    fn overlay_open(&self) -> bool {
        self.help_visible
            || self.model_picker.is_some()
            || self.provider_wizard.is_some()
            || self.completion_visible()
    }

    /// How many prompts wait for the current run to finish.
    pub fn queued_len(&self) -> usize {
        self.queued.len()
    }

    /// The next queued prompt once the run is over, as the action its
    /// submit produces (a message to send or a command to run).
    pub fn take_queued(&mut self) -> Option<ChatAction> {
        if self.busy {
            return None;
        }
        let prompt = self.queued.pop_front()?;
        Some(self.submit_text(prompt.text, prompt.blocks))
    }

    /// An interrupted or failed run hands its queued prompts back to the
    /// editor, so the user can edit or drop them instead of having them
    /// fire automatically.
    pub fn restore_queued_to_editor(&mut self) {
        if self.queued.is_empty() {
            return;
        }
        let mut parts = Vec::new();
        for prompt in std::mem::take(&mut self.queued) {
            parts.push(prompt.text);
            self.pasted_blocks.extend(prompt.blocks);
        }
        if !self.editor.is_empty() {
            parts.push(self.editor.text().to_string());
        }
        self.editor.set_text(parts.join("\n"));
    }

    /// Whole seconds the current run has been going (0 when idle).
    pub fn busy_elapsed_secs(&self) -> u64 {
        self.busy_since
            .map(|since| since.elapsed().as_secs())
            .unwrap_or(0)
    }

    /// Whole seconds since the current run last produced an agent event
    /// (0 when idle). Streams that flow keep this near zero; a provider
    /// buffering a whole tool call lets it climb.
    pub fn stream_idle_secs(&self) -> u64 {
        if !self.busy {
            return 0;
        }
        self.stream_activity
            .map(|at| at.elapsed().as_secs())
            .unwrap_or(0)
    }

    /// Whether the `/`-autocomplete menu is currently shown.
    pub fn completion_visible(&self) -> bool {
        !self.completion_dismissed && !self.help_visible && !self.completion_matches().is_empty()
    }

    /// Insert-texts currently offered by the autocomplete menu.
    pub fn completion_suggestions(&self) -> Vec<String> {
        self.completion_matches()
            .iter()
            .map(|item| item.insert.clone())
            .collect()
    }

    /// The text the autocomplete menu would accept on Tab.
    pub fn completion_selected(&self) -> Option<String> {
        let matches = self.completion_matches();
        if matches.is_empty() {
            return None;
        }
        let index = self.completion_index.min(matches.len() - 1);
        Some(matches[index].insert.clone())
    }

    /// Whether the command palette / help overlay is open.
    pub fn help_visible(&self) -> bool {
        self.help_visible
    }

    /// Update the status-row label after an in-session model switch.
    pub fn set_provider_label(&mut self, label: impl Into<String>) {
        self.provider_label = label.into();
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

    /// Route one terminal event: keys, pastes, the wheel and the mouse.
    pub fn handle_event(&mut self, event: Event) -> ChatAction {
        match event {
            Event::Key(key) => self.handle_key(key),
            Event::Paste(text) => self.handle_paste(&text),
            Event::WheelUp => {
                self.scroll_up(WHEEL_ROWS);
                ChatAction::Continue
            }
            Event::WheelDown => {
                self.scroll_down(WHEEL_ROWS);
                ChatAction::Continue
            }
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            Event::Resize(..) => ChatAction::Continue,
        }
    }

    fn scroll_up(&mut self, rows: usize) {
        // `frame` clamps the offset to the transcript height.
        self.scroll = self.scroll.saturating_add(rows);
    }

    fn scroll_down(&mut self, rows: usize) {
        self.scroll = self.scroll.saturating_sub(rows);
    }

    /// One screen of transcript rows minus an overlap row for context.
    fn page_rows(&self) -> usize {
        self.view.rows.saturating_sub(2).max(1)
    }

    /// Left drag selects transcript text and copies it on release; the
    /// right button pastes, as it does in the terminal without capture.
    fn handle_mouse(&mut self, mouse: MouseEvent) -> ChatAction {
        let row = mouse.row as usize;
        let column = mouse.column as usize;
        match mouse.kind {
            MouseKind::RightDown => ChatAction::CaptureClipboard,
            MouseKind::Down => {
                self.selection = (row < self.view.rows).then(|| {
                    let point = (self.view.top + row, column);
                    Selection {
                        anchor: point,
                        head: point,
                        dragging: true,
                    }
                });
                ChatAction::Continue
            }
            MouseKind::Drag => {
                if !self.selection.is_some_and(|selection| selection.dragging) {
                    return ChatAction::Continue;
                }
                // Dragging onto the top or bottom edge scrolls one row.
                if row == 0 && self.view.top > 0 {
                    self.scroll_up(1);
                } else if row + 1 >= self.view.rows {
                    self.scroll_down(1);
                }
                let row = row.min(self.view.rows.saturating_sub(1));
                if let Some(selection) = self.selection.as_mut() {
                    selection.head = (self.view.top + row, column);
                }
                ChatAction::Continue
            }
            MouseKind::Up => {
                let Some(mut selection) = self.selection.filter(|s| s.dragging) else {
                    return ChatAction::Continue;
                };
                selection.dragging = false;
                if selection.anchor == selection.head {
                    // A plain click clears the selection.
                    self.selection = None;
                    return ChatAction::Continue;
                }
                self.selection = Some(selection);
                let text = self.selection_text(selection);
                if text.is_empty() {
                    return ChatAction::Continue;
                }
                self.set_notice(format!("copied {} chars", text.chars().count()));
                ChatAction::Copy(text)
            }
        }
    }

    fn set_notice(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), self.now));
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ChatAction {
        // Any key other than Esc drops the pending exit confirmation.
        if key.code != KeyCode::Esc {
            self.exit_armed = false;
        }
        // Typing ends a finished mouse selection.
        if self.selection.is_some_and(|selection| !selection.dragging) {
            self.selection = None;
        }

        // Ctrl+C always exits, even with the palette open.
        if key.mods.ctrl && key.code == KeyCode::Char('c') {
            return ChatAction::Exit;
        }

        // Ctrl+O expands every reasoning block in place, or folds them back.
        if key.mods.ctrl && key.code == KeyCode::Char('o') {
            self.thinking_expanded = !self.thinking_expanded;
            self.invalidate_cache();
            self.set_notice(if self.thinking_expanded {
                "reasoning expanded · ctrl+o folds it"
            } else {
                "reasoning folded to the newest lines"
            });
            return ChatAction::Continue;
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

        // The provider wizard owns the keyboard while it is open; Esc closes it.
        if let Some(wizard) = &mut self.provider_wizard {
            if key.code == KeyCode::Esc {
                self.provider_wizard = None;
                return ChatAction::Continue;
            }
            return match wizard.handle_key(key) {
                WizardAction::Continue => ChatAction::Continue,
                WizardAction::Fetch(provider) => ChatAction::FetchModels(provider),
                WizardAction::Save { provider, model } => {
                    self.provider_wizard = None;
                    ChatAction::SaveProvider { provider, model }
                }
            };
        }

        // The interactive model picker owns the keyboard while it is open.
        if self.model_picker.is_some() {
            return self.handle_picker_key(key);
        }

        let completion_open = self.completion_visible();

        match key.code {
            KeyCode::Esc => {
                if completion_open {
                    // The menu absorbs this Esc; it neither arms nor
                    // confirms the exit.
                    self.completion_dismissed = true;
                    self.exit_armed = false;
                    ChatAction::Continue
                } else if self.exit_armed {
                    ChatAction::Exit
                } else {
                    // Two-step exit: a stray Esc must not kill the
                    // session — the status row asks for a second press.
                    self.exit_armed = true;
                    ChatAction::Continue
                }
            }
            // A modified Enter composes a newline instead of submitting.
            // Alt+Enter works everywhere; Shift+Enter needs the kitty
            // keyboard protocol (Windows Terminal 1.25+, WezTerm, kitty);
            // Ctrl+Enter arrives as LF = Ctrl+J on Windows Terminal 1.24.
            KeyCode::Enter if key.mods.alt || key.mods.shift || key.mods.ctrl => {
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
            KeyCode::Backspace if key.mods.ctrl || key.mods.alt => {
                self.editor.delete_word_back();
                self.reset_completion();
                ChatAction::Continue
            }
            KeyCode::Char('w') if key.mods.ctrl => {
                self.editor.delete_word_back();
                self.reset_completion();
                ChatAction::Continue
            }
            KeyCode::Backspace => {
                self.editor.backspace();
                self.reset_completion();
                ChatAction::Continue
            }
            KeyCode::Delete => {
                self.editor.delete_forward();
                self.reset_completion();
                ChatAction::Continue
            }
            KeyCode::Char('u') if key.mods.ctrl => {
                self.editor.delete_to_line_start();
                self.reset_completion();
                ChatAction::Continue
            }
            KeyCode::Char('k') if key.mods.ctrl => {
                self.editor.delete_to_line_end();
                self.reset_completion();
                ChatAction::Continue
            }
            KeyCode::Left if key.mods.ctrl || key.mods.alt => {
                self.editor.move_word_left();
                ChatAction::Continue
            }
            KeyCode::Right if key.mods.ctrl || key.mods.alt => {
                self.editor.move_word_right();
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
            KeyCode::Char('a') if key.mods.ctrl => {
                self.editor.move_home();
                ChatAction::Continue
            }
            KeyCode::End => {
                self.editor.move_end();
                ChatAction::Continue
            }
            KeyCode::Char('e') if key.mods.ctrl => {
                self.editor.move_end();
                ChatAction::Continue
            }
            // Up/Down move between the lines of a multi-line prompt and
            // recall history only from its first/last line.
            KeyCode::Up => {
                if completion_open {
                    self.completion_index = self.completion_index.saturating_sub(1);
                } else if !self.editor.move_up() {
                    self.recall_previous();
                }
                ChatAction::Continue
            }
            KeyCode::Down => {
                if completion_open {
                    let last = self.completion_matches().len().saturating_sub(1);
                    self.completion_index = (self.completion_index + 1).min(last);
                } else if !self.editor.move_down() {
                    self.recall_next();
                }
                ChatAction::Continue
            }
            KeyCode::PageUp => {
                self.scroll_up(self.page_rows());
                ChatAction::Continue
            }
            KeyCode::PageDown => {
                self.scroll_down(self.page_rows());
                ChatAction::Continue
            }
            KeyCode::Char(ch) if !key.mods.ctrl => {
                self.editor.insert_char(ch);
                self.reset_completion();
                ChatAction::Continue
            }
            _ => ChatAction::Continue,
        }
    }

    pub fn handle_paste(&mut self, text: &str) -> ChatAction {
        // A chat message may legitimately span lines, so a small paste is
        // inserted verbatim at the caret; it never submits because it is a
        // single event. Large pastes collapse to a placeholder.
        self.exit_armed = false;
        if let Some(wizard) = &mut self.provider_wizard {
            wizard.handle_paste(text);
            return ChatAction::Continue;
        }
        self.insert_text_block(text);
        self.reset_completion();
        ChatAction::Continue
    }

    /// Insert clipboard text at the caret (used by the Ctrl+V capture path).
    pub fn apply_clipboard_text(&mut self, prompt_text: &str) {
        self.insert_text_block(prompt_text);
        self.reset_completion();
    }

    /// Insert pasted/clipboard text at the caret. Text at or above the
    /// collapse bounds becomes a `[Pasted text #N +L lines]` placeholder —
    /// the editor stays readable and the full text is swapped back in by
    /// `submit_input` (the transcript keeps the compact marker).
    fn insert_text_block(&mut self, text: &str) {
        let line_count = text.lines().count();
        let char_count = text.chars().count();
        if line_count < PASTE_COLLAPSE_MIN_LINES && char_count < PASTE_COLLAPSE_MIN_CHARS {
            self.editor.insert_str(text);
            return;
        }
        self.paste_seq += 1;
        let placeholder = if line_count > 1 {
            format!("[Pasted text #{} +{line_count} lines]", self.paste_seq)
        } else {
            format!("[Pasted text #{} +{char_count} chars]", self.paste_seq)
        };
        self.editor.insert_str(&placeholder);
        self.pasted_blocks.push(PastedBlock {
            placeholder,
            text: text.to_string(),
        });
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

    /// The candidates for the current input: command names while a bare
    /// `/command` token is being typed, and provider/model pairs once the
    /// input is `/model ...` — that argument menu is what makes models
    /// discoverable without leaving the compose line.
    fn completion_matches(&self) -> Vec<CompletionItem> {
        let text = self.editor.text();
        if !text.starts_with('/') {
            return Vec::new();
        }
        if let Some(rest) = text.strip_prefix("/model ") {
            let needle = rest.trim().to_ascii_lowercase();
            return self
                .model_menu_pairs()
                .into_iter()
                .filter(|(provider, model)| {
                    needle.is_empty()
                        || provider.to_ascii_lowercase().starts_with(&needle)
                        || model.to_ascii_lowercase().starts_with(&needle)
                        || format!("{provider} {model}")
                            .to_ascii_lowercase()
                            .starts_with(&needle)
                })
                .map(|(provider, model)| {
                    let active = format!("{provider}/{model}") == self.provider_label;
                    CompletionItem {
                        insert: format!("/model {provider} {model}"),
                        usage: format!("{provider}/{model}"),
                        description: if active {
                            "active".to_string()
                        } else {
                            String::new()
                        },
                    }
                })
                .collect();
        }
        if text.chars().any(char::is_whitespace) {
            return Vec::new();
        }
        let query = text.to_ascii_lowercase();
        CHAT_COMMANDS
            .iter()
            .filter(|command| command.name.starts_with(&query))
            .map(|command| CompletionItem {
                insert: command.name.to_string(),
                usage: command.usage.to_string(),
                description: command.description.to_string(),
            })
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
        // Collapsed pastes belong to this compose; blocks whose
        // placeholder was edited away are simply dropped.
        let blocks = std::mem::take(&mut self.pasted_blocks);
        if text.is_empty() {
            return ChatAction::Continue;
        }
        if self.busy && !runs_while_busy(&text) {
            self.queued.push_back(QueuedPrompt { text, blocks });
            return ChatAction::Continue;
        }
        self.submit_text(text, blocks)
    }

    /// Run a submitted prompt: a slash command, or a message for the model.
    fn submit_text(&mut self, text: String, blocks: Vec<PastedBlock>) -> ChatAction {
        if let Some(command) = text.strip_prefix('/') {
            return self.run_slash_command(command);
        }
        // The transcript and history keep the compact placeholder; the
        // model receives the fully expanded message.
        self.history.push(text.clone());
        self.push_user_message(&text);
        // Sending jumps back to the newest rows.
        self.scroll = 0;
        ChatAction::Submit(expand_pasted_blocks(&text, &blocks))
    }

    fn run_slash_command(&mut self, command: &str) -> ChatAction {
        let mut parts = command.split_whitespace();
        match parts.next() {
            Some("help") | Some("?") => {
                self.help_visible = true;
                ChatAction::Continue
            }
            Some("clear") => {
                self.clear_transcript();
                ChatAction::Continue
            }
            Some("new") => {
                self.clear_transcript();
                // The fresh session starts with an empty conversation.
                self.context_tokens = None;
                ChatAction::NewSession
            }
            Some("exit") | Some("quit") => ChatAction::Exit,
            // Terminals often bind Ctrl+V themselves (Windows Terminal eats it
            // entirely for images), so the capture needs a key-free route.
            Some("paste") => ChatAction::CaptureClipboard,
            // The source text, not the rendered rows: wrapped code and
            // table cards would not survive a copy from the screen.
            Some("copy") => {
                let answer = self.transcript.iter().rev().find_map(|entry| match entry {
                    ChatEntry::Assistant { text, .. } => Some(text.clone()),
                    _ => None,
                });
                match answer {
                    Some(text) => {
                        self.set_notice(format!("copied {} chars", text.chars().count()));
                        ChatAction::Copy(text)
                    }
                    None => {
                        self.set_notice("nothing to copy yet");
                        ChatAction::Continue
                    }
                }
            }
            Some("thinking") => {
                self.thinking_hidden = !self.thinking_hidden;
                self.invalidate_cache();
                self.push_system_line(if self.thinking_hidden {
                    "thinking: hidden — reasoning is left out of the transcript"
                } else {
                    "thinking: shown — the newest lines of each block (Ctrl+O expands)"
                });
                ChatAction::Continue
            }
            Some("retain-thinking") => {
                self.retain_thinking = !self.retain_thinking;
                let status = if self.retain_thinking {
                    "retain thinking: on — reasoning is replayed to the model (session-wide)"
                } else {
                    "retain thinking: off — reasoning stays in traces only"
                };
                self.push_system_line(status);
                ChatAction::Continue
            }
            Some("provider") if parts.next() == Some("add") => {
                self.provider_wizard = Some(ProviderWizard::new());
                ChatAction::Continue
            }
            Some("provider") => {
                let label = self.provider_label.clone();
                self.push_system_line(format!("active provider: {label}"));
                for (provider, models) in self.catalog.clone() {
                    self.transcript.push(ChatEntry::System(format!(
                        "  {provider}: {}",
                        models.join(", ")
                    )));
                }
                ChatAction::Continue
            }
            Some("cost") => {
                let usage = self.session_usage;
                self.push_system_line(format!(
                    "session usage: {} request(s), prompt {} tok ({} cached), completion {} tok",
                    usage.requests,
                    usage.prompt_tokens,
                    usage.cached_tokens,
                    usage.completion_tokens
                ));
                // The label is "provider/model"; the model half may itself
                // contain slashes, so split at the first one only.
                let estimate = self.pricing.clone().or_else(|| {
                    self.provider_label
                        .split_once('/')
                        .and_then(|(provider, model)| {
                            crate::providers::builtin_pricing(provider, model)
                        })
                });
                match estimate {
                    Some(pricing) => {
                        let cost = pricing.estimate_usd(
                            usage.prompt_tokens,
                            usage.cached_tokens,
                            usage.completion_tokens,
                        );
                        self.push_system_line(format!(
                            "estimated cost: ${cost:.4} (price list of {}; actual billing may differ)",
                            pricing.as_of
                        ));
                    }
                    None => {
                        self.push_system_line("estimated cost: no pricing known for this model");
                    }
                }
                ChatAction::Continue
            }
            Some("model") => {
                let first = parts.next();
                let second = parts.next();
                match (first, second) {
                    (Some(provider), Some(model)) if parts.next().is_none() => {
                        ChatAction::SwitchModel {
                            provider: provider.to_string(),
                            model: model.to_string(),
                        }
                    }
                    (Some(arg), None) => self.switch_model_shorthand(arg),
                    (None, None) => {
                        self.open_model_picker();
                        ChatAction::Continue
                    }
                    _ => {
                        self.push_system_line("usage: /model [N | MODEL | PROVIDER MODEL]");
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

    /// Open the interactive model picker (bare `/model`): arrow keys move,
    /// Enter switches, Esc closes, typing filters. The cursor starts on the
    /// active pair so Enter with no navigation is a harmless no-op switch.
    fn open_model_picker(&mut self) {
        let pairs = self.model_menu_pairs();
        if pairs.is_empty() {
            self.push_system_line(
                "usage: /model [N | MODEL | PROVIDER MODEL] (no saved providers)",
            );
            return;
        }
        let active = self.provider_label.clone();
        let cursor = pairs
            .iter()
            .position(|(provider, model)| format!("{provider}/{model}") == active)
            .unwrap_or(0);
        self.model_picker = Some(ModelPicker {
            pairs,
            cursor,
            filter: String::new(),
        });
    }

    /// Keyboard handling while the model picker is open.
    fn handle_picker_key(&mut self, key: KeyEvent) -> ChatAction {
        match key.code {
            KeyCode::Esc => {
                self.model_picker = None;
                ChatAction::Continue
            }
            KeyCode::Enter => {
                let choice = self.model_picker.take().and_then(|picker| {
                    let filtered = picker.filtered();
                    if filtered.is_empty() {
                        return None;
                    }
                    let index = picker.cursor.min(filtered.len() - 1);
                    Some(filtered[index].clone())
                });
                match choice {
                    Some((provider, model)) => ChatAction::SwitchModel { provider, model },
                    None => ChatAction::Continue,
                }
            }
            _ => {
                if let Some(picker) = self.model_picker.as_mut() {
                    match key.code {
                        KeyCode::Up => picker.cursor = picker.cursor.saturating_sub(1),
                        KeyCode::Down => {
                            let last = picker.filtered().len().saturating_sub(1);
                            picker.cursor = (picker.cursor + 1).min(last);
                        }
                        KeyCode::Backspace => {
                            picker.filter.pop();
                            picker.cursor = 0;
                        }
                        KeyCode::Char(ch) if !key.mods.ctrl => {
                            picker.filter.push(ch);
                            picker.cursor = 0;
                        }
                        _ => {}
                    }
                }
                ChatAction::Continue
            }
        }
    }

    /// `/model ARG` with a single token: menu number, provider name, model
    /// name, or a brand-new model for the active provider.
    fn switch_model_shorthand(&mut self, arg: &str) -> ChatAction {
        if self.catalog.is_empty() && arg.parse::<usize>().is_err() {
            self.push_system_line("usage: /model [N | MODEL | PROVIDER MODEL]");
            return ChatAction::Continue;
        }
        let active = self.active_provider_name().to_string();
        match crate::providers::resolve_model_arg(&self.catalog, &active, arg) {
            Ok((provider, model)) => ChatAction::SwitchModel { provider, model },
            Err(err) => {
                self.push_system_line(err);
                ChatAction::Continue
            }
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

    /// A run that ended while calls were still running never reports their
    /// results: mark those cards cancelled so they stop spinning.
    fn cancel_running_tools(&mut self) {
        for entry in &mut self.transcript {
            if let ChatEntry::Tool(tool) = entry
                && tool.status == ToolStatus::Running
            {
                tool.status = ToolStatus::Cancelled;
                tool.elapsed = Some(tool.started.elapsed());
                tool.summary = Some("interrupted".to_string());
            }
        }
    }

    /// Stop the clock of a reasoning block that is still streaming: it is
    /// final from now on and renders as its collapsed summary.
    fn close_thinking(&mut self) {
        if let Some(ChatEntry::Thinking(thinking)) = self.transcript.last_mut()
            && thinking.elapsed.is_none()
        {
            thinking.elapsed = Some(thinking.started.elapsed());
        }
    }

    /// Drop the whole transcript (`/clear`, `/new`) with its cached rows,
    /// scroll position and selection.
    fn clear_transcript(&mut self) {
        self.transcript.clear();
        self.cache.clear();
        self.scroll = 0;
        self.last_total = 0;
        self.selection = None;
    }

    /// Forget every cached row: a display toggle changed how entries render.
    fn invalidate_cache(&mut self) {
        self.cache.clear();
        self.selection = None;
    }

    /// Current `/retain-thinking` state; the REPL passes it to each run's
    /// `AgentRunner`, so a mid-session toggle applies from the next turn.
    pub fn retain_thinking(&self) -> bool {
        self.retain_thinking
    }

    pub fn push_user_message(&mut self, message: &str) {
        self.close_thinking();
        self.transcript.push(ChatEntry::User(message.to_string()));
        self.streaming_assistant = false;
        self.streaming_thinking = false;
    }

    /// Append a system/status line (welcome banner, error notice, etc.).
    pub fn push_system_line(&mut self, text: impl Into<String>) {
        self.close_thinking();
        self.transcript.push(ChatEntry::System(text.into()));
        self.streaming_assistant = false;
        self.streaming_thinking = false;
    }

    /// Fold a streamed agent event into the transcript so the view updates as
    /// the agent works through tool rounds and the final answer.
    pub fn push_agent_event(&mut self, event: &crate::agent::AgentEvent) {
        use crate::agent::AgentEvent;
        self.stream_activity = Some(Instant::now());
        // Usage ticks arrive between deltas and must not break a growing
        // streaming block, so they bypass the flag reset below.
        if let AgentEvent::UsageUpdated(totals) = event {
            self.session_usage = self.run_usage_base.combined(*totals);
            if totals.context_tokens > 0 {
                self.context_tokens = Some(totals.context_tokens);
            }
            return;
        }
        // Every other event ends any live streaming block; the two streaming
        // arms re-arm their own flag.
        let was_thinking = std::mem::take(&mut self.streaming_thinking);
        let was_assistant = std::mem::take(&mut self.streaming_assistant);
        if was_thinking && !matches!(event, AgentEvent::Thinking(_)) {
            self.close_thinking();
        }
        match event {
            AgentEvent::Thinking(text) => {
                // Coalesce streamed reasoning fragments into one growing block.
                if was_thinking
                    && let Some(ChatEntry::Thinking(existing)) = self.transcript.last_mut()
                {
                    existing.text.push_str(text);
                } else {
                    self.transcript.push(ChatEntry::Thinking(ThinkingEntry {
                        text: text.clone(),
                        started: Instant::now(),
                        elapsed: None,
                        reveal: Reveal::new(),
                    }));
                }
                self.streaming_thinking = true;
            }
            AgentEvent::UsageUpdated(_) => unreachable!("handled above"),
            AgentEvent::ToolRoundStarted { round, tool_calls } => {
                // A single-call round would just duplicate its tool card; only
                // multi-call rounds earn a banner line.
                if *tool_calls > 1 {
                    self.transcript.push(ChatEntry::System(format!(
                        "tool round {round}: {tool_calls} call(s)"
                    )));
                }
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
                    headline: tool_headline(arguments),
                    arguments: arguments.clone(),
                    status: ToolStatus::Running,
                    started: Instant::now(),
                    elapsed: None,
                    summary: None,
                    detail: ToolDetail::Summary,
                }));
            }
            AgentEvent::ToolResult(result) => {
                self.apply_tool_result(result);
            }
            AgentEvent::ProviderSwitched {
                from_provider,
                from_model,
                to_provider,
                to_model,
                reason,
            } => {
                self.transcript.push(ChatEntry::System(format!(
                    "⚠ provider switch: {from_provider}/{from_model} → {to_provider}/{to_model} ({reason})"
                )));
            }
            AgentEvent::FinalContentDelta(delta) => {
                match (was_assistant, self.transcript.last_mut()) {
                    (true, Some(ChatEntry::Assistant { text, .. })) => {
                        text.push_str(delta);
                    }
                    _ => {
                        self.transcript.push(ChatEntry::Assistant {
                            text: delta.clone(),
                            reveal: Reveal::new(),
                        });
                    }
                }
                self.streaming_assistant = true;
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
                tool.elapsed = Some(tool.started.elapsed());
                tool.summary = tool_result_summary(&tool.name, &tool.arguments, result);
                tool.detail = tool_detail(&tool.name, &tool.arguments, result);
            }
            None => {
                let name = display_tool_name(&result.tool_name);
                let arguments = serde_json::Value::Null;
                self.transcript.push(ChatEntry::Tool(ToolEntry {
                    id: result.id.clone(),
                    summary: tool_result_summary(&name, &arguments, result),
                    detail: tool_detail(&name, &arguments, result),
                    name,
                    args: String::new(),
                    headline: String::new(),
                    arguments,
                    status,
                    started: Instant::now(),
                    elapsed: None,
                }));
            }
        }
        // Surface the self-correction memo so the user sees how the next call
        // should be shaped (the "forgiving tools" feedback loop).
        if let Some(hint) = &result.hint {
            self.transcript
                .push(ChatEntry::System(format!("memo: {hint}")));
        }
    }

    /// The whole screen, `height` rows: the transcript scrolled to the
    /// current position (blank rows below short content), then the panel
    /// docked to the bottom edge — busy row, palette, queued prompts, the
    /// editor between two rules, menus and the status row.
    pub fn frame(&mut self, width: usize, height: usize) -> Vec<Line> {
        let width = width.max(1);
        let height = height.max(1);
        let fresh = self.refresh(width);
        let (mut panel, mut rule_at) = self.panel_rows(width);
        // Keep at least one transcript row: a panel taller than the screen
        // loses its top rows (the editor and status row matter most).
        let max_panel = height.saturating_sub(1).max(1);
        if panel.len() > max_panel {
            let cut = panel.len() - max_panel;
            panel.drain(..cut);
            rule_at = rule_at.and_then(|at| at.checked_sub(cut));
        }
        let area = height.saturating_sub(panel.len());

        let total = self.total_rows(&fresh);
        // A scrolled view stays still while rows arrive below it.
        if self.scroll > 0 && total > self.last_total {
            self.scroll += total - self.last_total;
        }
        self.last_total = total;
        self.scroll = self.scroll.min(total.saturating_sub(area));
        let end = total - self.scroll;
        let start = end.saturating_sub(area);
        self.view = View {
            top: start,
            rows: area,
        };

        let mut rows = self.rows_between(&fresh, start, end);
        self.paint_selection(&mut rows, start);
        rows.resize(area, Line::default());
        if self.scroll > 0
            && let Some(at) = rule_at
        {
            panel[at] = scroll_rule(width, self.scroll, &self.theme);
        }
        rows.extend(panel);
        rows
    }

    /// Every transcript row at `width`, a blank row after each entry —
    /// what the REPL prints into the terminal's scrollback on exit.
    pub fn transcript_lines(&mut self, width: usize) -> Vec<Line> {
        let width = width.max(1);
        let fresh = self.refresh(width);
        let total = self.total_rows(&fresh);
        self.rows_between(&fresh, 0, total)
    }

    /// The transcript as the terminal keeps it after exit: every text
    /// fully revealed, no key hints (the keys stop working once the chat
    /// closes).
    pub fn exit_transcript(&mut self, width: usize) -> Vec<Line> {
        self.finish_reveals();
        self.key_hints = false;
        self.invalidate_cache();
        let lines = self.transcript_lines(width);
        self.key_hints = true;
        self.invalidate_cache();
        lines
    }

    /// Bring the row cache up to date for `width` and render the entries
    /// that still change. Returns those fresh rows by transcript index.
    fn refresh(&mut self, width: usize) -> Vec<(usize, Vec<Line>)> {
        if width != self.cache_width {
            self.cache.clear();
            self.cache_width = width;
            self.selection = None;
        }
        self.cache.resize(self.transcript.len(), None);
        let mut fresh = Vec::new();
        for index in 0..self.transcript.len() {
            if self.cache[index].is_some() {
                continue;
            }
            let rows = self.render_entry(&self.transcript[index], width);
            if self.is_settled(index) {
                self.cache[index] = Some(rows);
            } else {
                fresh.push((index, rows));
            }
        }
        fresh
    }

    /// A settled entry renders the same until a display toggle changes:
    /// its rows can be cached. Running tools, live reasoning, the
    /// streaming answer and text still being revealed are not settled.
    fn is_settled(&self, index: usize) -> bool {
        let revealing =
            |text: &str, reveal: &Reveal| self.animations && reveal.is_animating(text, self.now);
        match &self.transcript[index] {
            ChatEntry::Tool(tool) => tool.status != ToolStatus::Running,
            ChatEntry::Thinking(thinking) => {
                thinking.elapsed.is_some() && !revealing(&thinking.text, &thinking.reveal)
            }
            ChatEntry::Assistant { text, reveal } => {
                let streaming = self.streaming_assistant && index + 1 == self.transcript.len();
                !streaming && !revealing(text, reveal)
            }
            _ => true,
        }
    }

    /// The rows of entry `index`: cached, or rendered this frame.
    fn entry_rows<'a>(&'a self, fresh: &'a [(usize, Vec<Line>)], index: usize) -> &'a [Line] {
        match self.cache.get(index) {
            Some(Some(rows)) => rows,
            _ => fresh
                .iter()
                .find(|(at, _)| *at == index)
                .map_or(&[][..], |(_, rows)| rows.as_slice()),
        }
    }

    /// Transcript height: each entry that shows anything, plus the blank
    /// row after it.
    fn total_rows(&self, fresh: &[(usize, Vec<Line>)]) -> usize {
        (0..self.transcript.len())
            .map(|index| match self.entry_rows(fresh, index).len() {
                0 => 0,
                rows => rows + 1,
            })
            .sum()
    }

    /// Transcript rows `start..end`, cloned out of the entry blocks.
    fn rows_between(&self, fresh: &[(usize, Vec<Line>)], start: usize, end: usize) -> Vec<Line> {
        let mut rows = Vec::with_capacity(end.saturating_sub(start));
        let mut offset = 0usize;
        for index in 0..self.transcript.len() {
            if offset >= end {
                break;
            }
            let block = self.entry_rows(fresh, index);
            if block.is_empty() {
                continue;
            }
            let height = block.len() + 1;
            if offset + height > start {
                for row in start.max(offset)..end.min(offset + height) {
                    rows.push(block.get(row - offset).cloned().unwrap_or_default());
                }
            }
            offset += height;
        }
        rows
    }

    /// Paint the selection highlight over transcript rows that start at
    /// transcript row `first`.
    fn paint_selection(&self, rows: &mut [Line], first: usize) {
        let Some(selection) = self.selection else {
            return;
        };
        let ((row0, col0), (row1, col1)) = selection.ordered();
        let background = self.theme.selection;
        for (offset, line) in rows.iter_mut().enumerate() {
            let row = first + offset;
            if row < row0 || row > row1 {
                continue;
            }
            let from = if row == row0 { col0 } else { 0 };
            let to = if row == row1 { col1 + 1 } else { usize::MAX };
            *line = highlight(line, from, to, |style| style.bg(background));
        }
    }

    /// The selected text: whole rows between the ends, partial rows at
    /// the ends, trailing spaces trimmed (strips and padding are spaces).
    fn selection_text(&mut self, selection: Selection) -> String {
        let ((row0, col0), (row1, col1)) = selection.ordered();
        let fresh = self.refresh(self.cache_width.max(1));
        let rows = self.rows_between(&fresh, row0, row1 + 1);
        rows.iter()
            .enumerate()
            .map(|(offset, line)| {
                let row = row0 + offset;
                let from = if row == row0 { col0 } else { 0 };
                let to = if row == row1 { col1 + 1 } else { usize::MAX };
                columns_text(line, from, to).trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// One entry's rows at `width` for the current display state.
    fn render_entry(&self, entry: &ChatEntry, width: usize) -> Vec<Line> {
        let theme = &self.theme;
        match entry {
            ChatEntry::User(text) => user_lines(text, width, theme),
            ChatEntry::Banner {
                version,
                model,
                workspace,
            } => wrap_rows(banner_lines(version, model, workspace, theme), width),
            ChatEntry::Assistant { text, reveal } => {
                let visible = self.visible(text, reveal);
                if visible.trim().is_empty() {
                    return Vec::new();
                }
                // The rows sit two columns in, under the indent added here.
                let rows = markdown_lines(visible.trim_end(), width.saturating_sub(2), theme)
                    .into_iter()
                    .map(|line| indent_line(line, "  "))
                    .collect();
                let mut rows = wrap_rows(rows, width);
                self.fade(&mut rows, text, reveal);
                rows
            }
            ChatEntry::Thinking(_) if self.thinking_hidden => Vec::new(),
            ChatEntry::Thinking(thinking) => {
                let visible = self.visible(&thinking.text, &thinking.reveal);
                let mut rows = thinking_lines(
                    thinking,
                    visible,
                    self.thinking_expanded,
                    self.key_hints,
                    width,
                    theme,
                );
                // The header is not new text; only the body fades in.
                self.fade(&mut rows[1..], &thinking.text, &thinking.reveal);
                rows
            }
            ChatEntry::System(text) => {
                let style = if text.starts_with("memo:") {
                    Style::fg(theme.accent)
                } else {
                    Style::fg(theme.muted)
                };
                let row = Line {
                    spans: vec![Span::styled("· ", style), Span::styled(text.clone(), style)],
                };
                wrap_rows(vec![row], width)
            }
            ChatEntry::Tool(tool) => tool_card_lines(tool, width, theme, self.spinner_frame()),
        }
    }

    /// The revealed part of a streamed text (all of it without animations).
    fn visible<'a>(&self, text: &'a str, reveal: &Reveal) -> &'a str {
        if self.animations {
            &text[..reveal.shown().min(text.len())]
        } else {
            text
        }
    }

    /// Fade in the newest revealed characters of `rows`.
    fn fade(&self, rows: &mut [Line], text: &str, reveal: &Reveal) {
        let Some((foreground, background)) = self.terminal_colors.filter(|_| self.animations)
        else {
            return;
        };
        let count = reveal.fade_len(text, self.now);
        if count > 0 {
            fade_in(rows, count, background, foreground);
        }
    }

    /// Spinner frame for the animation clock.
    fn spinner_frame(&self) -> usize {
        (self.now.saturating_duration_since(self.clock).as_millis() / SPINNER_FRAME_MS) as usize
    }

    /// The panel docked to the bottom edge, and the index of the rule
    /// above the editor (it shows the scroll position when scrolled).
    fn panel_rows(&self, width: usize) -> (Vec<Line>, Option<usize>) {
        let mut lines = Vec::new();
        if self.busy {
            let elapsed = self.now.saturating_duration_since(self.clock).as_millis();
            let phase = (elapsed % SHIMMER_PERIOD_MS) as f32 / SHIMMER_PERIOD_MS as f32;
            lines.push(working_line(
                &self.theme,
                self.spinner_frame(),
                &self.busy_detail(),
                phase,
            ));
            if let Some(note) = stream_idle_note(self.stream_idle_secs()) {
                lines.push(Line::styled(
                    format!("   {note}"),
                    Style::fg(self.theme.muted),
                ));
            }
        }

        if self.help_visible {
            lines.extend(palette_lines(&self.theme));
        }

        for prompt in &self.queued {
            let row = Line {
                spans: vec![
                    Span::styled(" ⏵ queued ", Style::fg(self.theme.accent)),
                    Span::styled(
                        crate::repl::summarize_one_line(&prompt.text, width),
                        Style::fg(self.theme.muted),
                    ),
                ],
            };
            lines.push(truncate(&row, width));
        }

        let rule_at = lines.len();
        let editor_rows = self.editor.render(width, EDITOR_MAX_ROWS);
        lines.extend(ruled_lines(editor_rows, width, &self.theme));

        if let Some(wizard) = &self.provider_wizard {
            lines.extend(wizard.lines(&self.theme));
        } else if let Some(picker) = &self.model_picker {
            lines.extend(model_picker_lines(
                picker,
                &self.provider_label,
                &self.theme,
            ));
        } else if self.completion_visible() {
            let items: Vec<MenuItem> = self
                .completion_matches()
                .into_iter()
                .map(|item| MenuItem {
                    name: item.insert,
                    usage: item.usage,
                    description: item.description,
                })
                .collect();
            let selected = self.completion_index.min(items.len().saturating_sub(1));
            lines.extend(menu_lines(
                &self.theme,
                &items,
                self.editor.text(),
                selected,
                COMPLETION_MAX_ROWS,
            ));
        }

        lines.push(self.status_row(width));
        (lines, Some(rule_at))
    }

    /// Detail shown in the busy spinner: elapsed time, output tokens of
    /// this run once the provider reported usage, and the interrupt key.
    fn busy_detail(&self) -> String {
        let mut detail = format_elapsed(self.busy_elapsed_secs());
        let produced = self
            .session_usage
            .completion_tokens
            .saturating_sub(self.run_usage_base.completion_tokens);
        if produced > 0 {
            detail.push_str(&format!(" · ↓ {} tokens", compact_count(produced)));
        }
        detail.push_str(" · esc to interrupt");
        detail
    }

    /// The active model's context window in tokens (`None` = unknown);
    /// the host learns it from the provider's model list.
    pub fn set_context_window(&mut self, window: Option<u64>) {
        self.context_window = window;
    }

    /// Prices of the active model for `/cost` (`None` = fall back to the
    /// built-in price list).
    pub fn set_pricing(&mut self, pricing: Option<ModelPricing>) {
        self.pricing = pricing;
    }

    pub fn provider_wizard_open(&self) -> bool {
        self.provider_wizard.is_some()
    }

    /// Hand the model list result to the open provider wizard (dropped
    /// when the wizard was closed meanwhile).
    pub fn models_loaded(&mut self, result: Result<(ProviderConfig, ModelDiscovery), String>) {
        if let Some(wizard) = &mut self.provider_wizard {
            wizard.models_loaded(result);
        }
    }

    /// Bottom status row. Left: where you are (workspace and git branch),
    /// replaced by the one hint that matters right now (exit confirmation,
    /// picker/menu keys). Right: the active model, session token usage and
    /// the context meter. Narrow rows drop the session usage first, then
    /// the model name; the meter stays longest.
    fn status_row(&self, width: usize) -> Line {
        let muted = Style::fg(self.theme.muted);
        let left = if self.exit_armed {
            Line::styled(" Press Esc again to exit", Style::fg(self.theme.warning))
        } else if let Some((notice, _)) = &self.notice {
            Line::styled(format!(" {notice}"), Style::fg(self.theme.accent))
        } else if self.model_picker.is_some() {
            Line::styled(
                " ↑↓ select · Enter switch · type to filter · Esc close",
                muted,
            )
        } else if self.completion_visible() {
            Line::styled(" ↑↓ select · Tab complete · Esc close", muted)
        } else {
            let mut location = format!(" {}", display_path(&self.workspace));
            if let Some(branch) = &self.git_branch {
                location.push_str(&format!(" ({branch})"));
            }
            Line::styled(location, muted)
        };
        let model = Span::styled(self.provider_label.clone(), muted);
        let usage = self
            .usage_summary()
            .map(|usage| Span::styled(format!(" · {usage}"), muted));
        let meter = self
            .context_tokens
            .map(|tokens| context_meter(tokens, self.context_window, &self.theme));
        let row = |parts: Vec<Span>| {
            let mut spans = parts;
            spans.push(Span::raw(" "));
            Line { spans }
        };
        let mut candidates = Vec::new();
        let mut full = vec![model.clone()];
        full.extend(usage);
        if let Some(meter) = &meter {
            full.push(Span::styled(" · ", muted));
            full.extend(meter.iter().cloned());
            candidates.push(row(full));
            let mut short = vec![model.clone(), Span::styled(" · ", muted)];
            short.extend(meter.iter().cloned());
            candidates.push(row(short));
            candidates.push(row(meter.clone()));
        } else {
            candidates.push(row(full));
            candidates.push(row(vec![model]));
        }
        let fits = |right: &Line| left.width() + right.width() + 2 <= width;
        let right = candidates
            .iter()
            .find(|right| fits(right))
            .or(candidates.last())
            .cloned()
            .unwrap_or_default();
        status_line(width, left, right)
    }

    /// `↑12.3k ↓3.4k · cache 85%` for the session so far; `None` before the
    /// first provider response.
    fn usage_summary(&self) -> Option<String> {
        let usage = self.session_usage;
        if usage.requests == 0 {
            return None;
        }
        let mut summary = format!(
            "↑{} ↓{}",
            compact_count(usage.prompt_tokens),
            compact_count(usage.completion_tokens)
        );
        if usage.cached_tokens > 0 && usage.prompt_tokens > 0 {
            let percent = usage.cached_tokens * 100 / usage.prompt_tokens;
            summary.push_str(&format!(" · cache {percent}%"));
        }
        Some(summary)
    }
}

/// Cells of the context bar.
const CONTEXT_BAR_CELLS: u64 = 10;
/// Partial cells, in eighths: index 1 is ⅛ of a cell, 7 is ⅞.
const BAR_EIGHTHS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];

/// `████▌░░░░░ 474k/1M 45%`: how full the context window is. The bar fills
/// in eighths of a cell, in the success color, then the warning color from
/// 60%, the error color from 85%. Without a known window: `ctx 45.2k`.
fn context_meter(tokens: u64, window: Option<u64>, theme: &Theme) -> Vec<Span> {
    let muted = Style::fg(theme.muted);
    let Some(window) = window.filter(|window| *window > 0) else {
        return vec![Span::styled(
            format!("ctx {}", compact_tokens(tokens)),
            muted,
        )];
    };
    let percent = tokens.saturating_mul(100) / window;
    let color = Style::fg(match percent {
        0..=59 => theme.success,
        60..=84 => theme.warning,
        _ => theme.error,
    });
    let eighths = (u128::from(tokens.min(window)) * u128::from(CONTEXT_BAR_CELLS * 8)
        / u128::from(window)) as u64;
    let mut filled = "█".repeat((eighths / 8) as usize);
    filled.push_str(BAR_EIGHTHS[(eighths % 8) as usize]);
    let used = eighths.div_ceil(8);
    vec![
        Span::styled(filled, color),
        Span::styled(
            "░".repeat((CONTEXT_BAR_CELLS - used) as usize),
            Style::fg(theme.faint),
        ),
        Span::styled(
            format!(" {}/{} ", compact_tokens(tokens), compact_tokens(window)),
            muted,
        ),
        Span::styled(format!("{percent}%"), color),
    ]
}

/// Token counts for the context meter: `950`, `45.2k`, `474k`, `1M`,
/// `1.5M`.
fn compact_tokens(value: u64) -> String {
    let one_decimal = |value: f64| {
        let text = format!("{value:.1}");
        text.strip_suffix(".0").map(str::to_string).unwrap_or(text)
    };
    match value {
        0..=999 => value.to_string(),
        1_000..=99_999 => format!("{}k", one_decimal(value as f64 / 1_000.0)),
        100_000..=999_999 => format!("{}k", value / 1_000),
        _ => format!("{}M", one_decimal(value as f64 / 1_000_000.0)),
    }
}

/// `950`, `12.3k`, `1.2M`.
fn compact_count(value: u64) -> String {
    match value {
        0..=999 => value.to_string(),
        1_000..=999_999 => format!("{:.1}k", value as f64 / 1_000.0),
        _ => format!("{:.1}M", value as f64 / 1_000_000.0),
    }
}

/// The workspace path with the home directory shortened to `~`.
fn display_path(path: &Path) -> String {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    if let Some(home) = home
        && let Ok(rest) = path.strip_prefix(&home)
    {
        let rest = rest.display().to_string();
        return if rest.is_empty() {
            "~".to_string()
        } else {
            format!("~{}{rest}", std::path::MAIN_SEPARATOR)
        };
    }
    path.display().to_string()
}

/// The checked-out branch of the repository containing `workspace`, read
/// straight from `.git/HEAD` (no git process): `main`, or the short commit
/// id when HEAD is detached. Worktrees (`.git` file with `gitdir:`) work.
fn git_branch(workspace: &Path) -> Option<String> {
    let dot_git = workspace
        .ancestors()
        .map(|dir| dir.join(".git"))
        .find(|candidate| candidate.exists())?;
    let git_dir = if dot_git.is_file() {
        let pointer = std::fs::read_to_string(&dot_git).ok()?;
        let target = pointer.trim().strip_prefix("gitdir:")?.trim();
        dot_git.parent()?.join(target)
    } else {
        dot_git
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref: ") {
        Some(reference) => Some(
            reference
                .strip_prefix("refs/heads/")
                .unwrap_or(reference)
                .to_string(),
        ),
        None => Some(head.chars().take(7).collect()),
    }
}

/// Slash commands that only read or toggle local state run at once even
/// while the agent works; everything else waits in the queue.
fn runs_while_busy(text: &str) -> bool {
    let Some(command) = text.strip_prefix('/') else {
        return false;
    };
    let mut words = command.split_whitespace();
    // `/provider add` fetches and saves: it waits for the run like a prompt.
    if words.next() == Some("provider") {
        return words.next().is_none();
    }
    matches!(
        command.split_whitespace().next(),
        Some("help" | "?" | "cost" | "history" | "thinking" | "retain-thinking" | "paste" | "copy")
    )
}

/// Swap each collapsed paste back into the submitted message. Placeholders
/// the user edited or deleted simply do not match and their block is lost —
/// what the user sees in the editor is what gets sent.
fn expand_pasted_blocks(text: &str, blocks: &[PastedBlock]) -> String {
    let mut expanded = text.to_string();
    for block in blocks {
        expanded = expanded.replacen(&block.placeholder, &block.text, 1);
    }
    expanded
}

/// The startup banner: name and version, the active model and workspace,
/// and the three keys a newcomer needs. Pushed into the transcript, so it
/// flows into native scrollback like the rest of the conversation.
fn banner_lines(version: &str, model: &str, workspace: &str, theme: &Theme) -> Vec<Line> {
    let muted = Style::fg(theme.muted);
    vec![
        Line {
            spans: vec![
                Span::styled("harness", Style::fg(theme.accent).bold()),
                Span::styled(format!(" v{version}"), muted),
            ],
        },
        Line::styled(format!("{model} · {workspace}"), muted),
        Line::styled("/ commands · Alt+Enter newline · Esc Esc exit", muted),
    ]
}

/// Maximum picker rows shown at once; the window scrolls to keep the cursor
/// visible and a `(K/N)` counter reports the position in the full list.
const PICKER_MAX_ROWS: usize = 10;

/// Render the interactive model picker: `›` and the accent color mark the
/// cursor row, `[provider]` is muted, `✓` marks the active pair, and the
/// footer shows `(K/N)` plus the type-to-filter text.
fn model_picker_lines(picker: &ModelPicker, active_label: &str, theme: &Theme) -> Vec<Line> {
    let muted = Style::fg(theme.muted);
    let filtered = picker.filtered();
    if filtered.is_empty() {
        return vec![Line::styled(
            format!("   no models match '{}' (Esc to close)", picker.filter),
            muted,
        )];
    }
    let cursor = picker.cursor.min(filtered.len() - 1);
    let start = (cursor + 1).saturating_sub(PICKER_MAX_ROWS);
    let mut lines = Vec::new();
    for (index, (provider, model)) in filtered
        .iter()
        .enumerate()
        .skip(start)
        .take(PICKER_MAX_ROWS)
    {
        let (marker, name_style) = if index == cursor {
            (" › ", Style::fg(theme.accent))
        } else {
            ("   ", Style::default())
        };
        let mut spans = vec![
            Span::styled(marker, Style::fg(theme.accent)),
            Span::styled(format!("{model} "), name_style),
            Span::styled(format!("[{provider}]"), muted),
        ];
        if format!("{provider}/{model}") == active_label {
            spans.push(Span::styled(" ✓", Style::fg(theme.success)));
        }
        lines.push(Line { spans });
    }
    let filter_note = if picker.filter.is_empty() {
        String::new()
    } else {
        format!(" · {}", picker.filter)
    };
    lines.push(Line::styled(
        format!("   ({}/{}){filter_note}", cursor + 1, filtered.len()),
        muted,
    ));
    lines
}

/// The editor between two full-width rules — no side borders, so a mouse
/// selection copies only the typed text.
fn ruled_lines(inner: Vec<Line>, width: usize, theme: &Theme) -> Vec<Line> {
    let rule = Line::styled("─".repeat(width.max(1)), Style::fg(theme.faint));
    let mut lines = Vec::with_capacity(inner.len() + 2);
    lines.push(rule.clone());
    lines.extend(inner);
    lines.push(rule);
    lines
}

/// The `/help` palette as plain panel lines.
fn palette_lines(theme: &Theme) -> Vec<Line> {
    let muted = Style::fg(theme.muted);
    let mut lines = vec![Line::styled(" Commands", Style::default().bold())];
    for command in CHAT_COMMANDS {
        lines.push(Line {
            spans: vec![
                Span::raw(format!("   {:<22}", command.usage)),
                Span::styled(command.description, muted),
            ],
        });
    }
    lines.push(Line::styled(
        " Alt/Shift/Ctrl+Enter newline · Up/Down recall · PgUp/PgDn scroll · drag to copy · Ctrl+O reasoning · Esc close",
        muted,
    ));
    lines
}

/// Wrap styled rows to `width` with the chat's hanging indent.
fn wrap_rows(rows: Vec<Line>, width: usize) -> Vec<Line> {
    rows.into_iter()
        .flat_map(|line| wrap_styled_line(line, width))
        .collect()
}

/// The rule above the editor while the view is scrolled up: how many
/// rows are below the screen and the key that brings them back.
fn scroll_rule(width: usize, below: usize, theme: &Theme) -> Line {
    let label = format!(" ↓ {below} more rows · PgDn ");
    let label_width = visible_width(&label);
    if label_width + 4 > width {
        return Line::styled("─".repeat(width.max(1)), Style::fg(theme.accent));
    }
    let faint = Style::fg(theme.faint);
    Line {
        spans: vec![
            Span::styled("─".repeat(width - label_width - 2), faint),
            Span::styled(label, Style::fg(theme.accent)),
            Span::styled("──", faint),
        ],
    }
}

/// Reasoning rows kept visible under the header until Ctrl+O expands them.
const THINKING_TAIL_ROWS: usize = 4;

/// A reasoning block: `∴ Thinking… Ns` while it streams, `∴ Thought for
/// Ns` once closed, over its newest rows — all rows when `expanded`. The
/// header counts the folded rows and, with `hints`, names the key that
/// shows them.
fn thinking_lines(
    thinking: &ThinkingEntry,
    visible: &str,
    expanded: bool,
    hints: bool,
    width: usize,
    theme: &Theme,
) -> Vec<Line> {
    let muted = Style::fg(theme.muted);
    let visible = visible.trim();
    let mut body: Vec<Line> = if visible.is_empty() {
        Vec::new()
    } else {
        visible
            .split('\n')
            .map(|raw| Line {
                spans: vec![Span::raw("  "), Span::styled(raw, muted)],
            })
            .flat_map(|line| wrap_styled_line(line, width))
            .collect()
    };
    let folded = if expanded {
        0
    } else {
        body.len().saturating_sub(THINKING_TAIL_ROWS)
    };
    body.drain(..folded);
    let mut header = match thinking.elapsed {
        None => format!(
            "∴ Thinking… {}",
            format_elapsed(thinking.started.elapsed().as_secs())
        ),
        Some(elapsed) => format!("∴ Thought for {}", format_elapsed(elapsed.as_secs().max(1))),
    };
    if folded > 0 {
        header.push_str(&format!(" · +{folded} lines"));
        if hints {
            header.push_str(" · ctrl+o to expand");
        }
    } else if hints && expanded && body.len() > THINKING_TAIL_ROWS {
        header.push_str(" · ctrl+o to fold");
    }
    let mut lines = wrap_hanging(&Line::styled(header, muted), width, 2);
    lines.extend(body);
    lines
}

/// The user's turn: `› ` and the text on a full-width background strip,
/// so turns stand out when scrolling back through a long session.
fn user_lines(text: &str, width: usize, theme: &Theme) -> Vec<Line> {
    let width = width.max(1);
    let strip = Style::default().bg(theme.user_bg);
    let marker = Style::fg(theme.accent).bg(theme.user_bg).bold();
    let mut rows = Vec::new();
    for (index, raw) in text.split('\n').enumerate() {
        let prefix = if index == 0 { "› " } else { "  " };
        let line = Line {
            spans: vec![Span::styled(prefix, marker), Span::styled(raw, strip)],
        };
        for mut row in wrap_hanging(&line, width, 2) {
            // The hanging indent comes back unstyled: paint it into the strip.
            for span in &mut row.spans {
                if span.style.is_plain() {
                    span.style = strip;
                }
            }
            let used = row.width();
            if used < width {
                row.spans
                    .push(Span::styled(" ".repeat(width - used), strip));
            }
            rows.push(row);
        }
    }
    rows
}

/// Prepend a fixed indent to a styled line (used to nest markdown under a badge).
fn indent_line(line: Line, pad: &str) -> Line {
    let mut spans = vec![Span::raw(pad)];
    spans.extend(line.spans);
    Line { spans }
}

/// Output rows kept under a command card.
const OUTPUT_TAIL_ROWS: usize = 5;
/// Diff rows shown under an edit card before `… +N lines`.
const DIFF_MAX_ROWS: usize = 10;

/// A tool call: status glyph (spinner while running, then `✓` / `✗` /
/// `○`), the tool name in bold, its headline argument, the elapsed time
/// for slow calls, and a result preview — the tail of a command's output,
/// an edit's diff, or a one-line summary.
fn tool_card_lines(tool: &ToolEntry, width: usize, theme: &Theme, frame: usize) -> Vec<Line> {
    let muted = Style::fg(theme.muted);
    let (glyph, glyph_style) = match tool.status {
        ToolStatus::Running => (FRAMES[frame % FRAMES.len()], Style::fg(theme.accent)),
        ToolStatus::Ok => ("✓", Style::fg(theme.success)),
        ToolStatus::Failed => ("✗", Style::fg(theme.error)),
        ToolStatus::Cancelled => ("○", muted),
    };
    let mut spans = vec![
        Span::styled(format!("{glyph} "), glyph_style),
        Span::styled(tool.name.clone(), Style::default().bold()),
    ];
    if !tool.headline.is_empty() {
        spans.push(Span::styled(format!(" {}", tool.headline), muted));
    }
    let elapsed = tool.elapsed.unwrap_or_else(|| tool.started.elapsed());
    if elapsed.as_secs() >= 2 {
        spans.push(Span::styled(
            format!(" · {}", format_elapsed(elapsed.as_secs())),
            muted,
        ));
    }
    let mut lines = wrap_hanging(&Line { spans }, width, 2);

    let first_row = |text: String| format!("  ⎿ {text}");
    let failed = tool.status == ToolStatus::Failed;
    match &tool.detail {
        ToolDetail::Output {
            tail,
            hidden,
            exit_code,
        } => {
            let mut rows: Vec<String> = Vec::new();
            if *hidden > 0 {
                rows.push(format!("… +{hidden} earlier lines"));
            }
            rows.extend(tail.iter().cloned());
            if rows.is_empty() && !failed {
                rows.push("no output".to_string());
            }
            let output_rows = rows.len();
            for (index, row) in rows.into_iter().enumerate() {
                let text = if index == 0 {
                    first_row(row)
                } else {
                    format!("    {row}")
                };
                lines.push(truncate(&Line::styled(text, muted), width));
            }
            let indent = |text: String| {
                if output_rows == 0 {
                    first_row(text)
                } else {
                    format!("    {text}")
                }
            };
            match exit_code.filter(|code| *code != 0) {
                Some(code) => lines.push(Line::styled(
                    indent(format!("exit {code}")),
                    Style::fg(theme.error),
                )),
                // No exit code: a timeout or a spawn error says why.
                None if failed => {
                    let error = tool.summary.clone().unwrap_or_else(|| "failed".to_string());
                    lines.push(truncate(
                        &Line::styled(indent(error), Style::fg(theme.error)),
                        width,
                    ));
                }
                None => {}
            }
        }
        _ if failed => {
            let error = tool.summary.clone().unwrap_or_else(|| "failed".to_string());
            lines.push(truncate(
                &Line::styled(first_row(error), Style::fg(theme.error)),
                width,
            ));
        }
        ToolDetail::Diff { removed, added } => {
            lines.push(Line::styled(
                first_row(format!("−{} +{}", removed.len(), added.len())),
                muted,
            ));
            let rows = removed
                .iter()
                .map(|text| ('-', text, theme.diff_del, theme.diff_del_bg))
                .chain(
                    added
                        .iter()
                        .map(|text| ('+', text, theme.diff_add, theme.diff_add_bg)),
                );
            let total = removed.len() + added.len();
            for (sign, text, sign_color, background) in rows.take(DIFF_MAX_ROWS) {
                let row = Line {
                    spans: vec![
                        Span::raw("    "),
                        Span::styled(format!("{sign} "), Style::fg(sign_color).bg(background)),
                        Span::styled(text.clone(), Style::default().bg(background)),
                    ],
                };
                let mut row = truncate(&row, width);
                let used = row.width();
                if used < width {
                    row.spans.push(Span::styled(
                        " ".repeat(width - used),
                        Style::default().bg(background),
                    ));
                }
                lines.push(row);
            }
            if total > DIFF_MAX_ROWS {
                lines.push(Line::styled(
                    format!("    … +{} lines", total - DIFF_MAX_ROWS),
                    muted,
                ));
            }
        }
        ToolDetail::Summary => {
            if let Some(summary) = &tool.summary {
                lines.push(truncate(
                    &Line::styled(first_row(summary.clone()), muted),
                    width,
                ));
            }
        }
    }
    lines
}

/// Wrap one transcript row to the panel width with the chat's hanging
/// indent: continuations align under the entry's leading marker. Width is
/// measured in display columns (CJK/emoji take two), not chars.
fn wrap_styled_line(line: Line, width: usize) -> Vec<Line> {
    let indent = hanging_indent(&line.text());
    wrap_hanging(&line, width, indent)
}

/// Continuation indent for a transcript row: its leading spaces plus one
/// leading marker (`•`, `›`, `⎿`, `│`, a status glyph, or an ordered-list
/// number such as `12.`) and the space after it.
fn hanging_indent(text: &str) -> usize {
    let spaces = text.chars().take_while(|ch| *ch == ' ').count();
    // Spaces are ASCII, so the char count is also the byte offset.
    let rest = &text[spaces..];
    let marker_chars = match rest.chars().next() {
        Some('•' | '›' | '·' | '⎿' | '│' | '∴' | '✓' | '✗' | '●') => 1,
        Some(ch) if ch.is_ascii_digit() => {
            let digits = rest.chars().take_while(char::is_ascii_digit).count();
            match rest[digits..].chars().next() {
                Some('.' | ')') => digits + 1,
                _ => 0,
            }
        }
        _ => 0,
    };
    if marker_chars > 0 && rest.chars().nth(marker_chars) == Some(' ') {
        spaces + marker_chars + 1
    } else {
        spaces
    }
}

/// Render a block of (lightweight) Markdown to styled lines: ATX headings,
/// bullet and ordered lists, block quotes, fenced code blocks, horizontal
/// rules, tables, and inline `**bold**`, `*italic*`, `` `code` ``, and
/// `$math$`. Deliberately a pragmatic subset, not CommonMark. Code fences
/// stay literal (muted) and code lines carry no gutter, so a mouse
/// selection copies valid markdown and clean code. `width` lays out what
/// generic wrapping would break: tables and over-long code lines.
fn markdown_lines(text: &str, width: usize, theme: &Theme) -> Vec<Line> {
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
            lines.push(Line::styled(trimmed, Style::fg(theme.faint)));
            i += 1;
            continue;
        }
        if in_code {
            lines.extend(code_rows(raw, width, theme));
            i += 1;
            continue;
        }

        // Display math on lines of its own, indented under the text.
        if let Some((tex, used)) = display_math(&raw_lines[i..]) {
            lines.push(Line::styled(
                format!("  {}", crate::math::to_unicode(&tex)),
                base,
            ));
            i += used;
            continue;
        }

        // A run of `| … |` rows is a table: gather the whole block and lay it
        // out as a grid instead of leaking raw pipes.
        if is_table_row(trimmed) {
            let start = i;
            while i < raw_lines.len() && is_table_row(raw_lines[i].trim_start()) {
                i += 1;
            }
            lines.extend(table_lines(&raw_lines[start..i], width, base, theme));
            continue;
        }

        if let Some((level, body)) = heading_text(trimmed) {
            // One accent color for the top two levels; deeper levels keep
            // only the bold weight.
            let style = if level <= 2 {
                Style::fg(theme.heading).bold()
            } else {
                Style::default().bold()
            };
            lines.push(Line::styled(body, style));
            i += 1;
            continue;
        }

        if is_horizontal_rule(trimmed) || is_table_separator(trimmed) {
            lines.push(Line::styled("─".repeat(24), Style::fg(theme.faint)));
            i += 1;
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix('>') {
            let mut spans = vec![Span::styled("│ ", Style::fg(theme.faint))];
            spans.extend(inline_spans(
                rest.strip_prefix(' ').unwrap_or(rest),
                Style::fg(theme.muted),
                theme,
            ));
            lines.push(Line { spans });
            i += 1;
            continue;
        }

        if let Some((indent, marker_len)) = bullet_prefix(raw) {
            let rest = &raw[indent + marker_len..];
            let mut spans = vec![Span::styled(
                format!("{}• ", " ".repeat(indent)),
                Style::fg(theme.accent),
            )];
            spans.extend(inline_spans(rest, base, theme));
            lines.push(Line { spans });
            i += 1;
            continue;
        }

        if let Some((indent, marker_len)) = ordered_prefix(raw) {
            let mut spans = vec![Span::styled(
                raw[..indent + marker_len].to_string(),
                Style::fg(theme.accent),
            )];
            spans.extend(inline_spans(&raw[indent + marker_len..], base, theme));
            lines.push(Line { spans });
            i += 1;
            continue;
        }

        lines.push(Line {
            spans: inline_spans(raw, base, theme),
        });
        i += 1;
    }

    lines
}

/// One fenced code line. A line wider than `width` breaks with a faint
/// `↪` before each continuation, so a wrapped tail never passes for a line
/// of its own (`/copy` gives the exact source).
fn code_rows(raw: &str, width: usize, theme: &Theme) -> Vec<Line> {
    let line = Line::raw(raw);
    if line.width() <= width {
        return vec![line];
    }
    let mut rows = wrap_hanging(&line, width.saturating_sub(2), 0);
    for row in rows.iter_mut().skip(1) {
        row.spans
            .insert(0, Span::styled("↪ ", Style::fg(theme.faint)));
    }
    rows
}

/// A Markdown table row: starts with `|` and has at least one more pipe.
/// Separator rows (`|---|---|`) also match — `table_lines` filters them out.
fn is_table_row(line: &str) -> bool {
    line.starts_with('|') && line[1..].contains('|')
}

/// Lay a block of `| … |` rows out as a grid: box-drawing borders, a rule
/// under the header (the row directly above a `|---|` separator) and
/// between rows, column alignment from the separator. Cells keep their
/// inline markdown; a table wider than `width` wraps text inside its cells.
fn table_lines(rows: &[&str], width: usize, base: Style, theme: &Theme) -> Vec<Line> {
    let mut header = false;
    let mut aligns: Vec<Align> = Vec::new();
    let mut body: Vec<Vec<Vec<Span>>> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let cells = split_cells(row.trim());
        if is_table_separator(row.trim()) {
            // The row above the first separator is the header.
            if index == 1 {
                header = true;
                aligns = cells.iter().map(|cell| Align::of(cell.trim())).collect();
            }
            continue;
        }
        body.push(
            cells
                .iter()
                .map(|cell| inline_spans(cell.trim(), base, theme))
                .collect(),
        );
    }
    let columns = body.iter().map(Vec::len).max().unwrap_or(0);
    if columns == 0 {
        return Vec::new();
    }

    let mut natural = vec![1usize; columns];
    for cells in &body {
        for (column, cell) in cells.iter().enumerate() {
            let cell_width = cell.iter().map(|span| visible_width(&span.text)).sum();
            natural[column] = natural[column].max(cell_width);
        }
    }
    // Each column costs its text plus `│ ` and ` `; the row adds a last `│`.
    let widths = fit_columns(&natural, width.saturating_sub(3 * columns + 1));

    let border = Style::fg(theme.faint);
    let rule = |left: &str, middle: &str, right: &str| {
        let segments: Vec<String> = widths.iter().map(|width| "─".repeat(width + 2)).collect();
        Line::styled(format!("{left}{}{right}", segments.join(middle)), border)
    };
    let mut lines = vec![rule("┌", "┬", "┐")];
    for (index, cells) in body.into_iter().enumerate() {
        if index > 0 {
            lines.push(rule("├", "┼", "┤"));
        }
        let bold = header && index == 0;
        let wrapped: Vec<Vec<Line>> = cells
            .into_iter()
            .chain(std::iter::repeat_with(Vec::new))
            .zip(&widths)
            .map(|(spans, &width)| {
                let spans = if bold {
                    spans
                        .into_iter()
                        .map(|span| Span::styled(span.text, span.style.bold()))
                        .collect()
                } else {
                    spans
                };
                wrap_hanging(&Line { spans }, width, 0)
            })
            .collect();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
        for row in 0..height {
            let mut spans = vec![Span::styled("│", border)];
            for (column, cell) in wrapped.iter().enumerate() {
                let piece = cell.get(row).cloned().unwrap_or_default();
                let pad = widths[column].saturating_sub(piece.width());
                let before = match aligns.get(column) {
                    Some(Align::Right) => pad,
                    Some(Align::Center) => pad / 2,
                    _ => 0,
                };
                spans.push(Span::raw(" ".repeat(before + 1)));
                spans.extend(piece.spans);
                spans.push(Span::raw(" ".repeat(pad - before + 1)));
                spans.push(Span::styled("│", border));
            }
            lines.push(Line { spans });
        }
    }
    lines.push(rule("└", "┴", "┘"));
    lines
}

/// Column alignment from the separator row: `---` or `:--` left, `--:`
/// right, `:-:` center.
#[derive(Clone, Copy)]
enum Align {
    Left,
    Center,
    Right,
}

impl Align {
    fn of(cell: &str) -> Self {
        match (cell.starts_with(':'), cell.ends_with(':')) {
            (true, true) => Align::Center,
            (false, true) => Align::Right,
            _ => Align::Left,
        }
    }
}

/// The cells of a table row. A `|` splits cells unless it is escaped (`\|`)
/// or inside inline code: models often leave pipes in code unescaped.
fn split_cells(row: &str) -> Vec<String> {
    let row = row.strip_prefix('|').unwrap_or(row);
    let row = row.strip_suffix('|').unwrap_or(row);
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut in_code = false;
    let mut chars = row.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' if chars.peek() == Some(&'|') => {
                chars.next();
                cell.push('|');
            }
            '|' if !in_code => cells.push(std::mem::take(&mut cell)),
            _ => {
                in_code ^= ch == '`';
                cell.push(ch);
            }
        }
    }
    cells.push(cell);
    cells
}

/// Fit column widths into `budget`: columns narrower than an equal share
/// keep their width, the wider ones split what is left evenly.
fn fit_columns(natural: &[usize], budget: usize) -> Vec<usize> {
    let mut widths = natural.to_vec();
    if natural.iter().sum::<usize>() <= budget {
        return widths;
    }
    let mut open: Vec<usize> = (0..natural.len()).collect();
    let mut left = budget;
    loop {
        let share = left / open.len().max(1);
        let (fit, wide): (Vec<usize>, Vec<usize>) =
            open.iter().partition(|&&column| natural[column] <= share);
        if fit.is_empty() {
            let extra = left % wide.len().max(1);
            for (rank, &column) in wide.iter().enumerate() {
                widths[column] = (share + usize::from(rank < extra)).max(1);
            }
            return widths;
        }
        left -= fit.iter().map(|&column| natural[column]).sum::<usize>();
        open = wide;
    }
}

/// Display math on lines of its own: `$$ … $$` or `\[ … \]` on one line, or
/// a bare opener line through a bare closer line. Returns the TeX and the
/// lines used; `None` while the closer has not arrived (the lines stay text).
fn display_math(lines: &[&str]) -> Option<(String, usize)> {
    let first = lines[0].trim();
    let (open, close) = [("$$", "$$"), ("\\[", "\\]")]
        .into_iter()
        .find(|(open, _)| first.starts_with(open))?;
    let rest = &first[open.len()..];
    if rest.trim().is_empty() {
        let end = lines.iter().skip(1).position(|line| line.trim() == close)? + 1;
        return Some((lines[1..end].join(" "), end + 1));
    }
    let tex = rest.strip_suffix(close)?;
    (!tex.contains(close)).then(|| (tex.to_string(), 1))
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

/// If `line` begins an ordered-list item (`12. ` or `3) ` after optional
/// indent), return the indent width and the marker length incl. the space.
fn ordered_prefix(line: &str) -> Option<(usize, usize)> {
    let indent = line.len() - line.trim_start().len();
    let rest = &line[indent..];
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let after = rest.as_bytes().get(digits..digits + 2)?;
    (digits > 0 && matches!(after, [b'.' | b')', b' '])).then_some((indent, digits + 2))
}

/// Parse inline Markdown emphasis/code in `text` into styled spans.
fn inline_spans(text: &str, base: Style, theme: &Theme) -> Vec<Span> {
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
                Style::fg(theme.code),
            ));
            i = end + 1;
            continue;
        }
        // Math: rendered as Unicode before emphasis, so `_` and `*` in TeX
        // stay math.
        if let Some((tex, end)) = math_span(&chars, i) {
            buf.push_str(&crate::math::to_unicode(&tex));
            i = end;
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
                base.bold(),
            ));
            i = end + 2;
            continue;
        }
        // Italic: *text* or _text_ — short emphasis only, never whole blocks.
        // As in CommonMark, `_` inside a word (`web_search`) is no emphasis.
        let in_word = |at: Option<&char>| at.is_some_and(|ch| ch.is_alphanumeric());
        if (c == '*' || c == '_')
            && !(c == '_' && in_word(i.checked_sub(1).and_then(|at| chars.get(at))))
            && let Some(end) = find_char(&chars, i + 1, c)
            && end > i + 1
            && !(c == '_' && in_word(chars.get(end + 1)))
        {
            push_buf(&mut buf, &mut spans, base);
            spans.push(Span::styled(
                chars[i + 1..end].iter().collect::<String>(),
                base.italic(),
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

/// The math span that opens at `chars[start]` — `$…$`, `$$…$$`, `\(…\)`
/// or `\[…\]` — as its TeX and the index past the closer. A single `$`
/// follows the pandoc rule, which keeps prices like `$5 and $10` out: no
/// space after the opener, no space before the closer, no digit after it.
fn math_span(chars: &[char], start: usize) -> Option<(String, usize)> {
    let close: &[char] = match chars.get(start..start + 2)? {
        ['$', '$'] => &['$', '$'],
        ['$', _] => &['$'],
        ['\\', '('] => &['\\', ')'],
        ['\\', '['] => &['\\', ']'],
        _ => return None,
    };
    let single = close == ['$'];
    let from = start + if single { 1 } else { 2 };
    if single && chars[from].is_whitespace() {
        return None;
    }
    let end = (from + 1..=chars.len().saturating_sub(close.len())).find(|&end| {
        chars[end..].starts_with(close)
            && !(single
                && (chars[end - 1].is_whitespace()
                    || chars.get(end + 1).is_some_and(char::is_ascii_digit)))
    })?;
    Some((chars[from..end].iter().collect(), end + close.len()))
}

fn find_char(chars: &[char], from: usize, target: char) -> Option<usize> {
    (from..chars.len()).find(|&i| chars[i] == target)
}

/// Find the next `target target` pair (e.g. `**`) starting at `from`.
fn find_seq(chars: &[char], from: usize, target: char) -> Option<usize> {
    (from..chars.len().saturating_sub(1)).find(|&i| chars[i] == target && chars[i + 1] == target)
}

/// Plain-text projection of an entry for `transcript_text` (tests, non-TTY).
fn entry_plain_lines(entry: &ChatEntry) -> Vec<String> {
    match entry {
        ChatEntry::User(text) => vec![format!("you: {text}")],
        ChatEntry::Banner {
            version,
            model,
            workspace,
        } => vec![
            format!("harness v{version}"),
            format!("model {model} · workspace {workspace}"),
        ],
        ChatEntry::Assistant { text, .. } => vec![text.clone()],
        ChatEntry::Thinking(thinking) => vec![format!("thinking: {}", thinking.text)],
        ChatEntry::System(text) => vec![text.clone()],
        ChatEntry::Tool(tool) => {
            let glyph = match tool.status {
                ToolStatus::Running => "→",
                ToolStatus::Ok => "✓",
                ToolStatus::Failed => "✗",
                ToolStatus::Cancelled => "○",
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
    crate::repl::summarize_one_line(&value.to_string(), 80)
}

/// The tool name shown on a card: the canonical `file.*`/`shell.exec` form
/// when the runtime accepts the model's alias, else the alias verbatim.
fn display_tool_name(name: &str) -> String {
    crate::runtime::canonical_tool_name(name).unwrap_or_else(|| name.to_string())
}

/// The argument a human scans for on a card: the command, query or URL,
/// the path it applies to, or `primary · path` when both are present
/// (a search pattern and where it searches). Falls back to compact JSON.
fn tool_headline(arguments: &serde_json::Value) -> String {
    const PRIMARY: &[&str] = &["command", "cmd", "query", "pattern", "url"];
    let Some(map) = arguments.as_object() else {
        return compact_json_args(arguments);
    };
    let first = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| map.get(*key).and_then(serde_json::Value::as_str))
            .map(|value| crate::repl::summarize_one_line(value, 80))
    };
    match (first(PRIMARY), first(crate::runtime::PATH_ALIASES)) {
        (Some(primary), Some(path)) => format!("{primary} · {path}"),
        (Some(primary), None) => primary,
        (None, Some(path)) => path,
        (None, None) => compact_json_args(arguments),
    }
}

/// First string argument found under any of `keys`.
fn string_argument<'a>(arguments: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| arguments.get(*key).and_then(serde_json::Value::as_str))
}

/// The preview under a finished card: a command's output tail, an edit's
/// diff (from the call's own arguments — the result only counts
/// replacements), or the summary line.
fn tool_detail(
    name: &str,
    arguments: &serde_json::Value,
    result: &crate::runtime::ToolBatchResult,
) -> ToolDetail {
    match name {
        // A failed command keeps its output too: the tail says why, and
        // the model read the same text.
        "shell.exec" => {
            let mut rows: Vec<String> = result
                .content
                .lines()
                .map(|line| line.trim_end().to_string())
                .collect();
            while rows.last().is_some_and(String::is_empty) {
                rows.pop();
            }
            let hidden = rows.len().saturating_sub(OUTPUT_TAIL_ROWS);
            ToolDetail::Output {
                tail: rows.split_off(hidden),
                hidden,
                exit_code: result
                    .metadata
                    .get("exit_code")
                    .and_then(serde_json::Value::as_i64),
            }
        }
        "file.replace" if result.ok => {
            let old = string_argument(arguments, crate::runtime::REPLACE_OLD_ALIASES);
            let new = string_argument(arguments, crate::runtime::REPLACE_NEW_ALIASES);
            match (old, new) {
                (Some(old), Some(new)) => ToolDetail::Diff {
                    removed: old.lines().map(str::to_string).collect(),
                    added: new.lines().map(str::to_string).collect(),
                },
                _ => ToolDetail::Summary,
            }
        }
        _ => ToolDetail::Summary,
    }
}

/// One-line result for the plain projection and summary-only cards.
fn tool_result_summary(
    name: &str,
    arguments: &serde_json::Value,
    result: &crate::runtime::ToolBatchResult,
) -> Option<String> {
    if result.ok {
        match name {
            // The file's first line says nothing about a read; its size does.
            "file.read" => {
                let lines = result.content.lines().count();
                let truncated = result
                    .metadata
                    .get("truncated")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                return Some(format!(
                    "{lines} lines{}",
                    if truncated { " · truncated" } else { "" }
                ));
            }
            "file.write" | "file.append" => {
                if let Some(content) = string_argument(arguments, crate::runtime::CONTENT_ALIASES) {
                    let verb = if name == "file.write" {
                        "wrote"
                    } else {
                        "appended"
                    };
                    return Some(format!("{verb} {} lines", content.lines().count()));
                }
            }
            _ => {}
        }
    }
    generic_result_summary(result)
}

fn generic_result_summary(result: &crate::runtime::ToolBatchResult) -> Option<String> {
    if !result.ok {
        let error = result.error.clone().unwrap_or_else(|| "failed".to_string());
        return Some(crate::repl::summarize_one_line(&error, 100));
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
        Some(crate::repl::summarize_one_line(first, 100))
    } else {
        Some(format!(
            "{} lines · {}",
            rest + 1,
            crate::repl::summarize_one_line(first, 80)
        ))
    }
}
