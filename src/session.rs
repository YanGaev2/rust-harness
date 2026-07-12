//! Persistent chat sessions and agent traces under `~/.harness/projects/<slug>/`.
//!
//! One session = one append-only JSONL file. The invariant the whole resume
//! feature rests on: filtering the records down to `type == "message"` yields
//! exactly the `Vec<ChatMessage>` the provider saw. `thinking` records (and any
//! future non-replayed kinds) are for humans and offline analysis only.
//!
//! One agent run = one JSON trace file: the raw `AgentTrace` in a thin wrapper
//! that links it back to its session and turn.

use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::agent::{AgentError, AgentRunResult, AgentTrace, AgentTraceEvent};
use crate::request::{ChatMessage, MessageToolCall};

/// Storage root for one workspace: `<root>/sessions/*.jsonl` + `<root>/traces/*.json`.
#[derive(Debug, Clone)]
pub struct SessionStore {
    root: PathBuf,
}

impl SessionStore {
    /// Global per-user location, grouped by workspace slug. `HARNESS_HOME`
    /// replaces `~/.harness` entirely when set (tests point it at a tempdir).
    /// `None` when no home directory is available (persistence is then
    /// disabled, chat still works).
    pub fn for_workspace(workspace: &Path) -> Option<Self> {
        let base = match std::env::var_os("HARNESS_HOME") {
            Some(dir) => PathBuf::from(dir),
            None => std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(PathBuf::from)?
                .join(".harness"),
        };
        Some(Self {
            root: base.join("projects").join(workspace_slug(workspace)),
        })
    }

    /// Explicit root, used by tests to point at a tempdir.
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    fn traces_dir(&self) -> PathBuf {
        self.root.join("traces")
    }

    pub fn create_session(
        &self,
        workspace: &Path,
        provider: &str,
        model: &str,
    ) -> Result<Session, SessionError> {
        let sessions = self.sessions_dir();
        fs::create_dir_all(&sessions)?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let created = format_utc_timestamp(now.as_secs());
        // Two sessions created within the same millisecond (same process,
        // same workspace — e.g. `/new` right after startup on a fast fs)
        // would hash to the same id and silently share a file; bump the
        // seed until the path is fresh.
        let mut millis = now.as_millis();
        let (id, path) = loop {
            let id = session_id(workspace, millis, std::process::id());
            let path = sessions.join(format!("{created}_{id}.jsonl"));
            if !path.exists() {
                break (id, path);
            }
            millis += 1;
        };

        let meta = SessionRecord::Meta {
            session_id: id.clone(),
            workspace: workspace.display().to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            created,
            parent_session: None,
        };
        let mut session = Session {
            id,
            path,
            records: Vec::new(),
            skipped_lines: 0,
        };
        session.append_record(meta)?;
        fs::write(sessions.join("last"), session.file_name())?;
        Ok(session)
    }

    /// Load the session the `last` pointer names. `Ok(None)` when there is no
    /// pointer or its target is gone; corrupt lines are skipped, not fatal
    /// (a crash may truncate the file mid-line).
    pub fn resume_last(&self) -> Result<Option<Session>, SessionError> {
        let sessions = self.sessions_dir();
        let Ok(pointer) = fs::read_to_string(sessions.join("last")) else {
            return Ok(None);
        };
        let path = sessions.join(pointer.trim());
        let Ok(content) = fs::read_to_string(&path) else {
            return Ok(None);
        };

        let mut records = Vec::new();
        let mut skipped_lines = 0;
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<SessionRecord>(line) {
                Ok(record) => records.push(record),
                Err(_) => skipped_lines += 1,
            }
        }

        let id = records
            .iter()
            .find_map(|record| match record {
                SessionRecord::Meta { session_id, .. } => Some(session_id.clone()),
                _ => None,
            })
            .or_else(|| id_from_file_name(&path))
            .unwrap_or_default();

        Ok(Some(Session {
            id,
            path,
            records,
            skipped_lines,
        }))
    }

    /// Persist one agent run: `traces/<ts>_<provider>_r<turn>.json`.
    pub fn write_trace(&self, wrapper: &TraceWrapper) -> Result<PathBuf, SessionError> {
        let traces = self.traces_dir();
        fs::create_dir_all(&traces)?;
        let path = traces.join(format!(
            "{}_{}_r{}.json",
            wrapper.ts, wrapper.trace.provider, wrapper.turn
        ));
        fs::write(&path, serde_json::to_string(wrapper)?)?;
        Ok(path)
    }
}

/// A live or resumed chat session backed by an append-only JSONL file.
#[derive(Debug)]
pub struct Session {
    id: String,
    path: PathBuf,
    records: Vec<SessionRecord>,
    skipped_lines: usize,
}

impl Session {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Corrupt JSONL lines dropped while loading (0 for freshly created sessions).
    pub fn skipped_lines(&self) -> usize {
        self.skipped_lines
    }

    /// The user's turn is written immediately at submit time, so a crash
    /// mid-run loses the run but never the input.
    pub fn append_user(&mut self, text: &str) -> Result<(), SessionError> {
        self.append_record(SessionRecord::Message {
            message: ChatMessage::user(text),
            ts: now_timestamp(),
        })
    }

    /// Convert one finished (or aborted) run's trace into session records. The
    /// trace is the single source of truth: it already holds thinking, tool
    /// calls, results, and the final answer in provider order. The user message
    /// is NOT written here — `append_user` already did.
    pub fn append_run(&mut self, trace: &AgentTrace) -> Result<(), SessionError> {
        for event in &trace.events {
            match event {
                AgentTraceEvent::Thinking { content } => {
                    self.append_record(SessionRecord::Thinking {
                        content: content.clone(),
                        ts: now_timestamp(),
                    })?;
                }
                AgentTraceEvent::ModelToolCalls { calls, .. } => {
                    let tool_calls = calls
                        .iter()
                        .map(|call| {
                            MessageToolCall::new(&call.id, &call.name, call.arguments.clone())
                        })
                        .collect();
                    self.append_record(SessionRecord::Message {
                        message: ChatMessage::assistant_tool_calls(tool_calls),
                        ts: now_timestamp(),
                    })?;
                }
                AgentTraceEvent::ToolResult { result, .. } => {
                    // Same JSON payload the runner hands the provider.
                    let content = serde_json::to_string(result)?;
                    self.append_record(SessionRecord::Message {
                        message: ChatMessage::tool_result(&result.id, content),
                        ts: now_timestamp(),
                    })?;
                }
                AgentTraceEvent::FinalContent { content } => {
                    if let Some(content) = content {
                        self.append_record(SessionRecord::Message {
                            message: ChatMessage::assistant(content),
                            ts: now_timestamp(),
                        })?;
                    }
                }
                AgentTraceEvent::Error { .. } => {}
            }
        }
        Ok(())
    }

    /// The conversation to replay to the provider: `message` records only,
    /// trimmed back to the last point with no unanswered tool calls (providers
    /// reject assistant tool_calls without matching results).
    pub fn replay_messages(&self) -> Vec<ChatMessage> {
        let mut messages: Vec<ChatMessage> = self
            .records
            .iter()
            .filter_map(|record| match record {
                SessionRecord::Message { message, .. } => Some(message.clone()),
                _ => None,
            })
            .collect();

        let mut pending: HashSet<String> = HashSet::new();
        let mut valid_len = 0;
        for (index, message) in messages.iter().enumerate() {
            for call in message.tool_calls() {
                pending.insert(call.id().to_string());
            }
            if let Some(id) = message.tool_call_id() {
                pending.remove(id);
            }
            if pending.is_empty() {
                valid_len = index + 1;
            }
        }
        messages.truncate(valid_len);
        messages
    }

    fn append_record(&mut self, record: SessionRecord) -> Result<(), SessionError> {
        let mut line = serde_json::to_string(&record)?;
        line.push('\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(line.as_bytes())?;
        self.records.push(record);
        Ok(())
    }
}

/// UI-independent glue between the agent loop and persistent storage: owns the
/// in-memory conversation history plus the on-disk session, and writes a trace
/// per run. Every disk failure degrades to a notice — the chat never dies
/// because persistence did.
#[derive(Debug)]
pub struct ChatSession {
    store: Option<SessionStore>,
    workspace: PathBuf,
    disk: Option<Session>,
    history: Vec<ChatMessage>,
    turn: usize,
    notices: Vec<String>,
}

impl ChatSession {
    /// Resume the workspace's last session when one exists, else create a new
    /// one. `store: None` (no home directory) keeps the chat memory-only.
    pub fn start(
        store: Option<SessionStore>,
        workspace: &Path,
        provider: &str,
        model: &str,
    ) -> Self {
        let mut chat = Self {
            store,
            workspace: workspace.to_path_buf(),
            disk: None,
            history: Vec::new(),
            turn: 0,
            notices: Vec::new(),
        };
        let Some(store) = chat.store.clone() else {
            chat.notices
                .push("no home directory: session persistence disabled".to_string());
            return chat;
        };

        match store.resume_last() {
            Ok(Some(session)) => {
                if session.skipped_lines() > 0 {
                    chat.notices.push(format!(
                        "session {}: skipped {} corrupt line(s)",
                        session.id(),
                        session.skipped_lines()
                    ));
                }
                chat.history = session.replay_messages();
                // Continue turn numbering where the session left off, so trace
                // files stay sequential across restarts (one user turn = one run).
                chat.turn = chat
                    .history
                    .iter()
                    .filter(|message| message.role() == "user")
                    .count();
                chat.notices.push(format!(
                    "resumed session {} ({} messages)",
                    session.id(),
                    chat.history.len()
                ));
                chat.disk = Some(session);
            }
            Ok(None) => chat.create_disk_session(provider, model),
            Err(err) => {
                chat.notices
                    .push(format!("could not resume last session: {err}"));
                chat.create_disk_session(provider, model);
            }
        }
        chat
    }

    /// Abandon the current session and point `last` at a fresh one (`/new`).
    pub fn start_new(&mut self, provider: &str, model: &str) {
        self.history.clear();
        self.turn = 0;
        self.disk = None;
        if self.store.is_some() {
            self.create_disk_session(provider, model);
        }
    }

    /// Prior messages to hand to `AgentRunner::with_history`.
    pub fn history(&self) -> Vec<ChatMessage> {
        self.history.clone()
    }

    /// Warnings and status lines accumulated since the last call; the UI drains
    /// and renders them however it likes.
    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    /// Persist the user's turn immediately, before the run: a crash mid-run
    /// loses the run but never the input.
    pub fn begin_turn(&mut self, user_message: &str) {
        if let Some(session) = self.disk.as_mut()
            && let Err(err) = session.append_user(user_message)
        {
            self.notices.push(format!("session write failed: {err}"));
        }
    }

    pub fn complete_turn(&mut self, result: &AgentRunResult) {
        self.turn += 1;
        self.history = result.messages.clone();
        self.persist_run(&result.trace);
    }

    /// A failed run still leaves a trace worth keeping (e.g. max tool rounds
    /// exceeded); the in-memory history stays at the last complete exchange.
    pub fn fail_turn(&mut self, error: &AgentError) {
        self.turn += 1;
        if let Some(trace) = error.trace() {
            self.persist_run(trace);
        }
    }

    fn persist_run(&mut self, trace: &AgentTrace) {
        if let Some(session) = self.disk.as_mut()
            && let Err(err) = session.append_run(trace)
        {
            self.notices.push(format!("session write failed: {err}"));
        }
        if let Some(store) = self.store.as_ref() {
            let session_id = self
                .disk
                .as_ref()
                .map(|session| session.id().to_string())
                .unwrap_or_default();
            let wrapper = TraceWrapper::new(session_id, self.turn, trace.clone());
            if let Err(err) = store.write_trace(&wrapper) {
                self.notices.push(format!("trace write failed: {err}"));
            }
        }
    }

    fn create_disk_session(&mut self, provider: &str, model: &str) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        match store.create_session(&self.workspace, provider, model) {
            Ok(session) => self.disk = Some(session),
            Err(err) => {
                self.notices
                    .push(format!("could not create session file: {err}"));
            }
        }
    }
}

/// One line of a session JSONL file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionRecord {
    Meta {
        session_id: String,
        workspace: String,
        provider: String,
        model: String,
        created: String,
        parent_session: Option<String>,
    },
    Message {
        #[serde(flatten)]
        message: ChatMessage,
        ts: String,
    },
    Thinking {
        content: String,
        ts: String,
    },
}

/// One persisted agent run: the raw trace plus its session linkage.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TraceWrapper {
    pub ts: String,
    pub session_id: String,
    pub turn: usize,
    pub trace: AgentTrace,
}

impl TraceWrapper {
    pub fn new(session_id: impl Into<String>, turn: usize, trace: AgentTrace) -> Self {
        Self {
            ts: now_timestamp(),
            session_id: session_id.into(),
            turn,
            trace,
        }
    }
}

#[derive(Debug)]
pub enum SessionError {
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "session storage io error: {err}"),
            Self::Json(err) => write!(f, "session record serialization failed: {err}"),
        }
    }
}

impl Error for SessionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Json(err) => Some(err),
        }
    }
}

impl From<std::io::Error> for SessionError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<serde_json::Error> for SessionError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}

/// Sanitized absolute workspace path, e.g. `F:\rust-harness` → `F--rust-harness`
/// (same convention Claude Code uses for `~/.claude/projects/`).
pub fn workspace_slug(workspace: &Path) -> String {
    workspace
        .display()
        .to_string()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// UTC timestamp formatted filename-safe: `2026-07-11T09-15-42Z` (no colons —
/// Windows rejects them in file names; the same format is used inside records
/// for consistency).
pub fn format_utc_timestamp(secs_since_epoch: u64) -> String {
    let days = (secs_since_epoch / 86_400) as i64;
    let rem = secs_since_epoch % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}-{:02}-{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant's days-to-civil algorithm; keeps us off a chrono dependency.
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (year + i64::from(month <= 2), month, day)
}

fn now_timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format_utc_timestamp(secs)
}

fn session_id(workspace: &Path, epoch_millis: u128, pid: u32) -> String {
    let seed = format!("{}|{epoch_millis}|{pid}", workspace.display());
    blake3::hash(seed.as_bytes()).to_hex()[..6].to_string()
}

fn id_from_file_name(path: &Path) -> Option<String> {
    path.file_stem()?
        .to_str()?
        .rsplit('_')
        .next()
        .map(str::to_string)
}
