# Goal Tracking

Original goal: build a lightweight high-performance Rust harness CLI for LLM calls with
tool-calling, cache-aware requests, provider subscriptions/custom providers, a short
system prompt, high-performance tools, Windows/Linux native behavior, and convenient
paste support for text/images.

## Implemented in this checkpoint

- Rust binary/library crate initialized in `F:\rust-harness`.
- Short `harness` launcher binary can be installed into PATH and started like
  `claude` or `codex`.
- TDD coverage for provider discovery, custom provider storage, cache-key behavior,
  native shell profile selection, prompt length, forgiving file writes, shell
  timeout/output bound behavior, tool-call repair, and OpenAI-compatible chat calls.
- CLI commands:
  - `harness`
  - `provider subscriptions`
  - `provider list [--config PATH]`
  - `provider models --name NAME --url URL (--key KEY | --key-env ENV) [--auth SCHEME] [--timeout-ms N]`
  - `provider add --interactive --config PATH [--timeout-ms N]`
  - `provider add --config PATH --name NAME --url URL (--key KEY | --key-env ENV) [--auth SCHEME] [--timeout-ms N] --add-all`
  - `provider add --config PATH --name NAME --url URL (--key KEY | --key-env ENV) [--auth SCHEME] [--chat-api FORMAT] [--cache POLICY] [--timeout-ms N] --model MODEL`
  - `chat once --config PATH --provider NAME --model MODEL --message TEXT [--timeout-ms N]`
  - `chat stream --config PATH --provider NAME --model MODEL --message TEXT [--timeout-ms N]`
  - `agent run --config PATH --workspace PATH --provider NAME --model MODEL --message TEXT [--timeout-ms N] [--max-rounds N] [--max-tool-concurrency N] [--tool-timeout-ms N] [--trace PATH] [--tool-errors PATH]`
  - `repl --config PATH --workspace PATH --provider NAME --model MODEL [--timeout-ms N] [--max-rounds N] [--max-tool-concurrency N] [--tool-timeout-ms N]`
  - `diagnostics [--binary PATH] [--max-binary-bytes N] [--max-rss-bytes N]`
  - `clipboard paste --workspace PATH`
  - `tool call --workspace PATH TOOL_NAME JSON_ARGS`
  - `tool batch --workspace PATH [--max-concurrency N] [--timeout-ms N] JSON_ARRAY_OF_TOOL_CALLS`
- Tool runtime:
  - `file.read`
  - `file.tail`
  - `file.write`
  - `file.append`
  - `file.replace`
  - `file.list`
  - `file.search`
  - `file.delete`
  - `file.move`
  - `attachment.read`
  - `shell.exec`
  - bounded `file.read` defaults to 1 MiB, accepts `max_bytes`, and reports
    `bytes_read`, `total_bytes`, and `truncated`.
  - `file.tail` reads a bounded UTF-8 suffix with optional last-line limiting
    for large logs and outputs.
  - `file.hash` computes a streaming BLAKE3 digest and byte count without
    reading file content into the prompt context.
  - `file.stat` reports file/directory metadata without reading file content.
  - repair aliases such as `write_file`, `read_file`, `run_command`, `file`, `filename`, and `text`.
  - repairs common raw string or `_raw_arguments` tool arguments into structured
    arguments for file/shell calls when intent is clear.
  - coerces string numeric/boolean values such as `"lines": "2"` or
    `"overwrite": "true"` for common LLM argument-shape mistakes.
  - single-file tools accept Codex/OpenAI-style `file_path`.
  - `file.write` also accepts Codex/OpenAI-style `contents`.
  - `file.append` supports `append_file`, Codex/OpenAI-style `file_path`, and
    `contents` while preserving existing file content.
  - repair aliases such as `list_files`, `grep`, `search_files`, `dir`, and `query`.
  - repair aliases such as `edit_file`, `replace_file`, `find`, `with`, and `limit`.
  - `file.replace` accepts Codex/OpenAI-style `file_path`, `old_string`, and
    `new_string` arguments.
  - repair aliases such as `read_attachment`, `inspect_image`, `image`, and
    `image file: ...` prompt fragments.
  - repair aliases such as `remove_file`, `delete_file`, `rename_file`, `move_file`, `from`, and `to`.
  - `file.move` accepts Codex/OpenAI-style `source_path`, `target_path`, and
    `destination_path` arguments.
  - `shell.exec` drains stdout/stderr concurrently, accepts per-call
    `timeout_ms`, captures bounded output, and reports truncation metadata.
  - timed-out shell commands kill the child process, join bounded stdout/stderr
    readers, and retain captured output metadata for diagnostics.
  - `file.list` stops traversal after `max_results + 1` visited entries, enough
    to report truncation without collecting the whole workspace tree.
  - bounded scheduler for concurrent batch execution with stable result ordering.
  - optional batch timeout that returns ordered cancelled results for unfinished tool calls.
- Agent runner:
  - sends cache-aware OpenAI-compatible chat requests.
  - executes returned tool calls through the bounded scheduler.
  - appends assistant tool-call messages and tool-result messages before continuing.
  - returns tool execution errors to the model as JSON tool results instead of aborting the whole run.
  - enforces a maximum number of tool rounds and exposes `--max-rounds N` on
    `agent run` and `repl`.
  - exposes `--max-tool-concurrency N` on `agent run` and `repl` to cap
    parallel tool execution.
  - exposes `--tool-timeout-ms N` on `agent run` and `repl` to cap each
    tool-call batch.
  - `agent run --trace PATH` writes a full JSON trace containing model tool
    calls, tool results, and final content.
  - `agent run --tool-errors PATH` writes failed tool results into a separate
    JSON report.
- Tool-call tolerance:
  - common malformed OpenAI `function.arguments` JSON is repaired before runtime execution.
  - unrepaired argument strings are preserved under `_raw_arguments` so the agent can recover in the next turn.
- Streaming:
  - OpenAI-compatible chat streaming uses `stream: true` and data-only SSE chunks.
  - `chat stream` prints text deltas as they arrive and reads final usage chunks when present.
- Provider metadata:
  - built-in subscription profiles for `codex`, `xiaomi`, `glm`, `kimi`, `claude`, and `deepseek`.
  - each profile records an expected API-key environment variable and model hints.
  - provider auth can use Bearer, custom header, or subscription metadata; when
    the saved key is empty it resolves `key_env`, while an explicit API key wins.
  - provider cache policy can set provider-specific cache-key headers, body-level
    `cache_control` markers, or automatic cache metrics.
  - chat responses serialize a compact cache report with hit/miss tokens, hit
    ratio, and saved prompt-token estimate when cache metrics are present.
  - provider chat routing can select OpenAI-compatible chat, OpenAI Responses API,
    OpenAI Codex Responses API, or Anthropic Messages API format.
  - `provider list` prints saved providers, models, auth/cache/chat metadata, and
    key source without exposing inline API keys.
  - `provider add` applies builtin profile metadata when the saved provider name matches a known family.
  - `provider add --key-env ENV` can save custom providers without writing API
    keys into the config file.
  - `provider add` and `provider models` accept `--auth bearer`, `--auth header
    --auth-header NAME`, or `--auth subscription` for custom provider auth.
  - `provider add --cache` can save disabled/header/automatic/body-cache-control/
    anthropic-automatic cache policy metadata for custom providers.
  - `provider add --chat-api` can save explicit OpenAI-compatible, OpenAI Responses,
    OpenAI Codex Responses, or Anthropic Messages routing for custom providers.
  - Codex profile uses the dedicated `/codex/responses` route instead of generic
    Chat Completions.
  - `provider add --interactive` prompts for missing name/URL/key, lists models with
    `0) Add all`, and saves either all models or a selected model number.
  - network-backed provider, chat, agent, and REPL commands accept
    `--timeout-ms N` to bound API request waits from the CLI.
  - Claude uses Anthropic `x-api-key` auth and native `/messages` requests.
  - Claude enables Anthropic automatic prompt caching through top-level `cache_control` body markers.
  - DeepSeek cache metrics are parsed from `prompt_cache_hit_tokens` and `prompt_cache_miss_tokens`.
  - OpenAI-style cached token counts are parsed from `prompt_tokens_details.cached_tokens`.
- Clipboard:
  - captures text as `.harness/attachments/paste-<hash>.txt`.
  - captures PNG image bytes as `.harness/attachments/paste-<hash>.png`.
  - emits a prompt fragment for text or `image file: ...` references.
  - `attachment.read` can inspect/read those prompt fragments without allowing
    arbitrary absolute-path reads.
  - has a native system reader for Windows and Linux, plus a testable static source.
- REPL:
  - `harness` with no arguments starts the REPL when a saved provider/model is
    available, or opens the terminal interface when no provider is
    configured.
  - the no-provider startup path renders a terminal setup screen with
    status/workspace/config sections and a `[no provider] >` prompt before any
    provider wizard asks for fields.
  - the real binary entrypoints now use a Ratatui/Crossterm alternate-screen
    setup TUI when stdin/stdout are attached to a terminal, while non-TTY runs
    keep the line-mode setup path for tests and automation.
  - `/provider add` is now started from the setup TUI, but the field-entry
    provider wizard still reuses the existing line prompt flow.
  - the no-provider interface accepts `/provider add` to start provider setup
    from inside the interface instead of prompting for provider fields before
    the interface opens.
  - after `/provider add` saves a provider/model, the same `harness` launch
    continues into the REPL with that provider.
  - reads terminal key events in raw mode.
  - Ctrl+V captures text or image through the clipboard backend.
  - text paste is inserted into the current prompt.
  - image paste appends an `image file: ...` prompt fragment.
  - Enter submits through the multi-turn agent runner; Ctrl+C exits.
  - `/model PROVIDER MODEL` switches the active provider/model inside the session
    using the loaded provider config and configured model allowlist.
  - `/history QUERY` searches a bounded in-session prompt history in most-recent-first order.
  - agent runs emit streaming events for tool rounds, tool results, and final answer text;
    the terminal REPL renders those events as they arrive.
- Diagnostics:
  - reports current process RSS where the platform exposes it.
  - reports binary size for the selected binary path or current executable.
  - evaluates optional binary/RSS byte limits and returns JSON without failing output.
- Cross-platform verification:
  - Windows `cargo fmt`, `cargo test`, `cargo clippy --all-targets -- -D warnings`,
    and `cargo build --release` pass.
  - Linux WSL2 Ubuntu 24.04 with Rust 1.96.0 passes `cargo fmt -- --check`,
    `cargo test`, `cargo clippy --all-targets -- -D warnings`, and
    `cargo build --release` using a separate Linux target directory.

## Checkpoint: repair memo + live DeepSeek v4-pro re-validation (2026-06-28)

- Added a self-correction memo to the tolerant tool runtime. When a tool call is
  accepted after a repair, the model-facing `ToolBatchResult` now carries a
  `hint` string telling the model the canonical wire tool name and argument shape
  to use next time, instead of only a bare `repaired: true` flag.
  - The memo references the API-callable wire name (`file_search`), never the
    internal dotted name (`file.search`) the OpenAI-compatible API rejects.
  - The repair detector now treats the advertised wire name as a non-repair, so a
    plain `file_write`/`file_read`/`file_list` call (the harness's own advertised
    names) is not falsely flagged or memo'd. Only genuine aliases (`write_file`,
    `grep`, `ls`, …) or wrong argument keys trigger a memo.
  - `file.list`/`file.search` no longer count an omitted optional `path` as a
    repair (`path` defaults to the workspace root).
  - The runtime also strips `null`-valued arguments the model should have omitted
    and runs the call anyway, marking it repaired so the model gets the memo.
  - TDD coverage added in `tests/tool_runtime.rs` (null strip, wire-name is not a
    repair), `tests/tool_scheduler.rs` (memo on repaired batch results), and
    `tests/agent_loop.rs` (memo reaches the model in the next request body and
    uses the callable wire name).
- Re-validated end-to-end against the official DeepSeek v4-pro API using the key
  found in `the local env` (loaded into `DEEPSEEK_API_KEY`; never
  written into `providers.json`, which stores only `key_env`).
  - Live trace shows the memo working as intended: round 1 the model called
    `file_search {pattern: ...}`, got the memo, and round 2 switched to the
    canonical `file_search {query: ...}` on its own. Of 8 tool results only the
    one genuine deviation was flagged; the rest were clean (no memo noise).
  - Prompt caching verified via two identical `chat once` calls: 84% then 98%
    hit ratio, with the harness reporting `hit_ratio_percent` and
    `saved_prompt_tokens` parsed from DeepSeek cache usage fields.
  - Artifacts saved under `artifacts/deepseek-v4-pro-memo-run/`.

## Checkpoint: bracketed paste — native paste without breaking the terminal (2026-06-28)

- Enabled bracketed paste mode in both interactive front ends so multi-line text
  and clipboard pastes arrive as one atomic event instead of a stream of key
  presses that submit on the first newline and corrupt the prompt.
  - REPL (`src/repl.rs`): new `ReplEvent::Paste(String)` inserts the pasted block
    verbatim (newlines preserved) and never submits; `RawModeGuard` now sends
    `EnableBracketedPaste`/`DisableBracketedPaste`; the event loop handles
    `Event::Paste`.
  - Setup TUI (`src/tui.rs`): `TuiApp`/`SetupTuiApp` gained `handle_paste`, the
    terminal enables/disables bracketed paste, and both run loops handle
    `Event::Paste`. A paste inside the provider wizard routes into the active
    field, so an API key can be pasted straight in; trailing clipboard newlines
    are stripped from single-line fields.
  - Image paste continues to work through the existing Ctrl+V clipboard capture
    path (PNG saved under `.harness/attachments`).
  - TDD coverage: `tests/repl.rs` (multi-line paste does not submit),
    `tests/tui_screen.rs` (paste fills the wizard API-key field and strips the
    newline; source check that both terminals enable bracketed paste and handle
    `Event::Paste`).

## Checkpoint: Ratatui chat TUI for the live session (2026-06-28)

- The interactive chat session now runs as a Ratatui TUI on a real terminal, not
  just line-mode raw output. New `ChatTuiApp` (`src/tui.rs`) holds a scrolling
  transcript, prompt editor, and bracketed-paste input; `render_chat_tui` draws a
  header (provider/model + workspace), the transcript, the message box, and a
  footer.
  - Streamed agent events fold into the transcript live: `ToolRoundStarted`,
    `ToolResult` (✓/✗ + tool name), and `FinalContentDelta` (assistant text
    accumulates in place as it streams).
  - Wired into `harness repl` / the default launch via `run_chat_tui`
    (`src/repl.rs`): a real TTY drives the chat TUI, while pipes and tests keep
    the line-mode `run_terminal_repl` so captured-output tests stay stable.
  - Reuses the bracketed-paste terminal setup so multi-line and image pastes do
    not break the screen.
  - TDD coverage in `tests/chat_tui.rs`: Enter submit/echo, empty-input no-op,
    Ctrl+C exit, paste without submit, agent-event ingestion, a `TestBackend`
    render assertion, and a source check that the CLI wires the chat TUI on a TTY.

## Checkpoint: chat TUI parity features (2026-06-28)

- Brought the chat TUI to OpenCode/qwen-code/pi-style interaction parity:
  - In-session slash commands parsed by `ChatTuiApp`: `/model PROVIDER MODEL`
    (returns a `SwitchModel` action that `run_chat_tui` resolves against the
    provider catalog and updates the header label live), `/history QUERY`
    (searches submitted prompts and lists matches), `/clear`, `/help`, `/exit`.
  - Command palette: `/help` opens a centered overlay listing every command and
    key binding; Esc closes it (Ctrl+C still exits the session).
  - Prompt history recall with Up/Down arrows over previously submitted messages.
  - Transcript scrollback with PageUp/PageDown; new output snaps the view back to
    the bottom, and the panel title shows the current scroll offset.
  - TDD coverage in `tests/chat_tui.rs` (14 tests total): `/model` switch action,
    `/help` open + Esc close, `/clear`, `/history` match listing, arrow-key
    recall, PageUp scroll + snap-to-bottom, and a `TestBackend` render of the
    palette.
- Verified the release binary still completes a real DeepSeek v4-pro `agent run`
  end-to-end after all TUI/REPL changes (created `hello.txt` = `OK`, 2 rounds).

## Checkpoint: launch UX fixes (2026-06-28)

- `harness repl` no longer hard-requires `--provider`/`--model`. New
  `resolve_repl_provider` (`src/cli.rs`) falls back to the first configured
  provider and its first model, so `harness repl` works once any provider is
  saved; with none, it returns an actionable error pointing at `/provider add`.
  Tested in `tests/cli.rs`.
- Note on "the TUI didn't change": the no-arg `harness` opens the **setup** screen
  (visually unchanged) until a provider exists at the default config path
  (`%APPDATA%\harness-cli\providers.json` on Windows, or `$HARNESS_CONFIG`); the
  Ratatui **chat** TUI only launches once a provider is configured. The installed
  PATH binary must also be refreshed with `cargo install --path . --bin harness
  --force` after rebuilds.

## Prod-ready chat TUI

- **Thinking + tool-call streaming.** `ChatResponse` now carries `reasoning`
  (`src/chat_client.rs`): DeepSeek `reasoning_content`/`reasoning`, Responses-API
  `reasoning` items, and Anthropic `thinking` blocks are all parsed. `AgentEvent`
  gained `Thinking(String)` and `ToolCallStarted { round, id, name, arguments }`,
  emitted by `AgentRunner` before each tool batch, plus an `AgentTraceEvent::Thinking`
  for `--trace`. Verified live against DeepSeek v4-pro: the trace shows `thinking`
  events and `model_tool_calls` with arguments, and the file was actually written.
- **Modern chat surface** (`src/tui.rs`). Transcript is now a structured
  `Vec<ChatEntry>` (User/Assistant/Thinking/Tool/System). Tool calls render as
  cards (`⏳`/`✓`/`✗` + name + compact args) that update **in place** by id when
  the `ToolResult` arrives, and the self-correction memo is surfaced as a `memo:`
  line. Thinking renders dim/italic. Rounded borders, role badges, a spinner while
  the agent runs, and a caret in the prompt. Help palette is generated from the
  command registry.
- **Slash-command autocomplete.** Typing `/` opens a floating menu filtered by
  prefix (e.g. `/p` → `/provider`); Up/Down select, Tab completes, Esc dismisses
  (without exiting), Enter runs. Registry: `/model /provider /history /clear /help
  /exit`. Tested by state (`completion_visible/_suggestions/_selected`) and render.
- **Paste without auto-submit + multi-line input.** The prompt is a multi-line
  buffer with a char cursor (Left/Right/Home/End, Shift/Alt+Enter inserts a
  newline). The chat read loop (`src/repl.rs`) coalesces a drained burst of key
  events into a single paste, so even on the **legacy Windows console** (where
  crossterm has no `Event::Paste`) a pasted newline becomes literal `\n` instead
  of a premature submit. Bracketed paste still takes the fast path where supported.
  Tested via `coalesce_chat_events` in `tests/repl.rs`.
- **Image/text clipboard in chat.** Ctrl+V returns `ChatTuiAction::CaptureClipboard`;
  the loop reads the system clipboard, inserts text at the caret or saves a PNG to
  `.harness/attachments` and references it in the prompt.

## Chat TUI follow-ups (mouse, markdown, SSE)

- **Mouse-wheel scroll.** `ChatTerminal` now enables `EnableMouseCapture`; the chat
  loop maps `Event::Mouse(ScrollUp/ScrollDown)` to `ChatTuiApp::scroll_back/forward`.
  Fixes the bug where the wheel was translated by the terminal into Up/Down arrows
  and misread as prompt-history recall. Tested via `coalesce_chat_events`.
- **Markdown rendering.** Assistant answers render through `markdown_lines`
  (`src/tui.rs`): ATX headings, `-`/`*` bullets, fenced code blocks, horizontal
  rules, table-separator rules, and inline `**bold**`/`*italic*`/`` `code` ``. The
  plain-text projection (`transcript_text`) stays raw so pipes/tests are unaffected.
- **SSE streaming in the agent loop.** Extended the OpenAI SSE reader
  (`read_openai_stream_full`, `StreamDelta`, `stream_chat`) to accumulate content +
  `reasoning_content` + tool-call argument fragments by index into a full
  `ChatResponse` while emitting fragments live. `AgentRunner::with_streaming(true)`
  (opt-in; the chat TUI sets it, tests stay blocking) streams `Thinking` and
  `FinalContentDelta` deltas, deduped against the end-of-turn emit. `agent run`
  gained a `--stream` flag. Thinking deltas coalesce in the TUI via
  `streaming_thinking`. Verified live on DeepSeek v4-pro: a streamed two-round
  tool-calling run (`file_write` → `file_read`) parsed tool args from SSE fragments
  and wrote the file.

## Remaining work

- Chat TUI further polish: an interactive model-selector list (instead of typing
  `/model PROVIDER MODEL`), a scrollbar gutter, and transcript virtualization for
  very long sessions. True multimodal vision payloads (sending image bytes, not just
  a path reference) and streaming for the Anthropic/Responses formats (currently the
  OpenAI-compatible path streams; others fall back to blocking) also remain.
  See `docs/tui-reference-analysis.md`.
- More LLM request adapters beyond OpenAI-compatible, OpenAI Responses, OpenAI
  Codex Responses, and Anthropic Messages chat.
- Provider-specific OAuth/browser subscription flows where API-key metadata is not enough.
- More API-specific prompt-cache body variants beyond the generic `cache_control` text-block marker.
- Deeper tool runtime cancellation for already-running non-shell tools, richer intent repair,
  and broader tool catalog beyond file/shell basics.
- 2026-07-02: a full multi-agent audit produced `docs/improvement-audit-2026-07.md` —
  76 verified findings (hangs, loop-prevention, LLM-error tolerance, Windows/Linux/macOS
  correctness, perf/RAM, TUI) with a four-wave implementation roadmap. Top items: shell
  timeout doesn't kill the process tree, tool-worker panic hangs the agent forever, the
  global 60s ureq deadline kills long streams, CRLF breaks `file.replace` on Windows,
  and macOS can't even compile the test suite.
- 2026-07-06: chat TUI restyled to a minimal Claude-Code-like look (spec:
  `docs/superpowers/specs/2026-07-06-chat-tui-restyle-design.md`): borderless
  transcript with blank-line separated blocks, `>` user echo, `●`/`⎿` tool cards,
  reasoning shown as unlabeled dim italic (no `think` badge), single-accent cyan
  palette. Input compose now soft-wraps with hand-computed caret position (the old
  caret froze once the line hit the border), and the `/`-command menu moved from a
  floating popup to inline rows below the input with the typed prefix highlighted.
  Single-call tool rounds no longer print a `tool round` banner.
- 2026-07-06 (fixes after live use): key *release* events (Windows delivers them
  for the Enter that launched the binary or submitted the prompt) no longer
  coalesce into a phantom pasted newline that pushed the caret to a second input
  row; transcript scrolling now wraps lines by hand and windows in exact visual
  rows — clamped at the oldest entry, inert when everything fits (no more
  bottom-row eating or stuck `↑ N` indicator), wrapped tails stay visible; user
  messages render with a cyan `>` marker and regular text instead of the dim gray
  that blended into reasoning.
- 2026-07-06 (round 3, transcript polish): wrapped lines now hang-indent to their
  block's text column; Markdown tables render as aligned padded columns (pipes
  parsed into cells, header bold with per-cell underline, inline code inside cells
  works); multi-line tool results show a line count ("13 lines · first entry");
  tool cards display the canonical `file.*` name instead of the model's wire
  alias (`runtime::canonical_tool_name`); user turns sit on a dark highlight
  strip (256-color 236) with a cyan `>` marker, Claude-Code-style; status-line
  hints no longer overwrite the provider label on narrow terminals.
- 2026-07-06 (round 4, forgiving shell + chat limits): `shell.exec` now repairs
  Unix-isms before running under Windows PowerShell 5.1 — `&&` chains rewritten
  as `; if ($?) { ... }` via a quote-aware splitter (stop-on-failure semantics
  kept, `2>&1` untouched), and a leading `cd` into a nonexistent directory
  (hallucinated `/mnt/<project>`) is dropped since commands already run in the
  workspace; each rewrite sets `repaired`, stores `repair_note`/`original_command`
  in metadata, and `from_execution` prefers that note over the generic repair
  memo. Failed commands now include stderr in model-visible content (a PS parse
  error used to surface as a bare "failed"). Interactive repl/chat is no longer
  round-capped by default (`--max-rounds N` still applies); `agent run` keeps 4.
  Help palette documents Shift+drag text selection (mouse capture eats plain drag).
- 2026-07-06 (round 5, environment awareness): the agent loop now appends one
  environment line to the system prompt (`prompt::agent_system_prompt`): OS,
  shell dialect (PowerShell 5.1 with an explicit "no && / ||" warning on
  Windows), and the workspace root with the two facts that actually matter —
  commands already run there, and `cd` does not persist between shell calls
  (each shell.exec is a fresh process). Models no longer have to guess and
  invent `/mnt/<project>` paths. The line is constant per session, so the
  provider cache prefix over the system prompt stays stable across rounds;
  `DEFAULT_SYSTEM_PROMPT` itself is unchanged (chat once/stream still use it).
- 2026-07-11 (session & trace persistence): every agent run now auto-saves its
  raw trace to `~/.harness/projects/<workspace-slug>/traces/<ts>_<provider>_r<turn>.json`
  (`HARNESS_HOME` overrides the base dir; REPL, chat TUI, and one-shot
  `agent run`, on success and on error paths) for offline analysis of model
  quality and bad tool calls (original wire names/arguments in
  `model_tool_calls`, `ok`/`error`/`repaired`/`hint` in `tool_result`). The REPL
  and chat TUI are now multi-turn: `AgentRunner::with_history` replays prior
  `ChatMessage`s (appended after the stable cache prefix), `AgentRunResult`
  returns the full post-run message list, and each conversation is persisted as
  an append-only session JSONL (`sessions/<ts>_<id>.jsonl` + `last` pointer;
  `meta`/`message`/`thinking` records — thinking is stored but never replayed).
  Launching `harness` resumes the workspace's last session; `/new` starts a
  fresh one. Resume skips corrupt lines and trims dangling tool calls; all
  persistence failures degrade to warnings instead of killing the chat.
  New module `src/session.rs` (SessionStore/Session/ChatSession/TraceWrapper),
  design spec in `docs/superpowers/specs/2026-07-11-session-trace-persistence-design.md`.
- 2026-07-11 (round 2, interruptible agent): Esc/Ctrl+C now stop a running agent
  in the chat TUI. The run executes on a worker thread (`std::thread` + `mpsc`
  events, no async runtime) while the UI loop keeps polling input; a shared
  `AtomicBool` cancel flag is checked between SSE chunks
  (`read_openai_stream_full`), after each provider response, and before each
  tool round. Cancelled runs return `AgentError::Cancelled` carrying the partial
  trace (persisted with an `interrupted by user` error event), the transcript
  shows `Interrupted by user`, and the session stays open — idle Esc/Ctrl+C
  still exit. Transcript scrolling works while the agent is busy; queued Enter
  presses are dropped during a run so they cannot double-submit. Line-mode REPL
  (non-TTY pipes) remains synchronous by design.
- 2026-07-11 (round 3, working-row polish): the chat TUI `Working…` row now
  shows elapsed run time (`Working… (12s)`, `(2m 5s)` past a minute, updated on
  every busy-loop redraw) and is separated from the transcript by a blank
  spacer line.
- 2026-07-11 (harness-tui phase 1, foundation): new `crates/harness-tui`
  workspace crate — our own TUI library that will replace ratatui + crossterm
  (design spec `docs/superpowers/specs/2026-07-11-harness-tui-design.md`).
  Landed: styled line/span text model with unicode-aware `visible_width` and
  grapheme-safe styled word wrap; minimal row diff between frames
  (`diff_frames`, tail-append touches only tail rows); headless `TestTerminal`
  whose snapshots show text, styles, and the drawn caret (`[ ]{reverse}`); and
  a platform terminal layer with hand-written FFI (kernel32 / libc symbols —
  no crossterm): tty check, size query, raw input mode, VT enable,
  synchronized-output frames (`CSI ?2026`), hidden hardware cursor, bracketed
  paste, and guaranteed restore on drop + panic hook (`install_panic_restore`).
  Manual check: `cargo run -p harness-tui --example smoke`. 41 new tests
  (`crates/harness-tui/tests/{text,diff,headless,terminal}.rs`); only deps are
  `unicode-width` + `unicode-segmentation`. Next phases: input parser, core
  loop, components, chat/setup migration.
- 2026-07-12 (harness-tui phase 5, setup TUI + stack removal): the setup TUI
  (`src/tui.rs`) now runs on `harness-tui` — state machines take
  `harness_tui::input::KeyEvent`s, rendering is pure `setup_lines` /
  `setup_tui_lines` (`Vec<Line>` with a reverse-cell caret, wizard steps
  replacing the command list while the dialog is open), and the terminal loops
  reuse the REPL `InputPump` (now `pub(crate)`) over `Screen::stdout` with
  400ms polling, per-tick resize checks, and `release()` on exit.
  `SetupTerminal` is gone, and `ratatui` + `crossterm` are removed from
  Cargo.toml entirely; `tests/tui_screen.rs` asserts on line text instead of
  ratatui buffers and `tests/tui_stack.rs` now guards that the legacy stack
  stays out of the manifest.
- 2026-07-12 (codex review round): adversarial correctness review
  (`agents/codex-review-harness-tui.md`, 19 findings) with 13 fixed same-day:
  Alt+Enter now decodes as Enter+alt (was a literal `\r`); parser `flush()`
  keeps torn UTF-8; a stale `Running` tool card from a cancelled run no longer
  freezes the next turn's scrollback flush (regression test); `Screen` reserves
  the cursor row with an empty panel, tail-clips oversized panels, and uses
  saturating origin math; Windows console gets `DISABLE_NEWLINE_AUTO_RETURN` +
  `ENABLE_PROCESSED_OUTPUT` (exact-width status row no longer scrolls the
  panel) and input code page forced to UTF-8 (Cyrillic input) with restore;
  unix raw mode pins `VMIN=1/VTIME=0`; non-Linux unix targets are a
  compile_error (the FFI layer is linux-gnu only); the stdin reader is one
  process-wide thread (setup→chat handoff can't race stdin), EOF/read errors
  end the TUI loops instead of idle-spinning; busy loop re-checks terminal
  size and consumes queued Esc as cancel before the finished check. Deferred
  (documented in the review file): resize leaves the old panel footprint on
  width-only changes, torn escapes on >3ms read splits, legacy burst-coalesce
  semantics, DSR robustness, guard non-composability, worker survival on draw
  errors, char-count (not width) panel row caps for CJK.
- 2026-07-12 (post-migration UX fixes): `/new` and `/clear` now wipe the
  terminal screen and scrollback via the new `Screen::clear` (CSI 2J + 3J,
  origin reset, panel forgotten) — previously only the app transcript was
  dropped and the old conversation stayed visible; the chat input field is
  drawn inside a rounded full-width frame (`framed_lines` in `src/chat.rs`,
  rows clipped/padded to the inner width), and the panel reserve accounts
  for the two border rows.
- 2026-07-12 (bottom-pinned panel): `Screen` now keeps a content cursor
  separate from the panel position — content flows top-down into native
  scrollback while the panel is always painted on the bottom rows of the
  window (the input field no longer floats right under the last block near
  the top of an empty screen). A growing panel scrolls content up to make
  room; a shrinking one clears its stale rows. Also fixed a session-id
  collision: `create_session` bumps its millisecond seed until the path is
  fresh, so `/new` in the same millisecond no longer appends to (and later
  resumes) the session it just abandoned — this was a real flake on tmpfs.
- 2026-07-12 (full-window takeover): `Screen::takeover` scrolls the shell's
  leftover screen content into native scrollback and starts the TUI with a
  blank viewport — chat and setup claim the whole window on launch (content
  from the top row, panel pinned to the bottom) instead of attaching below
  the shell prompt. The shell banner stays reachable by scrolling up.
- 2026-07-12 (reference screen model, codex round 2): after live feedback,
  the bottom-pinned panel and newline takeover were replaced with the model
  the references actually use (verified against pi, opencode and qwen-code;
  full analysis in `agents/codex-screen-flow-report.md`): startup claims the
  viewport with CSI 2J + H (no 3J - user scrollback survives), the whole UI
  is one contiguous top-down flow (content, then the input frame directly
  below - never `height - panel_len`), and `Screen::present(committed,
  live)` commits finalized rows and paints the next live frame in one
  synchronized write with the reserve computed from the new frame.
  `ChatApp::peek_scrollback`/`acknowledge_emitted` move the commit boundary
  only after the terminal write succeeds; resize keeps the content-following
  origin. Deferred from the report: progressive commit of streaming
  responses taller than the live budget (Qwen Static/pending model),
  debounced full-replay resize.
