# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`harness-cli` is a lightweight Rust prototype of an LLM harness CLI (like `claude` / `codex`).
It focuses on cache-friendly provider requests, multi-format chat routing, a small system
prompt, an agent tool-calling loop, and "forgiving" workspace-bounded file/shell tools that
tolerate imperfect LLM tool calls. It runs natively on Windows and Linux.

The installed binary is `harness`. `README.md` is the user-facing command catalog;
`goal.md` is the running implementation checkpoint/changelog. Keep both in mind — when you
add a capability, the convention in this repo is to append a bullet to `goal.md` (and usually
`README.md`).

## Build, test, lint

```powershell
cargo build --release        # release build (binary at target/release/harness.exe)
cargo test                   # full test suite (unit + integration in tests/)
cargo fmt                    # format; CI gate is `cargo fmt -- --check`
cargo clippy --all-targets -- -D warnings   # lint; warnings are errors
```

Run a single integration test file or a single test by name:

```powershell
cargo test --test agent_loop                 # one tests/*.rs file
cargo test agent_runner_executes_tool_calls  # one test fn by (substring) name
```

Install / run the interactive binary:

```powershell
cargo install --path . --bin harness --force
harness                       # launches REPL if a provider is saved, else the setup TUI
```

Linux verification is run under WSL2 with a **separate target dir** so it does not clobber the
Windows `target/` (Windows and Linux artifacts are incompatible):

```bash
CARGO_TARGET_DIR=/tmp/rust-harness-target-linux cargo test
CARGO_TARGET_DIR=/tmp/rust-harness-target-linux cargo clippy --all-targets -- -D warnings
```

Toolchain: edition 2024, Rust 1.96+. Runtime deps are deliberately minimal: `ureq` (blocking
HTTP), `serde`/`serde_json`, `blake3`, and the in-repo `harness-tui` crate (whose only deps
are `unicode-width` + `unicode-segmentation`). `tempfile` is dev-only. The repo is a cargo
workspace: `crates/harness-tui/` is our own terminal UI library (line-based rendering,
native-scrollback screen, own input parser, own platform FFI — no ratatui/crossterm).

## Architecture

The crate is both a library (`harness_cli`, `src/lib.rs`) and a binary. **Two identical entry
points** — `src/main.rs` and `src/bin/harness.rs` — both just call `cli::run_terminal`; the
real logic lives in the library so the integration tests in `tests/` can exercise it directly.

Request/response flows through these layers (most central modules):

- **`cli.rs`** — the dispatcher. No `clap`; commands are matched by **slicing `args`**
  (`[scope, command, rest @ ..]`). Every subcommand (`provider`, `chat`, `agent`, `tool`,
  `clipboard`, `repl`, `diagnostics`) is a function here. With no args + a TTY it routes to
  the setup TUI or REPL; otherwise it falls back to line mode (this is what keeps tests/pipes
  out of raw terminal mode).
- **`request.rs`** — `RequestEnvelope`, the cache-aware request model. It produces two BLAKE3
  keys: `cache_prefix_key()` over the *stable* prefix (provider, model, system prompt, tools)
  and `full_request_key()` over everything. Changing the prefix invalidates provider cache;
  keep it stable.
- **`prompt.rs`** — the single `DEFAULT_SYSTEM_PROMPT` const, intentionally kept short
  (500–1000 token target) for cache friendliness. Treat its size as a design constraint.
- **`providers.rs`** — `ProviderConfig` (auth scheme, cache policy, chat-api format, key/env)
  plus `BuiltinProvider` profiles (`codex`, `xiaomi`, `glm`, `kimi`, `claude`, `deepseek`).
  Saving a provider whose name matches a builtin family inherits that profile's metadata.
- **`chat_client.rs`** — `ProviderChatClient::send` dispatches on `ChatApiFormat` to four
  adapters: OpenAI-compatible chat, OpenAI Responses, OpenAI Codex Responses, and Anthropic
  Messages. This is where request bodies, auth headers, cache markers, and usage/cache-metric
  parsing diverge per provider. Add new provider request shapes here.
- **`agent.rs`** — `AgentRunner`, the tool-calling loop: build `RequestEnvelope` → `send` →
  if the model returned tool calls, run them through `ToolScheduler`, append assistant
  tool-call + tool-result messages, repeat until a final answer or `max_tool_rounds`.
  Failed tools are returned to the model as JSON results, not aborted. Emits `AgentEvent`s
  (consumed by the REPL) and builds an `AgentTrace` (the `--trace` / `--tool-errors` output).
- **`runtime.rs`** — the tool runtime. `ToolRuntime::execute` is the canonical dispatch over
  `file.read`/`file.write`/`file.append`/`file.replace`/`file.list`/`file.search`/`file.tail`/
  `file.hash`/`file.stat`/`file.delete`/`file.move`/`attachment.read`/`shell.exec`.
  `ToolScheduler` runs a batch with bounded concurrency via `std::thread` + `mpsc`,
  preserving input order and supporting a batch deadline that marks unfinished calls cancelled.
- **`tools/`** — the actual implementations: `files.rs` (workspace-bounded file ops),
  `shell.rs` (native shell exec with timeout + bounded stdout/stderr capture), `attachments.rs`
  (reads only from `.harness/attachments` or `.codex/attachments`).
- **`platform.rs`** — `ShellProfile::native()` picks `powershell.exe -NoProfile …` on Windows,
  `bash -lc` on Linux. Shell tool behavior is platform-specific by design.
- **`config.rs`** — `ConfigStore`, a JSON file (default `.harness/providers.json`) holding a
  `BTreeMap<String, ProviderConfig>`. Missing file = empty config (not an error).
- **`repl.rs`** / **`chat.rs`** / **`tui.rs`** — interactive front ends, all on `harness-tui`.
  `chat.rs` is the chat state machine (`ChatApp`: editing, slash commands, completions,
  agent-event ingestion, markdown, panel/scrollback rendering); `repl.rs` owns the terminal
  loops (`run_chat_tui` on `harness_tui::core::Screen` + `InputPump`, plus the non-TTY line
  REPL); `tui.rs` is the setup screen (no-provider onboarding). Chat history is printed into
  the terminal's native scrollback; only the bottom panel is diffed and repainted.
- **`clipboard.rs`** — text/PNG capture into `.harness/attachments` with a prompt fragment;
  has a native system reader plus a `StaticClipboard` for tests.
- **`diagnostics.rs`** — process RSS / binary-size checks, JSON output.

### The "forgiving tools" convention (important when touching `runtime.rs` or `tools/`)

Tool calls from models are assumed to be imperfect. Two repair stages run before execution and
both set a `repaired` flag on the result (rather than failing):

1. **`ToolResolution::from_name`** normalizes and aliases tool names — `write_file`,
   `file_write`, `grep`, `ls`, `rm`, `edit_file`, etc. all map to canonical `file.*`/`shell.exec`.
2. **`repair_tool_arguments`** coerces a raw string or a leftover `_raw_arguments` field into a
   structured object, parses `key: value` / `key=value` lines, and individual executors accept
   many argument aliases (`path`/`file`/`filename`/`file_path`, `content`/`text`/`contents`,
   `old_string`/`new_string`, Codex-style `source_path`/`destination_path`) plus string→number
   /bool coercion (`"lines": "2"`, `"overwrite": "true"`).

When extending tools, **add the new alias to the resolver / arg list rather than rejecting the
call**, and surface `repaired: true` so the model can learn from the next turn. Unrepairable
arguments are preserved under `_raw_arguments` so the agent can recover instead of crashing.

## Testing conventions

- This is a **TDD codebase** — there are ~190 tests across `tests/*.rs`, roughly one file per
  module (`agent_loop`, `chat_client`, `tool_runtime`, `provider_profiles`, `tui_screen`, …).
  Add tests alongside any new behavior.
- Network code is tested with **real in-process mock servers**: tests bind a `TcpListener` to
  `127.0.0.1:0`, spawn a thread that reads the raw HTTP request and writes a canned response.
  Follow `tests/agent_loop.rs` / `tests/chat_client.rs` as the pattern — there is no HTTP mock
  library; you assert on the raw request body and reply with literal JSON.
- File/clipboard tests use `tempfile::tempdir()` for an isolated workspace.
- Error types are hand-rolled enums implementing `Display` + `Error` + `From` (no `anyhow`/
  `thiserror`). Match that style for new errors.

## Notable constraints

- Keep `DEFAULT_SYSTEM_PROMPT` short and the `cache_prefix_key` inputs stable — both exist for
  prompt-cache efficiency.
- No async runtime; reach for `std::thread`/`mpsc` and blocking `ureq`, not `tokio`.
- File and attachment tools are **workspace-bounded** — do not introduce arbitrary
  absolute-path reads/writes.
- Shell tools must stay bounded (timeout + capped stdout/stderr) so a runaway command cannot
  hang or flood the agent.
