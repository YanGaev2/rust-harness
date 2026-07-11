# Session & Trace Persistence — Design

**Date:** 2026-07-11
**Status:** Approved (brainstorm 2026-07-10..11)

## Purpose

Two intertwined capabilities:

1. **Trace persistence** — every agent run is automatically saved to disk as a raw
   `AgentTrace`, so runs can be analyzed offline (model quality, bad tool calls and
   why they were bad: `ok`/`error`, `repaired`, repair `hint`).
2. **Session persistence + resume** — the REPL/TUI conversation survives restarts.
   Launching `harness` resumes the last chat for the workspace by default. This
   requires multi-turn conversation support, which the agent loop currently lacks
   (each submit builds `messages = vec![user(...)]` from scratch).

Non-goals: `/compact` (format reserves `parent_session` for it, nothing more),
cross-process locking, trace summaries/analytics (raw trace only — analysis happens
outside the harness), session support for one-shot `harness agent run`.

## Storage layout

Global per-user root, grouped by workspace (same convention as `~/.claude` /
`~/.codex` — home-dir dotfolder, portable across Windows and Linux):

```
~/.harness/projects/<slug>/
├── sessions/
│   ├── 2026-07-10_14-33-05_a1b2c3.jsonl
│   └── last                                # plain text: filename of latest session
└── traces/
    └── 2026-07-10_14-35-12_deepseek_r3.json
```

- `<slug>`: sanitized absolute workspace path (non-alphanumeric → `-`, e.g.
  `F--rust-harness`).
- Home dir from `USERPROFILE` (Windows) / `HOME` (Unix) directly; no new crates.
- `last` is an explicit pointer (mtime is unreliable: backups/antivirus touch files).

## Session file format (JSONL)

One session = one append-only JSONL. First line is a header; every subsequent line
is one record. A compact operation (future) starts a **new file** whose header
points back via `parent_session`; old file stays an untouched archive.

```jsonl
{"type":"meta","session_id":"a1b2c3","workspace":"F:\\rust-harness","provider":"deepseek","model":"deepseek-chat","created":"2026-07-10T14:33:05Z","parent_session":null}
{"type":"message","role":"user","content":"почини тест","ts":"..."}
{"type":"thinking","content":"Так, тест падает из-за...","ts":"..."}
{"type":"message","role":"assistant","content":"","tool_calls":[...],"ts":"..."}
{"type":"message","role":"tool","tool_call_id":"call_1","content":"{...}","ts":"..."}
{"type":"message","role":"assistant","content":"готово","ts":"..."}
```

Invariant: **filter `type == "message"` and you get exactly the `Vec<ChatMessage>`
the model sees.** `thinking` records (and any future non-replayed record types) are
for humans/analysis only and are never replayed to the provider.

## Trace file format

One agent run = one JSON file: the raw `AgentTrace` (unchanged) in a thin wrapper
linking it to its session:

```json
{"ts":"2026-07-10T14:35:12Z","session_id":"a1b2c3","turn":3,"trace":{ ...AgentTrace... }}
```

The trace already carries everything needed for bad-call analysis:
`ModelToolCalls` (original name/arguments as the model sent them), `ToolResult`
with `ok`/`error`, `repaired`, and `hint` (the repair memo explaining what was
wrong). Written after **every** run — success and error paths
(`AgentError::MaxToolRoundsExceeded` carries its trace) — for the REPL, TUI, and
`agent run`. Filename: `<ts>_<provider>_r<turn>.json`.

## AgentRunner changes (src/agent.rs)

Exactly two, both additive; persistence stays out of the agent loop:

1. `with_history(Vec<ChatMessage>)` — builder; `run_with_events` seeds
   `messages = history + [user(new_message)]`.
2. `AgentRunResult` gains `messages: Vec<ChatMessage>` — the full message list
   after the run (currently built internally and discarded). The REPL carries it
   forward as next-turn history.

Cache note: history only ever **appends** to the tail of `messages`, so
`cache_prefix_key` (system prompt + tools) stays stable across the whole chat —
provider prefix caching keeps working at any conversation depth.

## New module: src/session.rs

```rust
pub struct SessionStore { root: PathBuf }        // ~/.harness/projects/<slug>/
impl SessionStore {
    pub fn for_workspace(ws: &Path) -> SessionStore;
    pub fn with_root(root: PathBuf) -> SessionStore;   // test seam (tempdir)
    pub fn create_session(&self, workspace: &Path, provider: &str, model: &str)
        -> Session;                                    // jsonl + meta + update `last`
    pub fn resume_last(&self) -> Option<Session>;
    pub fn write_trace(&self, w: &TraceWrapper) -> ...;
}

pub struct Session { id, path, records: Vec<SessionRecord> }
impl Session {
    pub fn replay_messages(&self) -> Vec<ChatMessage>; // type=="message" only
    pub fn append_user(&mut self, text: &str);         // written at submit time
    pub fn append_run(&mut self, trace: &AgentTrace);  // thinking/calls/results/final
}

#[serde(tag = "type", rename_all = "snake_case")]
enum SessionRecord { Meta {...}, Message {...}, Thinking {...} }
```

Key decision: **`append_run` converts trace events to session lines.** The trace
already holds everything in order (thinking → round's tool calls → results →
final), and the mapping is unambiguous: `ModelToolCalls` → assistant message with
tool_calls; `ToolResult` → tool message whose content is the same JSON the runner
sends the provider. One writer, one source of truth, works on error paths too.

Support code (no new dependencies): `Deserialize` for `ChatMessage`
(src/request.rs — currently `Serialize` only); ~20-line epoch→UTC civil-from-days
helper for timestamps; session id = first 6 hex chars of
`blake3(workspace_path + epoch_millis + pid)`.

## REPL/TUI/CLI integration

- **Startup**: `last` exists → load, announce `resumed session a1b2c3 (12
  messages)`, render history into the transcript; else create a new session.
- **`/new`** (REPL and TUI): start a fresh session; `last` switches to it.
- **Write order**: user message immediately at submit; the rest from the trace
  after the run. Trade-off: a crash mid-run loses that run but not the user input.
- **Write failures never kill the chat**: warn via system line, continue in memory.
- **`agent run`**: writes traces, does not participate in sessions (one-shot,
  stateless by design).
- Model switching mid-session works for free: history is provider-agnostic
  `ChatMessage`s; meta records the starting provider/model, each trace records the
  actual one.

## Edge cases

| Situation | Behavior |
|---|---|
| `last` points to a missing/corrupt file | Warn + start a new session; corrupt JSONL lines are skipped with a warning (crash may truncate mid-line) |
| Session ends with dangling assistant tool_calls (crash mid-run) | Replay trims the tail to the last valid point — providers reject tool_calls without matching results |
| `~/.harness/` missing | `create_dir_all` on first write; failure → warn, run in memory |
| Neither `USERPROFILE` nor `HOME` set | Persistence disabled with a warning; chat still works |
| Two `harness` processes, same workspace | Not contested: unique session ids, last writer wins `last`. Known limitation |
| Non-TTY line-mode REPL | Same session logic (terminal-independent) |
| Very long session | Nothing (compact is out of scope; `parent_session` is reserved) |

## Testing

Repo conventions: TDD, `tempfile::tempdir()`, raw `TcpListener` mock servers.

`tests/session_store.rs` (new, via `SessionStore::with_root`):
1. Create session → meta line + `last` updated.
2. `append_user` + `append_run` from a canned `AgentTrace` → correct lines, thinking present.
3. `replay_messages` filters thinking; messages byte-identical to the originals.
4. Full round-trip: write dialog → load → identical `Vec<ChatMessage>`.
5. Corrupt / truncated line → skipped, rest preserved.
6. Dangling tool_calls → tail trimmed.
7. `resume_last` with no `last` file → `None`.
8. Trace filename + wrapper contents (`ts`, `session_id`, `turn`, raw trace).

`tests/agent_loop.rs` (existing mock-server pattern):
9. `with_history`: request body contains history before the new user message.
10. `AgentRunResult.messages` is the full post-run list.

`tests/repl*.rs` (line mode, non-TTY):
11. REPL run with tempdir root → session + trace on disk afterwards.
12. Second run → "resumed session", history reaches the mock provider.

Test #3 is the load-bearing one: it guards the only invariant resume depends on —
a resumed session sends the provider exactly what the model saw before restart.

## Docs

Per repo convention: bullet in `goal.md`; `README.md` section covering storage
locations, `/new`, and resume behavior.
