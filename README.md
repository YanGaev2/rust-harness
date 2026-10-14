# harness-cli

> **1007 tool calls across 5 model families. 0 failed. 0 repairs needed.**

A **4 MB** Rust agent harness (Windows + Linux) that speaks your model's
native tool dialect: tool names, argument shapes, and schemas are
**measured from each model's training priors** — not guessed, not imposed.

## Why

- ⚡ **Invisible footprint** — 4.2 MB static binary, **5.6 MB RAM**, ~34 ms
  cold start. No tokio, no Node, no Electron: blocking I/O, `std::thread`,
  and an in-repo terminal UI library.
- 🎯 **Tools fitted to the model, not the other way around** — before a
  model ships as a preset, a probe suite measures which tool names and
  argument conventions it absorbed in training, and the harness declares
  exactly those. Measured effect: **−46% tool calls** on deepseek-v4-pro
  after renaming tools to its own priors.
- 🔧 **Self-repairing tool calls** — a sloppy call gets fixed, not failed:
  name aliases (`grep` → `file.search`), argument coercion, CRLF
  re-encoding — plus a `repaired` memo so the model self-corrects. A
  malformed call costs **zero extra round-trips**.
- 💾 **Cache-first by construction** — byte-stable request prefix + BLAKE3
  cache keys keep provider prompt caches hot (**up to 85%** of prompt
  tokens billed at the cached rate); `/cost` shows a live, cache-aware
  session price.
- 🔌 **Any OpenAI-compatible API** — bench-verified presets, Anthropic and
  Responses formats, per-provider proxy (`url | env | none`), and an
  interactive in-chat model picker.

Every claim above is verified on a 26-task agent benchmark (×2 repeats,
real APIs, real filesystem):

| Model | Bench | Tool calls | Repairs / failures | Agent-loop cache hit |
|---|---|---|---|---|
| glm-5.2 | **52/52** | 185 | 0 / 0 | ~85% |
| mimo-v2.5-pro | **52/52** | 192 | 0 / 0 | 0% (no cache markers yet) |
| deepseek-v4-pro | 51/52 | 218 | 0 / 0 | ~85% (64-token blocks) |
| qwen3.6-35b-a3b | 51/52 | 214 | 0 / 0 | 54% |
| qwen3.7-max | 50/52 | 195 | 0 / 0 | 30% (2048-token minimum) |
| gpt-5.6-luna | 48/52 | 195 | 0 / 0 | 44% |

Every benchmark miss is model-side (mental arithmetic, safety refusals) or
a transient upstream error — **across 1199 real tool calls the tools
themselves never broke once**. The bench also audits the harness itself:
one run exposed a scheduler race that silently dropped two of three
same-file edits, now fixed and covered by deterministic regression tests.

<details>
<summary><b>How tools get fitted to a model</b></summary>

Before a model family is marked supported, we run a probe suite against it:
free-form completions at several temperatures, "list your tools" prompts,
and live function-calling rounds with empty schemas — extracting which tool
names, argument names, and value conventions the model absorbed in training
(`read_file` vs `file_read`, `old_string` vs `old_str`, timeout in seconds
vs milliseconds). Then the harness declares exactly those names and JSON
Schemas, and the 26-task benchmark must pass before the pair ships as a
preset.

The shell tool detects the interpreter that actually exists on the machine
(pwsh 7 → Windows PowerShell 5.1 → cmd.exe on Windows; bash → POSIX sh on
Linux/docker) and tells the model which dialect it is writing for.
Environments without any shell (distroless containers) simply don't
advertise the shell tool.

Tool results are token-lean and honest: empty searches say "no matches"
instead of an empty string, shell metadata never repeats stdout/stderr,
and `read_file` decodes UTF-16 files (PowerShell `>` redirects) instead of
silently returning empty content.

</details>

<details>
<summary><b>Cache economics, measured</b></summary>

Every request keeps a byte-stable prefix (system prompt ~700 tokens, tool
schemas, provider metadata) so provider-side prompt caches hit on every
agent round. The request model exposes two BLAKE3 keys (`cache_prefix_key`,
`full_request_key`) and the session cost tracker prices cached and fresh
tokens separately.

| Model | Cache behaviour | $/1M in / cached / out |
|---|---|---|
| deepseek-v4-pro | 64-token blocks, hits from the 2nd request | 0.435 / 0.0036 / 0.87 |
| glm-5.2 | ~85% hit from round 2, 97% on prefix repeat | 1.40 / 0.26 / 4.40 |
| qwen3.7-max | caches only prefixes ≥ 2048 tokens | vendor pricing not verified |
| gpt-5.6-luna (OpenRouter) | 44% across the bench | ≈ 1.0 / — / 6.0 (from live `cost_details`) |

A live glm-5.2 session (2 requests, 3 371 prompt tokens of which 1 600
cached, 35 completion) cost **$0.0030**; `/cost` in the REPL shows the same
numbers per session, cache-aware.

</details>

## Install and launch

Install the short launcher into Cargo's bin directory:

```powershell
cargo install --path . --bin harness --force
```

Then start the interactive interface like other terminal agents:

```powershell
harness
```

On Windows Terminal, add a profile tuned for the harness (an app cannot
change the terminal font, but Windows Terminal reads profile fragments):

```powershell
harness terminal-setup            # adds the "Harness" profile
harness terminal-setup --remove   # removes it
```

The profile runs PowerShell with Cascadia Mono, line height 1.2, real bold
(`intenseTextStyle: bold`; the default only brightens bold text), padding,
and a Harness Dark/Light color scheme pair. Your `settings.json` does not
change.

On first launch, if no provider is configured yet, `harness` opens a
harness-tui terminal setup screen with a status header, workspace/config
paths, command hints, and the `[no provider] >` prompt. From there, run
`/provider add` to start the provider wizard:

1. Select a built-in family, or `custom` for any OpenAI- or Anthropic-compatible API.
2. For `custom`, type the provider name.
3. Type the base URL (the address before `/models`). Built-in families fill it in.
4. Type or paste the API key. Enter sends `GET {base URL}/models`.
5. Select a model, or `Add all`. Type to filter the list. Enter saves.

The wizard reads the model ids, the context window (`context_length`,
`context_window`, `max_model_len`, Anthropic `max_input_tokens`) and the
OpenRouter prices from the list. It saves them to `model_info` in
`providers.json`. The chat uses the saved window for its context meter and the
saved prices for `/cost`. A `custom` provider that rejects the Bearer key (401/403)
gets a second request with `x-api-key` and `anthropic-version`. When that request
succeeds, or the list has the Anthropic shape, the provider uses the Anthropic
Messages API. When the request fails, Enter goes back to the URL and Tab lets you
type the model id. After the save, the same launch starts the REPL with that provider; later
`harness` opens the REPL directly. Non-interactive runs fall back to the
line-mode setup screen so tests and pipes do not enter raw mode.

## Current commands

List models from an OpenAI-compatible provider:

```powershell
harness provider models --name custom --url https://api.example.com/v1 --key $env:API_KEY
```

List built-in subscription profile hints:

```powershell
harness provider subscriptions
```

List saved providers without printing API keys:

```powershell
harness provider list --config .harness/providers.json
```

Save every discovered model into a local config:

```powershell
harness provider add --config .harness/providers.json --name custom --url https://api.example.com/v1 --key $env:API_KEY --add-all
```

Interactively enter provider details, list models, and choose `0) Add all` or a model number:

```powershell
harness provider add --interactive --config .harness/providers.json
```

Save selected models:

```powershell
harness provider add --config .harness/providers.json --name local-openai --url http://localhost:11434/v1 --key local-key --model qwen3-coder
```

Save a provider without writing the API key into the config:

```powershell
harness provider add --config .harness/providers.json --name gateway --url https://api.example.com/v1 --key-env GATEWAY_API_KEY --model gateway-model
```

Save a provider whose API expects a custom auth header:

```powershell
harness provider add --config .harness/providers.json --name header-gateway --url https://api.example.com/v1 --key-env HEADER_GATEWAY_API_KEY --auth header --auth-header x-api-key --add-all
```

Route traffic through a proxy — **strictly config opt-in**. Ambient
`HTTP_PROXY`/`HTTPS_PROXY` environment variables are ignored unless the
config says `env`, so the environment can never silently reroute requests:

```powershell
harness proxy set http://user:pass@127.0.0.1:8080     # config-wide proxy
harness proxy set env                                  # opt into HTTP(S)_PROXY env vars
harness proxy show                                     # global + per-provider view
harness provider add --config .harness/providers.json --name behind-proxy --url https://api.example.com/v1 --key-env KEY --model m --proxy http://127.0.0.1:8080
```

A per-provider `--proxy` (URL, `env`, or `none`) overrides the global one;
`none` forces that provider direct even when a global proxy is set.

Save a custom provider that exposes DeepSeek-style automatic cache metrics:

```powershell
harness provider add --config .harness/providers.json --name cache-gateway --url https://api.example.com/v1 --key-env CACHE_GATEWAY_API_KEY --cache automatic --cache-hit-field prompt_cache_hit_tokens --cache-miss-field prompt_cache_miss_tokens --model deepseek-v4-pro
```

Save a provider that should use the OpenAI Responses API instead of Chat Completions:

```powershell
harness provider add --config .harness/providers.json --name openai-responses --url https://api.openai.com/v1 --key $env:OPENAI_API_KEY --chat-api openai-responses --model gpt-5
```

Save a provider that should use the Codex-specific Responses route:

```powershell
harness provider add --config .harness/providers.json --name codex --url https://api.openai.com/v1 --key-env OPENAI_API_KEY --chat-api openai-codex-responses --model gpt-5-codex
```

Known built-in names reuse their profile metadata when saved this way, so
`--name claude` stores Anthropic `x-api-key` auth and native Messages API routing.
Custom providers can set `--chat-api openai-compatible`, `--chat-api openai-responses`,
`--chat-api openai-codex-responses`, or `--chat-api anthropic-messages`.

Run one cache-aware chat request through a saved provider:

```powershell
harness chat once --config .harness/providers.json --provider custom --model deepseek-v4-pro --message "write notes.txt"
```

Stream an OpenAI-compatible chat response as text deltas:

```powershell
harness chat stream --config .harness/providers.json --provider custom --model gpt-stream --message "hello"
```

Run an agent loop that executes returned tool calls and continues until the model
returns a final answer. Add `--stream` to consume the response over SSE
(OpenAI-compatible providers) instead of a single blocking request:

```powershell
harness agent run --config .harness/providers.json --workspace . --provider custom --model deepseek-v4-pro --message "write notes.txt" --max-rounds 4 --max-tool-concurrency 4 --tool-timeout-ms 10000 --stream
```

Add `--retain-thinking` to replay each assistant turn's reasoning back to the
provider (`reasoning_content` on OpenAI-compatible APIs, an unsigned `thinking`
block on Anthropic-format ones). Off by default — reasoning is always captured
in traces and session files either way. Thinking-mode DeepSeek V4 / Kimi / MiMo
require the echo-back on tool-call turns, so switch it on for those models; the
Responses format has no replayable form and ignores the flag.

Save a full JSON trace and a separate tool-error report for an agent run:

```powershell
harness agent run --config .harness/providers.json --workspace . --provider custom --model deepseek-v4-pro --message "write notes.txt" --trace artifacts/agent-trace.json --tool-errors artifacts/tool-errors.json
```

Chain fallback models for `agent run` by dropping a `fallback.json` next to
`providers.json` — on an error classified as switchable (quota-exhausted 429,
402, 5xx, overloaded, network) the run continues on the next provider/model
pair, with a `⚠ provider switch` line in the UI and a `provider_switched`
event in the trace. Auth failures, context overflow, and content-policy
errors never switch (a fallback would only mask them); short rate-limit
waits (`Retry-After` ≤ 10s) retry in place first:

```json
{
  "chain": [
    { "provider": "mimo", "model": "mimo-v2.5-pro" },
    { "provider": "deepseek", "model": "deepseek-chat" }
  ]
}
```

Loop guardrails watch every agent run: a tool call repeated verbatim after
failing (or returning byte-identical read-only results) first gets a warning
attached to its result, then is blocked with a synthetic result telling the
model to diagnose instead of retrying — the run itself keeps going.

Network-backed `provider models`, `provider add --add-all`,
`provider add --interactive`, `chat once`, `chat stream`, `agent run`, and
`repl` commands accept `--timeout-ms N` to tighten request timeouts when needed.

Start the interactive REPL. On a real terminal it opens the **harness-tui chat
TUI** (our own line-based library in `crates/harness-tui`, no ratatui/crossterm);
piped/non-TTY callers fall back to line mode. The chat TUI:

- **Streams** responses over SSE (OpenAI-compatible providers). Text appears
  at a steady pace, not in network bursts: the display trails the stream by
  about 0.3 s, and the newest characters fade in from the background color.
  The spinner and the `Working…` shimmer run on a clock, so they do not
  speed up when deltas arrive fast.
- Shows **reasoning** by default: a `∴ Thinking… 4s` header (then `∴ Thought
  for 4s`) over the newest four lines of each block. Ctrl+O expands every
  block in place and folds it back. `/thinking` hides reasoning and shows it
  again.
- Shows each **tool call** as a card. The status glyph is a spinner while the
  call runs, then `✓`, `✗`, or `○` (interrupted). The card shows the tool name
  and its main argument (command, query, pattern, path, URL), not raw JSON.
  Calls of 2s and longer show their time. `shell.exec` cards show the last
  five output lines and a non-zero exit code, also when the command failed;
  a timeout shows its reason. `file.replace` cards show a diff on tinted
  rows. A `memo:` line appears when the forgiving runtime
  corrected the call.
- Runs **full screen** (the alternate screen buffer). The editor and the
  status line stay on the bottom edge while the transcript scrolls above
  them: mouse wheel, PgUp/PgDn. When you scroll up, the rule above the
  editor shows `↓ 42 more rows · PgDn`, and new output does not move the
  view; sending a message jumps back to the newest rows. Your turns are `›`
  lines on a full-width strip; answers have no marker. Below the transcript:
  the `⠹ Working… (12s · ↓ 1.2k tokens · esc to interrupt)` row, the editor
  between two rules, and the status line. The status line shows the
  workspace and git branch on the left, the model and session tokens
  (`↑12.3k ↓3.4k · cache 85%`) on the right. Key hints appear there only
  when they apply.
- Shows **how full the context window is** at the right end of the status
  line: `████▌░░░░░ 474k/1M 45%`. The count is the latest request's prompt
  plus completion, i.e. the conversation the next request carries. The
  window comes from the provider's `/models` list (`context_length`,
  `context_window` or `max_model_len`), fetched in the background at start
  and after `/model`. The bar turns orange at 60% and red at 85%. When the
  provider does not report a window (DeepSeek), the line shows `ctx 45.2k`.
  On a narrow terminal the session totals go first, then the model name.
- **Select with the mouse**: drag over the transcript to highlight text; on
  release it goes to the clipboard (OSC 52) and the status line shows
  `copied N chars`. The right button pastes. Shift+drag still gives the
  terminal's own selection.
- **On exit** the terminal shows its own screen again, as it was before the
  session, and the conversation is printed into its scrollback (all text
  revealed, no key hints).
- Renders assistant answers as **Markdown**: headings, bullet and numbered
  lists, block quotes, tables, fenced code blocks, rules, and inline
  `**bold**`/`*italic*`/`` `code` ``. Code fences stay literal and code lines
  have no gutter, so a selection copies valid markdown. Tables are a grid
  with box-drawing borders and the column alignment of the separator row.
  When a table is wider than the screen, text wraps inside its cells. A code
  line wider than the screen continues on rows that start with `↪`. Math
  (`$…$`, `$$…$$`, `\(…\)`, `\[…\]`) shows as one line of Unicode:
  `$\alpha_1 \le x^2$` becomes `α₁ ≤ x²`. `/copy` puts the last answer on the
  clipboard as raw markdown.
- Picks its **palette from the terminal**: it asks the terminal for its
  background color (OSC 11) and uses a dark or a light 24-bit palette. On
  terminals without truecolor it uses the nearest 256 colors. Grays are
  explicit colors, never dim or italic attributes.
- Offers **slash-command autocomplete**: type `/` to open a filtered menu below the
  input (`/p` → `/provider`, typed prefix highlighted, usage + description per row);
  Up/Down select, Tab completes, Esc closes, Enter runs. Typing `/model ` extends
  the menu with the saved provider/model pairs themselves.
  Commands: `/model [N | MODEL | PROVIDER MODEL]`, `/provider [add]`, `/paste`,
  `/copy`, `/history QUERY`, `/clear`, `/new`, `/cost`, `/thinking`,
  `/retain-thinking`, `/help`, `/exit`.
- **Interactive model picker**: `/model` alone opens a selector under the
  input — `› deepseek-v4-pro [deepseek] ✓` rows (cursor marker, muted provider,
  check on the active pair) with a `(K/N)` counter; Up/Down move, typing
  filters, Enter switches, Esc closes. `/model 2`, a bare model or provider
  name, or the full pair still switch directly. A model name the config has
  never seen switches to it AND saves it to the provider (`model X saved to
  provider Y`), so typing a brand-new model is enough to add it. `/provider`
  lists all saved providers with their models. `/provider add` opens the same
  provider wizard as the setup screen; after the save the chat switches to the
  new provider. While the agent works, `/provider add` waits in the queue. (The line-mode REPL prints the
  same list as numbered text.)
- **Resumes the last chat on launch**: the conversation is persisted per
  workspace and reloaded at startup (a `resumed session … (N messages)` notice
  appears in the transcript). `/new` abandons it and starts a fresh session.
- **Welcome banner**: the conversation flows from the top, blank rows fill
  the space down to the editor. The startup banner (`harness vX`, active
  model, workspace, key hints) is the first transcript entry.
- **Type ahead while the agent works**: keys go to the editor during a run.
  Enter queues the prompt (`⏵ queued …` above the editor), and the queue is
  sent in order when the run ends. Local commands (`/help`, `/cost`,
  `/thinking`, …) run at once. An interrupted run puts the queued prompts
  back into the editor.
- **Esc or Ctrl+C interrupts a running agent** (an `Interrupted by user` notice
  appears; the partial trace is still saved). Esc first closes an open menu. Streaming stops between tokens,
  tool loops stop before the next round; the session stays open — when idle,
  **exit is two-step**: the first Esc shows `Press Esc again to exit` in the
  status row (any other key disarms it), the second Esc exits; Ctrl+C still
  exits immediately. The setup TUI follows the same contract, and Esc inside
  the provider wizard closes the dialog instead of killing the program.
- Has a **multi-line prompt**. Alt+Enter, Ctrl+J and Ctrl+Enter insert a
  newline (Windows Terminal sends Ctrl+Enter as Ctrl+J). Shift+Enter also
  works on terminals with the kitty keyboard protocol (Windows Terminal
  1.25+, WezTerm, kitty, Ghostty). Editing keys: Delete, Ctrl+W and
  Ctrl+Backspace (delete a word), Ctrl+U/Ctrl+K (delete to line start/end),
  Ctrl+A/Ctrl+E and Home/End (line start/end), Ctrl+Left/Right (by word).
  Up/Down move between the prompt's lines before they recall history.
  **Paste never auto-submits** — pasted newlines
  stay literal even on the legacy Windows console (a burst of key events is
  coalesced into one paste). A paste of 3+ lines (or 700+ chars) **collapses to a
  `[Pasted text #1 +184 lines]` placeholder** in the editor; the full text is
  substituted back in when the message is sent, while the transcript and history
  keep the compact marker.
- Captures **Ctrl+V or `/paste`** from the system clipboard: text is inserted at
  the caret, a PNG image is saved to `.harness/attachments` and referenced in the
  prompt. `/paste` exists because many terminals bind Ctrl+V themselves —
  Windows Terminal in particular swallows it entirely when the clipboard holds
  an image (its own paste action finds no text and the key never reaches the
  app), so use `/paste` there.
- Recalls prompt history with Up/Down.
- **Survives window resizing**: the alternate screen has no scrollback for
  the terminal to reflow, so a resize repaints every row at the new size.
  The old inline screen let Windows Terminal push the painted panel into
  its scrollback on resize (`ESC [2J` there scrolls the viewport up), which
  left a ghost input box in the middle of the history.

```powershell
harness repl --config .harness/providers.json --workspace . --provider custom --model deepseek-v4-pro --timeout-ms 60000 --max-rounds 4 --max-tool-concurrency 4 --tool-timeout-ms 10000
```

### Sessions and traces on disk

Every chat and agent run is persisted automatically under a global per-user
store (override the base directory with the `HARNESS_HOME` environment
variable; default `~/.harness`):

```
~/.harness/projects/<workspace-slug>/
├── sessions/
│   ├── 2026-07-11T09-15-42Z_a1b2c3.jsonl   # one session = one append-only JSONL
│   └── last                                 # pointer to the session resumed on launch
├── traces/
│   └── 2026-07-11T09-15-42Z_deepseek_r3.json  # one agent run = one raw trace
└── raw/
    └── 2026-07-11T09-15-42Z_a1b2c3.jsonl   # raw wire log, same file name as the session
```

- **Session JSONL**: first line is a `meta` header (session id, workspace,
  starting provider/model, `parent_session` reserved for future compaction);
  each following line is a `message` (exactly what the provider sees — filter
  `type == "message"` to rebuild the conversation) or a `thinking` record
  (model reasoning, kept for reading/analysis, never replayed to the provider).
  Corrupt lines and dangling tool calls from a crash are skipped/trimmed on
  resume.
- **Trace files**: the raw agent trace (`ModelToolCalls` with the model's
  original tool names/arguments, `ToolResult` with `ok`/`error`/`repaired` and
  the repair-memo `hint`, thinking, final content) wrapped with `ts`,
  `session_id`, and `turn` — one file per run, success or failure, including
  one-shot `agent run`. Feed these to an LLM (or scripts) to compare models and
  spot bad tool calls offline.
- **Raw wire log**: every provider request body, blocking response body, and
  SSE `data:` line (including `[DONE]`) is appended pre-parse as
  `{ts, kind, url, body}` JSONL — insurance against parser bugs: whatever
  actually crossed the wire survives even when response parsing drops a field.
  Bodies only, headers (and auth keys) are never written. On automatically in
  the REPL/TUI whenever session persistence is active.
- Persistence failures never kill the chat: they surface as warnings and the
  conversation continues in memory.

Check lightweight process/binary diagnostics:

```powershell
harness diagnostics --binary target/release/harness.exe --max-binary-bytes 5000000
```

Execute a repaired tool call directly:

```powershell
harness tool call --workspace . write_file '{"file":"notes.txt","text":"hello"}'
```

Append text without shelling out:

```powershell
harness tool call --workspace . append_file '{"file":"notes.txt","text":"\nnext line"}'
```

Execute a bounded-concurrency batch of tool calls:

```powershell
harness tool batch --workspace . --max-concurrency 4 --timeout-ms 10000 '[{"id":"one","name":"write_file","arguments":{"file":"one.txt","text":"1"}}]'
```

Search the public web (keyless DuckDuckGo scraper; result titles, URLs, snippets):

```powershell
harness tool call --workspace . web_search '{"query":"rust ureq timeout","count":5}'
```

Fetch a page as readable text (HTML stripped, entities decoded, output bounded):

```powershell
harness tool call --workspace . web_fetch '{"url":"https://blog.rust-lang.org/","max_chars":4000}'
```

In agent runs both tools are advertised automatically (`web_search`/`web_fetch`,
probe-aligned names) and follow the provider's proxy policy; `harness tool call`
goes direct.

Capture the current system clipboard as a prompt-ready attachment:

```powershell
harness clipboard paste --workspace .
```

Capture already-pasted terminal text through the same attachment path:

```powershell
harness clipboard paste --workspace . --text "pasted text"
```

## Roadmap

Done (each item is verified by tests and/or the agent benchmark):

- ✅ Cache-first request envelope — byte-stable prefix, BLAKE3 cache keys, live `/cost`
- ✅ Four chat API formats (OpenAI-compatible, OpenAI Responses, Codex Responses, Anthropic Messages) + SSE streaming
- ✅ Provider onboarding — presets, `/models` discovery, Bearer/header/env auth, per-provider proxy, cache metadata
- ✅ Agent loop with forgiving tool runtime — name aliases, argument coercion, repair memos, loop guardrails
- ✅ Model-fitted tools — probe suite + 26-task benchmark across 6 model families (1199 tool calls, 0 failures)
- ✅ Fallback chains with error classification (switch on 5xx/429/quota, never on auth/context/policy)
- ✅ Own TUI library (`harness-tui`) — no ratatui/crossterm, native-scrollback screen, own input parser
- ✅ Chat TUI — markdown rendering, slash-command autocomplete, interactive model picker, streaming tool cards
- ✅ Session & trace persistence under `~/.harness` with resume on launch
- ✅ Clipboard & attachments — Ctrl+V / `/paste`, PNG capture into `.harness/attachments`
- ✅ Window-resize survival — full-screen repaint, no ghost panels
- ✅ TUI redesign — docked layout without history gaps, light/dark truecolor palette, collapsible reasoning, tool cards with output tails and diffs, type-ahead queue, editing keys, Windows Terminal profile
- ✅ Full-screen chat — pinned editor, own scrolling, drag-to-copy (OSC 52), in-place reasoning expand, paced text reveal with fade-in
- ✅ Agent self-awareness — environment line + harness usage notes in the system prompt

Planned:

- [ ] Subagents — spawn scoped child agent runs with their own tool budgets
- [ ] Provider presets/configs for more model families
- [ ] Vision — image blocks in provider requests so models actually see PNG attachments

## Implemented core

- Minimal system prompt under the 500-1000 token target.
- Cache-aware request envelope with separate prefix and full request keys.
- OpenAI-compatible chat request adapter with tool-call parsing.
- OpenAI-compatible streaming chat adapter for data-only SSE text deltas.
- OpenAI-compatible tool argument parsing repairs common malformed JSON and preserves
  unrepaired raw arguments as `_raw_arguments` instead of failing the whole response.
- Built-in provider family slots for `codex`, `xiaomi`, `glm`, `kimi`, and `claude`.
- Built-in subscription metadata for `codex`, `xiaomi`, `glm`, `kimi`, `claude`, and `deepseek`.
- Provider auth policy supports Bearer, custom header, env-backed subscription
  keys, and explicit API-key override.
- `provider list` prints saved providers, models, auth/cache/chat metadata, and
  key source without exposing inline API keys.
- Provider onboarding accepts `--auth bearer`, `--auth header --auth-header NAME`,
  or `--auth subscription` for custom providers and model discovery.
- Network-backed provider, chat, agent, and REPL commands accept
  `--timeout-ms N` so long-running API calls can be bounded from the CLI.
- Provider cache policy supports configurable cache-key headers, body-level
  `cache_control` markers, and automatic cache-metric parsing.
- Chat responses include a compact cache report with hit/miss tokens, hit ratio,
  and saved prompt-token estimate when provider usage exposes cache metrics.
- Provider chat routing supports OpenAI-compatible chat, OpenAI Responses API,
  OpenAI Codex Responses API, and Anthropic Messages API format.
- `provider add` accepts `--chat-api openai-compatible`, `openai-responses`,
  `openai-codex-responses`, or `anthropic-messages` to save custom request routing metadata.
- Codex profile uses the dedicated `/codex/responses` route instead of generic
  Chat Completions.
- Claude profile uses Anthropic `x-api-key` auth and native `/messages` requests.
- Claude profile enables Anthropic automatic prompt caching with top-level
  `cache_control: {"type":"ephemeral"}` in `/messages` request bodies.
- DeepSeek profile treats context caching as automatic and parses `prompt_cache_hit_tokens`
  / `prompt_cache_miss_tokens` from response usage.
- OpenAI-compatible usage parser also reads nested `prompt_tokens_details.cached_tokens`.
- OpenAI-compatible `/models` discovery with an explicit `Add all` choice.
- Interactive provider onboarding can prompt for name, URL, and key, then save
  `0) Add all` or a selected model number.
- Provider onboarding accepts `--key-env ENV` so custom providers can resolve
  subscription/API keys from the environment without storing secrets.
- Provider onboarding accepts cache metadata flags such as `--cache automatic`,
  `--cache-hit-field`, `--cache-miss-field`, `--cache header`,
  `--cache body-cache-control`, and `--cache anthropic-automatic`.
- JSON config store for custom providers, URLs, keys, and selected models.
- Native shell profile selection for Windows and Linux.
- Diagnostics command reports current process RSS, binary size, and optional limit checks.
- Forgiving file-write tool that does not require a prior read and still records previous content.
- Tool runtime for `file.read`, `file.tail`, `file.hash`, `file.stat`,
  `file.write`, `file.append`, `file.replace`, `file.delete`, `file.move`,
  `attachment.read`, and `shell.exec`, including common LLM alias repair.
- Tool runtime can coerce common raw string or `_raw_arguments` mistakes into
  structured arguments for file and shell tools.
- Tool runtime coerces string numeric/boolean values such as `"lines": "2"`
  or `"overwrite": "true"` for common LLM argument-shape mistakes.
- Tool runtime drops `null`-valued arguments the model should have omitted and
  runs the call anyway instead of failing.
- When a tool call is tolerated after a repair, the tool result carries a `hint`
  memo telling the model the canonical wire tool name and argument shape to use
  next time, so it self-corrects instead of repeating the mistake. The memo only
  fires on genuine deviations (a real alias or wrong argument), never on the
  API-safe wire name the harness itself advertises (`file.write` is sent as
  `file_write`), and it references the callable wire name, never a dotted name
  the API would reject.
- Single-file tools accept Codex/OpenAI-style `file_path` in addition to the
  generic path aliases.
- `file.write` also accepts Codex/OpenAI-style `contents`.
- `file.append` supports `append_file`, Codex/OpenAI-style `file_path`, and
  `contents` while preserving existing file content.
- `file.replace` accepts Codex/OpenAI-style `file_path`, `old_string`, and
  `new_string` arguments in addition to the generic aliases.
- `file.move` accepts Codex/OpenAI-style `source_path`, `target_path`, and
  `destination_path` arguments in addition to the generic aliases.
- Bounded workspace file tools for truncated `file.read`, recursive `file.list`,
  bounded suffix `file.tail`, streaming `file.hash`, metadata-only `file.stat`,
  literal `file.search`, literal `file.replace`, safe delete, and non-shell
  move/rename.
- `file.list` stops traversal after `max_results + 1` visited entries, enough to
  report truncation without collecting the whole workspace tree.
- `file.search` and `file.list` follow ripgrep's rules: hidden entries and
  paths in the workspace root `.gitignore` are not walked (`file.list` still
  shows an ignored directory, it does not enter it). An explicit `path`
  inside such a directory is searched as asked. A search without matches
  names the skipped paths, so "no matches" does not read as "not in the
  project". Entries that vanish or cannot be read mid-walk (a build running
  next to the search) are passed over instead of failing the call.
- `shell.exec` on Windows PowerShell reports a POSIX-like exit code: the
  last native program's code, else 0 unless a real error happened.
  `NativeCommandError` records from `2>&1` (cargo prints progress to stderr)
  no longer fail the call, and delayed table output (`Select-Object
  @{N=...}`) is flushed before the process exits.
- Attachment read tool repairs `image file: ...` references and only reads from
  workspace `.harness/attachments` or user `.codex/attachments` roots.
- Native shell command timeout, per-call `timeout_ms`, and bounded stdout/stderr
  capture so tool execution does not hang on full pipes or retain unbounded
  command output.
- Timed-out shell commands kill the child process and retain captured output
  metadata for diagnostics. Pipe readers are drained with a short grace period,
  never joined to EOF: a background process the command left behind (a server
  started via `Start-Process`/`&`) inherits the pipe and would otherwise freeze
  the agent until it exits.
- Esc/Ctrl+C during an agent turn cancels a running `shell.exec`: the shared
  cancel flag reaches the tool runtime, the shell child is killed, and the
  partial output comes back in the cancelled result.
- SSE idle timeout has a **300s floor**: providers that buffer whole tool
  calls (baseten/Kimi) go legitimately silent for minutes while the model
  writes a large file; connect/header phases keep the snappy request timeout.
  While the stream is silent ≥15s the chat shows a dimmed
  `stream silent Ns — model may still be generating; Esc interrupts` note.
- Truncated streams surface as errors instead of shorter answers: an in-band
  SSE `{"error":...}` event maps onto the HTTP-error taxonomy for the failover
  classifier, and EOF without `[DONE]`/`finish_reason` is `UnexpectedEof`.
  Transport errors get up to two quick in-place retries before the fallback
  chain is consulted — unless deltas already reached the user, in which case
  the turn fails fast (no silent replay that would duplicate visible text).
- **A failed or interrupted turn keeps its conversation**: the error carries
  the messages up to the cut (user message + a partial assistant turn with
  any streamed text and thinking), the session history adopts them, and the
  partial thinking is echoed back to the API on the next request. A follow-up
  "продолжай" resumes the turn instead of starting from a blank context.
- Reasoning retention is **on by default** in the chat TUI and line REPL
  (`/retain-thinking` toggles it): DeepSeek V4 / Kimi / MiMo thinking modes
  require the echo-back on every assistant turn.
- Bounded tool scheduler for concurrent batch execution with order-preserving JSON results
  and optional batch deadlines that mark unfinished calls as cancelled results.
- Multi-turn agent runner that appends assistant tool-call messages, executes tools,
  appends tool results, reports tool errors back to the model, and stops at a final
  assistant answer.
- `agent run` can write a full trace with model tool calls, tool results, and
  final content via `--trace PATH`, plus failed tool results via
  `--tool-errors PATH`.
- `agent run` and `repl` accept `--max-rounds N` to bound repeated tool-call
  loops. `agent run` defaults to 4 rounds; the interactive `repl`/chat is
  unbounded by default (an exploring agent legitimately needs many rounds) —
  pass `--max-rounds N` to cap it.
- The agent's system prompt includes a one-line **environment description**
  (OS, shell dialect, workspace root, and the fact that `cd` does not persist
  between shell calls), so the model does not guess its surroundings and invent
  Linux paths on Windows. The line is constant per session, keeping the prompt
  cache prefix stable.
- The shell tool **repairs Unix-isms before running a command** on Windows
  PowerShell 5.1: `&&` chains are rewritten as `; if ($?) { … }` (keeping
  stop-on-failure semantics), and a leading `cd <path>` into a directory that
  does not exist (e.g. a hallucinated `/mnt/<project>`) is dropped because
  commands already run in the workspace. Each rewrite sets `repaired: true`,
  records `repair_note`/`original_command` in the result metadata, and hands
  the model a corrective memo. On failure the command's stderr is included in
  the model-visible content, so the model can see *why* the call failed.
- `agent run` and `repl` accept `--max-tool-concurrency N` to cap parallel
  tool execution when a model returns many calls at once.
- `agent run` and `repl` accept `--tool-timeout-ms N` to cap a whole tool-call
  batch in each agent round.
- Clipboard capture backend for text and PNG image attachments with prompt fragments.
- Native system clipboard reader for Windows and Linux command-line environments.
- Interactive terminal REPL with Ctrl+V text/image paste handling, Enter submit,
  Ctrl+C exit, in-session history search, model switching, and streaming agent
  progress/output events.
- Bracketed paste support in the REPL and setup TUI so multi-line text and
  clipboard pastes arrive as one atomic event instead of submitting on the first
  newline; pastes inside the provider wizard route into the active field (e.g. an
  API key) with trailing clipboard newlines stripped.
- Ratatui chat TUI for the live session on a real terminal: a scrolling
  transcript that folds in streamed tool rounds, tool results, and assistant
  output, plus a prompt editor and bracketed paste. Non-TTY runs (pipes, tests)
  keep the line-mode REPL.
- Chat TUI in-session commands and navigation: `/model PROVIDER MODEL` (live
  provider/model switch), `/history QUERY`, `/clear`, `/help` (command palette
  overlay), `/exit`; Up/Down recall previous prompts and PageUp/PageDown scroll
  the transcript (new output snaps back to the bottom).
- No-provider setup TUI (on `harness-tui`) for the real `harness` terminal
  launch path, with line-mode fallback for non-TTY output.

## Workspace

The repo is a cargo workspace. `crates/harness-tui/` is our own terminal UI
library (line-based rendering, native-scrollback screen model) powering all
interactive front ends — the previous third-party TUI stack is fully removed.
Run its tests with `cargo test -p harness-tui`; run everything with
`cargo test --workspace`.

## Verification

```powershell
cargo test --workspace
```

Linux verification was run under WSL2 Ubuntu 24.04 with a separate target dir:

```bash
CARGO_TARGET_DIR=/tmp/rust-harness-target-linux cargo test
CARGO_TARGET_DIR=/tmp/rust-harness-target-linux cargo clippy --all-targets -- -D warnings
CARGO_TARGET_DIR=/tmp/rust-harness-target-linux cargo build --release
```
