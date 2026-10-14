//! The "add provider" wizard shared by the setup screen and the chat:
//! pick a builtin family or `custom`, enter the name, base URL and API
//! key, then pick models from the provider's own `/models` list.
//!
//! The wizard is a pure state machine. It never does I/O: after the key
//! step it returns [`WizardAction::Fetch`], the host runs
//! `OpenAiCompatibleModelClient::discover` off the UI thread and hands the
//! result back through [`ProviderWizard::models_loaded`]. Esc belongs to
//! the host, which closes the wizard.

use harness_tui::input::{KeyCode, KeyEvent};
use harness_tui::text::{Line, Span, Style};
use harness_tui::theme::Theme;

use crate::providers::{BuiltinProvider, ModelDiscovery, ProviderConfig, provider_from_profile};

/// Model rows shown at once on the model step.
const MODEL_ROWS: usize = 10;

#[derive(Debug, Clone, PartialEq)]
pub enum WizardAction {
    Continue,
    /// Fetch the model list for this provider, then call `models_loaded`.
    Fetch(ProviderConfig),
    /// Save the provider; `model` is the one to start with.
    Save {
        provider: ProviderConfig,
        model: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Provider,
    Name,
    BaseUrl,
    ApiKey,
    Fetching,
    Failed,
    Models,
}

/// A single-line field. A prefilled default is replaced by the first
/// typed character; Backspace edits it instead.
#[derive(Debug, Clone, Default, PartialEq)]
struct Field {
    value: String,
    edited: bool,
}

impl Field {
    fn prefilled(value: &str) -> Self {
        Self {
            value: value.to_string(),
            edited: false,
        }
    }

    fn insert(&mut self, text: &str) {
        if !self.edited {
            self.value.clear();
            self.edited = true;
        }
        self.value.push_str(text);
    }

    fn backspace(&mut self) {
        self.edited = true;
        self.value.pop();
    }

    fn text(&self) -> &str {
        self.value.trim()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderWizard {
    step: Step,
    /// Index into the provider list: builtin families, then `custom`.
    selected: usize,
    name: Field,
    base_url: Field,
    api_key: Field,
    /// The provider as the model list answered it, with its list.
    discovered: Option<(ProviderConfig, ModelDiscovery)>,
    error: Option<String>,
    filter: String,
    cursor: usize,
}

impl Default for ProviderWizard {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderWizard {
    pub fn new() -> Self {
        Self {
            step: Step::Provider,
            selected: 0,
            name: Field::default(),
            base_url: Field::default(),
            api_key: Field::default(),
            discovered: None,
            error: None,
            filter: String::new(),
            cursor: 0,
        }
    }

    pub fn title(&self) -> &str {
        "Provider setup"
    }

    /// True while the model list request runs.
    pub fn is_fetching(&self) -> bool {
        self.step == Step::Fetching
    }

    fn custom_selected(&self) -> bool {
        self.selected == BuiltinProvider::ALL.len()
    }

    fn builtin(&self) -> Option<BuiltinProvider> {
        BuiltinProvider::ALL.get(self.selected).copied()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> WizardAction {
        if key.mods.ctrl {
            return WizardAction::Continue;
        }
        match self.step {
            Step::Provider => self.provider_key(key.code),
            Step::Name | Step::BaseUrl | Step::ApiKey => self.text_key(key.code),
            Step::Fetching => WizardAction::Continue,
            Step::Failed => self.failed_key(key.code),
            Step::Models => self.models_key(key.code),
        }
    }

    /// A paste lands in the active text field or the model filter.
    /// Line breaks are dropped: every field is a single line.
    pub fn handle_paste(&mut self, text: &str) {
        let text: String = text
            .chars()
            .filter(|ch| !matches!(ch, '\n' | '\r'))
            .collect();
        match self.step {
            Step::Name => self.name.insert(&text),
            Step::BaseUrl => self.base_url.insert(&text),
            Step::ApiKey => self.api_key.insert(&text),
            Step::Models => {
                self.filter.push_str(&text);
                self.cursor = 0;
            }
            Step::Provider | Step::Fetching | Step::Failed => {}
        }
    }

    /// Feed the model list result (or the error text) back in.
    pub fn models_loaded(&mut self, result: Result<(ProviderConfig, ModelDiscovery), String>) {
        if self.step != Step::Fetching {
            return;
        }
        match result {
            Ok((provider, discovery)) => {
                self.cursor = self.start_cursor(&discovery);
                self.discovered = Some((provider, discovery));
                self.filter.clear();
                self.error = None;
                self.step = Step::Models;
            }
            Err(error) => {
                self.error = Some(error);
                self.step = Step::Failed;
            }
        }
    }

    fn provider_key(&mut self, code: KeyCode) -> WizardAction {
        let count = BuiltinProvider::ALL.len() + 1;
        match code {
            KeyCode::Up => self.selected = (self.selected + count - 1) % count,
            KeyCode::Down => self.selected = (self.selected + 1) % count,
            KeyCode::Enter => {
                let (name, base_url) = match self.builtin() {
                    Some(builtin) => (
                        builtin.name(),
                        builtin.profile().base_url.unwrap_or_default(),
                    ),
                    None => ("", ""),
                };
                self.name = Field::prefilled(name);
                self.base_url = Field::prefilled(base_url);
                self.api_key = Field::default();
                self.step = if self.custom_selected() {
                    Step::Name
                } else {
                    Step::BaseUrl
                };
            }
            _ => {}
        }
        WizardAction::Continue
    }

    fn text_key(&mut self, code: KeyCode) -> WizardAction {
        let field = match self.step {
            Step::Name => &mut self.name,
            Step::BaseUrl => &mut self.base_url,
            _ => &mut self.api_key,
        };
        match code {
            KeyCode::Char(ch) => field.insert(&ch.to_string()),
            KeyCode::Backspace => field.backspace(),
            KeyCode::Enter if !field.text().is_empty() => match self.step {
                Step::Name => self.step = Step::BaseUrl,
                Step::BaseUrl => self.step = Step::ApiKey,
                _ => return self.start_fetch(),
            },
            _ => {}
        }
        WizardAction::Continue
    }

    fn start_fetch(&mut self) -> WizardAction {
        self.step = Step::Fetching;
        self.error = None;
        WizardAction::Fetch(self.draft_provider())
    }

    fn draft_provider(&self) -> ProviderConfig {
        provider_from_profile(
            self.name.text().to_string(),
            self.base_url.text().to_string(),
            self.api_key.text().to_string(),
        )
    }

    fn failed_key(&mut self, code: KeyCode) -> WizardAction {
        match code {
            // Back to the URL; the URL and the key keep their values, so
            // Enter, Enter retries as is.
            KeyCode::Enter => {
                self.base_url.edited = true;
                self.api_key.edited = true;
                self.step = Step::BaseUrl;
            }
            // No model list: type the model id by hand.
            KeyCode::Tab => {
                self.discovered = None;
                self.filter.clear();
                self.cursor = 0;
                self.step = Step::Models;
            }
            _ => {}
        }
        WizardAction::Continue
    }

    fn models_key(&mut self, code: KeyCode) -> WizardAction {
        match code {
            KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down => {
                let last = self.rows().len().saturating_sub(1);
                self.cursor = (self.cursor + 1).min(last);
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.cursor = 0;
            }
            KeyCode::Char(ch) => {
                self.filter.push(ch);
                self.cursor = 0;
            }
            KeyCode::Enter => return self.choose_model(),
            _ => {}
        }
        WizardAction::Continue
    }

    fn choose_model(&mut self) -> WizardAction {
        let rows = self.rows();
        let (models, model) = match rows.get(self.cursor.min(rows.len().saturating_sub(1))) {
            Some(Row::AddAll) => {
                let all = self.all_models();
                let model = self.preferred_model(&all);
                (all, model)
            }
            Some(Row::Model(model)) => (vec![model.clone()], model.clone()),
            // Nothing in the list matches: the filter text is a model id
            // the list does not know (or there is no list at all).
            None if !self.filter.trim().is_empty() => {
                let model = self.filter.trim().to_string();
                (vec![model.clone()], model)
            }
            None => return WizardAction::Continue,
        };
        let provider = match &self.discovered {
            Some((provider, discovery)) => {
                provider.clone().with_discovered_models(discovery, &models)
            }
            None => self.draft_provider().with_model(model.clone()),
        };
        WizardAction::Save { provider, model }
    }

    fn all_models(&self) -> Vec<String> {
        self.discovered
            .as_ref()
            .map(|(_, discovery)| discovery.add_all_model_ids())
            .unwrap_or_default()
    }

    /// The model to start with after "Add all": the family's
    /// bench-verified model when the list has it, else the first one.
    fn preferred_model(&self, models: &[String]) -> String {
        self.hint_in(models)
            .or_else(|| models.first().cloned())
            .unwrap_or_default()
    }

    fn hint_in(&self, models: &[String]) -> Option<String> {
        let hints = self.builtin()?.profile().model_hints;
        hints
            .iter()
            .find(|hint| models.iter().any(|model| model == *hint))
            .map(|hint| hint.to_string())
    }

    /// Start on the family's bench-verified model when the list has it.
    fn start_cursor(&self, discovery: &ModelDiscovery) -> usize {
        let models = discovery.add_all_model_ids();
        self.hint_in(&models)
            .and_then(|hint| models.iter().position(|model| *model == hint))
            .map(|index| index + 1)
            .unwrap_or(0)
    }

    /// The model step's rows: "Add all" (only without a filter), then the
    /// models that contain the filter text.
    fn rows(&self) -> Vec<Row> {
        let models = self.all_models();
        if models.is_empty() {
            return Vec::new();
        }
        let needle = self.filter.trim().to_ascii_lowercase();
        let mut rows = Vec::new();
        if needle.is_empty() {
            rows.push(Row::AddAll);
        }
        rows.extend(
            models
                .into_iter()
                .filter(|model| model.to_ascii_lowercase().contains(&needle))
                .map(Row::Model),
        );
        rows
    }

    pub fn lines(&self, theme: &Theme) -> Vec<Line> {
        let muted = Style::fg(theme.muted);
        let accent = Style::fg(theme.accent);
        let mut lines = vec![Line::styled(self.title(), Style::default().bold())];
        // What is already settled stays visible above the active step.
        if self.step != Step::Provider {
            lines.push(summary_line("provider", &self.provider_label(), muted));
        }
        if !matches!(self.step, Step::Provider | Step::Name) {
            lines.push(summary_line("name", self.name.text(), muted));
        }
        if !matches!(self.step, Step::Provider | Step::Name | Step::BaseUrl) {
            lines.push(summary_line("url", self.base_url.text(), muted));
            lines.push(summary_line(
                "key",
                &mask_secret(self.api_key.text()),
                muted,
            ));
        }
        lines.push(Line::raw(""));

        let hint = match self.step {
            Step::Provider => {
                lines.push(Line::styled("Select provider", Style::default().bold()));
                let custom = "custom (any OpenAI- or Anthropic-compatible API)";
                let names = BuiltinProvider::ALL
                    .iter()
                    .map(|provider| provider.name())
                    .chain(std::iter::once(custom));
                for (index, name) in names.enumerate() {
                    lines.push(list_row(name, index == self.selected, Vec::new(), theme));
                }
                "↑↓ select · Enter accept · Esc close"
            }
            Step::Name => {
                lines.push(field_line("Provider name", &self.name.value, accent));
                "Enter accept · Esc close"
            }
            Step::BaseUrl => {
                lines.push(field_line("Base URL", &self.base_url.value, accent));
                lines.push(Line::styled(
                    "   the address before /models, e.g. https://api.example.com/v1",
                    muted,
                ));
                "Enter accept · Esc close"
            }
            Step::ApiKey => {
                lines.push(field_line(
                    "API key",
                    &mask_secret(&self.api_key.value),
                    accent,
                ));
                "Enter fetch models · Esc close"
            }
            Step::Fetching => {
                lines.push(Line::styled(
                    format!(
                        "Fetching models from {}/models …",
                        self.base_url.text().trim_end_matches('/')
                    ),
                    accent,
                ));
                "Esc close"
            }
            Step::Failed => {
                let error = self.error.as_deref().unwrap_or("unknown error");
                lines.push(Line::styled(
                    format!("Could not fetch models: {error}"),
                    Style::fg(theme.error),
                ));
                "Enter edit URL and key · Tab type the model id · Esc close"
            }
            Step::Models => {
                self.model_lines(&mut lines, theme);
                "↑↓ select · Enter save · type to filter · Esc close"
            }
        };
        lines.push(Line::raw(""));
        lines.push(Line::styled(hint, muted));
        lines
    }

    fn model_lines(&self, lines: &mut Vec<Line>, theme: &Theme) {
        let muted = Style::fg(theme.muted);
        let filter = format!("Model › {}", self.filter);
        lines.push(Line::styled(filter, Style::default().bold()));
        let Some((provider, discovery)) = &self.discovered else {
            lines.push(Line::styled(
                "   no model list — type the model id and press Enter",
                muted,
            ));
            return;
        };
        let rows = self.rows();
        if rows.is_empty() {
            let note = if self.filter.trim().is_empty() {
                "   the provider returned no models — type the model id".to_string()
            } else {
                format!(
                    "   no match — Enter saves '{}' as the model id",
                    self.filter.trim()
                )
            };
            lines.push(Line::styled(note, muted));
            return;
        }
        let cursor = self.cursor.min(rows.len() - 1);
        let start = (cursor + 1).saturating_sub(MODEL_ROWS);
        for (index, row) in rows.iter().enumerate().skip(start).take(MODEL_ROWS) {
            let (label, details) = match row {
                Row::AddAll => (
                    format!("Add all ({} models)", discovery.add_all_model_ids().len()),
                    Vec::new(),
                ),
                Row::Model(model) => (model.clone(), model_details(discovery, model, muted)),
            };
            lines.push(list_row(&label, index == cursor, details, theme));
        }
        lines.push(Line::styled(
            format!(
                "   ({}/{}) · format {}",
                cursor + 1,
                rows.len(),
                format_label(provider)
            ),
            muted,
        ));
    }

    fn provider_label(&self) -> String {
        match self.builtin() {
            Some(builtin) => builtin.name().to_string(),
            None => "custom".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Row {
    AddAll,
    Model(String),
}

fn summary_line(label: &str, value: &str, style: Style) -> Line {
    Line::styled(format!("  {label:<9}{value}"), style)
}

fn field_line(label: &str, value: &str, accent: Style) -> Line {
    Line {
        spans: vec![
            Span::styled(format!("{label} › "), Style::default().bold()),
            Span::raw(value.to_string()),
            Span::styled("▏", accent),
        ],
    }
}

fn list_row(label: &str, selected: bool, details: Vec<Span>, theme: &Theme) -> Line {
    let (marker, style) = if selected {
        (" › ", Style::fg(theme.accent))
    } else {
        ("   ", Style::default())
    };
    let mut spans = vec![
        Span::styled(marker, Style::fg(theme.accent)),
        Span::styled(label.to_string(), style),
    ];
    spans.extend(details);
    Line { spans }
}

/// "  128k ctx · $0.44/$0.87 per Mtok" for the models the list describes.
fn model_details(discovery: &ModelDiscovery, model: &str, muted: Style) -> Vec<Span> {
    let info = discovery.model_info(model);
    let mut parts = Vec::new();
    if let Some(window) = info.context_window {
        parts.push(format!("{} ctx", compact_tokens(window)));
    }
    if let Some(pricing) = info.pricing {
        parts.push(format!(
            "${}/${} per Mtok",
            compact_price(pricing.input_per_mtok),
            compact_price(pricing.output_per_mtok)
        ));
    }
    if parts.is_empty() {
        return Vec::new();
    }
    vec![Span::styled(format!("  {}", parts.join(" · ")), muted)]
}

fn compact_tokens(tokens: u64) -> String {
    // 1 048 576 (2^20) reads as "1M", 1 500 000 as "1.5M".
    if tokens >= 1_000_000 {
        let millions = format!("{:.1}", tokens as f64 / 1_000_000.0);
        format!("{}M", millions.trim_end_matches(".0"))
    } else if tokens >= 1_000 {
        format!("{}k", tokens / 1_000)
    } else {
        tokens.to_string()
    }
}

fn compact_price(usd: f64) -> String {
    let text = format!("{usd:.3}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn format_label(provider: &ProviderConfig) -> &'static str {
    match provider.chat_api() {
        crate::providers::ChatApiFormat::OpenAiCompatible => "OpenAI-compatible",
        crate::providers::ChatApiFormat::OpenAiResponses => "OpenAI Responses",
        crate::providers::ChatApiFormat::OpenAiCodexResponses => "Codex Responses",
        crate::providers::ChatApiFormat::AnthropicMessages => "Anthropic Messages",
    }
}

fn mask_secret(value: &str) -> String {
    "*".repeat(value.chars().count().min(12))
}
