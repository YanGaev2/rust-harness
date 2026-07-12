# Terminal chat screen-flow architecture report

Date: 2026-07-12  
Repository: `F:\rust-harness` (`harness-cli`)  
Scope: analysis and implementation design only; no source-code change is made by this report.

## Executive conclusion

The two complaints have the same architectural root: one conversation is being rendered in two coordinate systems.

- Finished entries are written at the native-content cursor near the top of the viewport.
- The mutable tail and editor are rendered in a separately bottom-anchored panel.
- A user entry changes ownership from the second system to the first as soon as a later streaming entry appears.

That ownership transfer is why the user message first appears near the bottom, then moves to the top, while the answer remains near the bottom. Bottom anchoring then turns the distance between those two coordinate systems into the giant empty gap.

The reference implementations fall into two distinct families:

1. **Pi and Qwen's default mode:** main terminal buffer, one vertically ordered flow, content followed directly by the mutable response and composer. This is the right behavioral model for harness because native scrollback survives exit.
2. **OpenCode and Qwen's optional terminal-buffer mode:** alternate-screen/full-window viewport with an independently pinned composer and an in-app scrollbox. This produces a clean top-origin full-screen launch, but the conversation does not remain in native terminal scrollback after exit.

The recommended harness model is therefore:

- stay on the main screen; never enter `CSI ? 1049 h`;
- claim the visible viewport instantly with `CSI 2 J` + `CSI H`, without newline scroll-push and without `CSI 3 J` at startup;
- render a single contiguous vertical flow: committed history, mutable response/tool tail, spinner, editor, menus, status;
- anchor the mutable region at the row immediately following committed content, never at `height - panel_height`;
- promote finalized rows and redraw the next mutable frame in **one synchronized terminal write**;
- let new content scroll the terminal naturally only when it reaches the bottom.

The current `master` snapshot needs one qualification. The complaint-causing implementation is commit `b4357ed`. During this analysis, `master` advanced to `0df7963` (`fix: reference screen model - clear at startup, panel follows content`). That commit is a useful partial correction: it replaces newline takeover with a viewport clear and changes the normal panel origin to follow content. It is not complete: resize explicitly pins the panel back to the bottom, commit and live repaint still happen as two frames, finalization reserves the old panel height, and long live output is still discarded from the visible head. Those remaining issues are covered below.

## Terminology

- **Main/primary screen:** the normal terminal buffer. Output becomes native scrollback and remains after the process exits.
- **Alternate screen:** the terminal's disposable application buffer, conventionally entered with `ESC [ ? 1049 h` and left with `ESC [ ? 1049 l`.
- **Committed content:** immutable chat rows that may be released into native scrollback.
- **Live region:** the mutable assistant/tool rows plus spinner, editor, menus, and status.
- **Flow origin:** the physical row at which the live region starts. It must equal the row immediately after committed content.

## Reference comparison

| Reference | Startup / first frame | Composer placement | Submit and stream | Native transcript after exit | Resize / tall content |
|---|---|---|---|---|---|
| Pi coding agent | Main screen; no alternate-screen entry and no startup clear. First render begins at the existing cursor row. | Directly after chat/status/widgets in one ordered root container. | User and assistant components are appended to the same container; assistant component is updated in place. | Yes. The full rendered tree advances the main buffer and exits below it. | Full clear/replay (`2J`, home, `3J`) on most width/height changes; appended rows scroll with CRLF. |
| OpenCode TUI | OpenTUI default alternate screen; first frame owns the full viewport from the top. | Separate non-shrinking composer below a growing scrollbox; structurally viewport-bottom pinned. | User and assistant remain in one message list; streamed parts update in place and the scrollbox follows the bottom. | No full transcript. On exit only a small epilogue is printed to the restored primary screen. | OpenTUI recomputes the full viewport; the scrollbox owns overflow and scroll position. |
| Qwen Code default | Main screen because `useTerminalBuffer=false`; no startup clear. First frame begins at the current cursor row. | `MainContent` followed immediately by `Composer`; no flexible spacer. | User is appended to `<Static>`, assistant stays pending below it, then is committed to `<Static>` without changing order. | Yes. `<Static>` writes completed history above the dynamic region. | Width resize is debounced 200 ms, then `clearTerminal` (`2J`, `3J`, home) and full Static replay; tall output is progressively frozen. |
| Qwen Code terminal-buffer mode | Alternate screen via Ink; clean full-window viewport. | In-app viewport plus composer. | Completed and pending items share one virtual list. | No; setting documentation explicitly says host scrollback is not used. | Virtualized in-app list owns resize and overflow. |

The references do **not** unanimously clear to the top on main-screen startup. Pi and default Qwen start at the shell cursor. OpenCode starts at the top because it uses the alternate screen. Harness's requested `2J` + home startup is therefore a deliberate product-specific hybrid: Pi's available clear primitive plus Qwen's main-screen Static/live flow. It should not be described as Pi's actual startup behavior.

Library defaults were checked through Context7 against the official [OpenTUI renderer documentation](https://github.com/anomalyco/opentui/blob/main/packages/web/src/content/docs/core-concepts/renderer.mdx) and [Ink documentation](https://github.com/vadimdemedes/ink), then cross-checked against the exact versions pinned by the reference lockfiles. Repository citations below identify the application decisions; dependency behavior is called out separately where it supplies an implicit default.

## Reference evidence

### Pi: one main-buffer render tree

#### Startup and first frame

`ProcessTerminal.start()` enables raw input, bracketed paste, resize handling, and keyboard-protocol negotiation, but it does not enter an alternate screen and does not clear or home the terminal (`external/tui-reference/pi/packages/tui/src/terminal.ts:134-167`). `TUI.start()` starts the terminal, hides the cursor, queries cell size, and requests a render; it also performs no clear or alternate-screen switch (`external/tui-reference/pi/packages/tui/src/tui.ts:635-647`).

The renderer is explicit about first paint: `doRender()` calls `fullRender(false)` and comments that the first render outputs everything “without clearing” (`external/tui-reference/pi/packages/tui/src/tui.ts:1335-1339`). Because that write contains neither home nor absolute top positioning, its first row is the terminal's current cursor row.

Pi does expose `ProcessTerminal.clearScreen()` as exactly `"\x1b[2J\x1b[H"` (`external/tui-reference/pi/packages/tui/src/terminal.ts:500-502`), but the coding-agent startup path does not call it. A repository-wide search found production startup calls to `TUI.start()`, not to this clear primitive.

#### Layout

Pi's `Container.render()` concatenates each child component's rows in child order (`external/tui-reference/pi/packages/tui/src/tui.ts:256-289`). The coding app adds these root children in this order:

1. header and loaded resources;
2. chat and pending messages;
3. status/widgets;
4. editor;
5. lower widgets and footer.

The exact construction is in `InteractiveMode.init()` at `external/tui-reference/pi/packages/coding-agent/src/modes/interactive/interactive-mode.ts:641-654`; the TUI starts at lines 659-660. There is no viewport-height spacer between chat and editor. If the conversation is short, the editor is immediately below it. If the flow reaches the bottom, the entire flow advances.

#### Submit, stream, and finalize

The main loop obtains editor input and calls `session.prompt(userInput)` (`interactive-mode.ts:829-837`). Session events then maintain stable component positions:

- user `message_start` appends the user component to `chatContainer` (`interactive-mode.ts:2782-2789`);
- assistant `message_start` appends one streaming assistant component immediately afterward (`interactive-mode.ts:2790-2800`);
- `message_update` mutates that same component (`interactive-mode.ts:2804-2836`);
- `message_end` performs the final update and merely clears the tracking references; it does not remove and reinsert the component (`interactive-mode.ts:2839-2875`).

`addMessageToChat()` shows the same stable ordering concretely: the user component is appended at lines 3144-3175 and the assistant component at lines 3178-3186 (`interactive-mode.ts:3105-3192`). Thus no entry changes from a bottom-panel coordinate system into a top-content coordinate system.

#### Native scrollback, tall output, and resize

Pi retains the entire conversation in the root render tree. When a changed/appended row lies below the prior viewport, `doRender()` moves to the viewport bottom and emits enough CRLFs to scroll it into view (`external/tui-reference/pi/packages/tui/src/tui.ts:1461-1477`), then writes the changed rows (`tui.ts:1480-1495`). On stop it moves below the last rendered content and writes a final CRLF before restoring the cursor (`tui.ts:687-709`). The transcript therefore remains in the main terminal buffer.

Pi handles changed wrapping by destructive replay. `fullRender(true)` sends synchronized output followed by `\x1b[2J\x1b[H\x1b[3J` and re-emits every row (`tui.ts:1283-1325`). Width changes always take that path (`tui.ts:1342-1347`); height changes normally do too, except for a Termux special case (`tui.ts:1349-1355`). This is deterministic, but it destroys pre-session scrollback.

### OpenCode: alternate-screen viewport

#### Startup and first frame

OpenCode creates `@opentui/core`'s renderer without passing `screenMode` (`external/tui-reference/opencode/packages/tui/src/app.tsx:180-205`). The lockfile pins `@opentui/core` and `@opentui/solid` to 0.3.4 (`external/tui-reference/opencode/bun.lock:1079-1081`, package resolution at line 2012).

The exact pinned OpenTUI 0.3.4 implementation resolves an omitted `screenMode` to `"alternate-screen"`; its terminal switch is `ESC[?1049h`. This also agrees with the current official OpenTUI renderer documentation, which documents alternate screen as the default. Therefore OpenCode's first frame is a full alternate-screen frame at the top of the viewport, not a continuation below the shell cursor.

This conclusion is based on both the local call site/version and the exact dependency API; it is not inferred merely from the visual layout.

#### Layout

The session view is a full-height flex layout. Its message `<scrollbox>` has `flexGrow={1}`, `stickyScroll={true}`, and `stickyStart="bottom"` (`external/tui-reference/opencode/packages/tui/src/routes/session/index.tsx:1165-1185`). All user and assistant messages render inside it (`session/index.tsx:1186-1280`). The prompt is in a separate `flexShrink={0}` box after the scrollbox (`session/index.tsx:1281-1320`).

Consequently, OpenCode's input is viewport-bottom pinned, not structurally adjacent to the last content row. `stickyStart="bottom"` keeps the scroll offset following the bottom when content overflows; it does not turn the prompt into part of the message flow. This is a valid fullscreen-app design, but not the requested harness design for short conversations.

#### Submit and streaming

`Prompt.submit()` prevents concurrent submissions (`external/tui-reference/opencode/packages/tui/src/component/prompt/index.tsx:924-941`), sends the request (`prompt/index.tsx:1087-1113`), clears prompt state and the input, and calls `onSubmit` (`prompt/index.tsx:1116-1139`). The session's `onSubmit` calls `toBottom()` (`session/index.tsx:1308-1317`), whose implementation scrolls to `scrollHeight` after layout (`session/index.tsx:416-420`).

Incoming messages are inserted or reconciled in one ordered message store (`external/tui-reference/opencode/packages/tui/src/context/sync.tsx:304-342`). Incoming parts and text deltas update their existing position (`sync.tsx:359-395`). Assistant markdown is rendered with `streaming={true}` (`session/index.tsx:1679-1694`). The composer is separate, but both halves of the turn remain inside the same scrollbox, so the user entry never jumps into a different coordinate system.

#### Exit, scrollback, and resize

Destroying the renderer restores the primary screen (`external/tui-reference/opencode/packages/tui/src/util/renderer.ts:3-7`). After renderer teardown, `run()` prints only an error and/or the configured epilogue (`app.tsx:343-352`). `sessionEpilogue()` contains a logo, session title, and continue command—not the transcript (`external/tui-reference/opencode/packages/tui/src/util/presentation.ts:29-37`; epilogue setup at `session/index.tsx:202-206`). Thus this model fails the harness requirement that completed conversation history survive exit in native scrollback.

Resize is owned reactively by OpenTUI (`useTerminalDimensions()` in `app.tsx:355-360` and `session/index.tsx:248`). The full-screen tree is recomputed, while the scrollbox retains/updates its in-app scroll offset. There is no transfer of completed rows into host scrollback.

### Qwen Code: Static committed history plus a dynamic tail

Qwen's default path is the closest match to the desired harness behavior.

#### Startup modes

`startInteractiveUI()` reads `ui.useTerminalBuffer`, defaulting to `false`, and passes it directly as Ink's `alternateScreen` option (`external/tui-reference/qwen-code/packages/cli/src/ui/startInteractiveUI.tsx:174-187`). The schema also defaults the setting to false and explicitly states that enabling it uses an in-app viewport rather than host terminal scrollback (`external/tui-reference/qwen-code/packages/cli/src/config/settingsSchema.ts:1001-1009`).

Therefore:

- default Qwen does not enter the alternate screen and starts rendering at the current main-screen cursor row;
- optional terminal-buffer mode enters Ink's alternate screen (`ESC[?1049h`) and restores it with `ESC[?1049l` on exit.

The lockfile pins Ink 7.0.3 and `ansi-escapes` 7.3.0 (`external/tui-reference/qwen-code/package-lock.json:6793-6800`, `12838-12868`). The exact pinned implementations were checked: Ink writes the alternate-screen escape when `alternateScreen` is true, and `ansi-escapes.clearTerminal` is `2J`, `3J`, then home on non-legacy Windows.

#### Layout

`DefaultAppLayout` is a column containing `<MainContent />`, followed immediately by the controls box and `<Composer />` (`external/tui-reference/qwen-code/packages/cli/src/ui/layouts/DefaultAppLayout.tsx:62-110`). There is no `flexGrow` spacer between them. This is the content-following layout the user is asking for.

In default mode `MainContent` renders:

- completed history through Ink `<Static>` (`external/tui-reference/qwen-code/packages/cli/src/ui/components/MainContent.tsx:408-440`);
- mutable `pendingHistoryItems` directly below it (`MainContent.tsx:441-463`).

In terminal-buffer mode it instead combines completed and pending items into a `ScrollableList`, initially scrolled to the end (`MainContent.tsx:378-405`; combined list at lines 243-250).

#### Submit, stream, and finalize

The input buffer is cleared before the submit callback (`external/tui-reference/qwen-code/packages/cli/src/ui/components/InputPrompt.tsx:573-576`; Enter dispatch at lines 1548-1578). Query preparation immediately appends a final user history item (`external/tui-reference/qwen-code/packages/cli/src/ui/hooks/useGeminiStream.ts:1029-1051`). The response starts and grows as `pendingHistoryItem` below that user item (`useGeminiStream.ts:1108-1194`). On completion Qwen commits the pending item and clears pending state (`useGeminiStream.ts:2335-2338`).

The implementation explicitly prevents a one-frame disappearance when a pending item becomes Static: normal appends include the finalized item immediately rather than waiting for progressive replay (`external/tui-reference/qwen-code/packages/cli/src/ui/components/MainContent.tsx:230-241`). This is the exact invariant harness currently lacks: the visual position is continuous while ownership changes.

Large streaming responses are split: safe prefixes are committed while the final suffix remains pending (`useGeminiStream.ts:1144-1191`). That avoids keeping an unbounded response in the mutable area. The final pending list is assembled at `useGeminiStream.ts:3032-3048`.

#### Native scrollback and resize

Ink `<Static>` permanently appends completed items above dynamic output, so default Qwen history lives in native scrollback. Qwen's history manager also treats direct updates to Static entries as exceptional (`external/tui-reference/qwen-code/packages/cli/src/ui/hooks/useHistoryManager.ts:55-95`).

Width changes require rewrap. Qwen debounces them for 200 ms, then calls a full `refreshStatic`; comments document that this emits `clearTerminal`, including `ESC[3J`, and replays history once at the settled width (`external/tui-reference/qwen-code/packages/cli/src/ui/hooks/useResizeSettleRepaint.ts:9-49`). `refreshStatic()` performs the clear only in main-screen mode and remounts Static (`external/tui-reference/qwen-code/packages/cli/src/ui/AppContainer.tsx:948-964`); the hook is installed at `AppContainer.tsx:2715-2716`.

This preserves one correctly wrapped copy of the chat after replay, but erases pre-session scrollback. Qwen documents that trade-off directly at `useResizeSettleRepaint.ts:25-27`.

## Diagnosis of the complaint-causing harness model (`b4357ed`)

### Complaint 1: startup “skips down” and leaves the editor floating

The old startup path does two independent things that together produce the complaint.

1. `run_chat_tui()` calls `screen.takeover()` immediately after attaching (`b4357ed:src/repl.rs:408-412`).
2. `Screen::takeover()` moves to the bottom row and emits exactly one CRLF per terminal row, then homes (`b4357ed:crates/harness-tui/src/core.rs:170-186`). This is a scroll-push, not a clear. It visibly advances the main buffer by an entire window and creates a window-sized run of blank terminal rows.
3. `Screen::panel_row()` always computes `height - panel_height` (`b4357ed:crates/harness-tui/src/core.rs:40-45`), and `render_panel()` always targets that row (`core.rs:108-145`).
4. Startup intentionally has no transcript banner (`src/repl.rs:415-416`), so the first frame contains only editor/status rows at the physical bottom. Everything above them is blank.

The result is not merely cosmetic. The scroll-push says “content begins at row 0,” while the bottom panel says “the only current content begins near row `height`.” The empty space is therefore a designed invariant of that model.

The old tests codify it: `panel_pins_to_the_bottom_even_on_an_empty_screen` expects the input/status on rows 8 and 9 of a ten-row screen (`b4357ed:crates/harness-tui/tests/core.rs:38-48`), and `takeover_scrolls_shell_content_away_and_starts_at_the_top` expects one CRLF per row (`core.rs` test lines 158-176 in that commit).

### Complaint 2: user message jumps away from its response

The exact frame sequence is:

1. `ChatApp::submit_input()` clears the editor, appends `ChatEntry::User`, and returns `ChatAction::Submit` (`src/chat.rs:436-450`).
2. `run_chat_tui()` sets `busy=true` and draws immediately (`src/repl.rs:490-492`).
3. While busy, `take_scrollback()` deliberately excludes the last transcript entry (`src/chat.rs:730-744`). At this moment the user entry is the last/only entry, so it remains live.
4. `panel_lines()` renders all un-emitted entries before spinner/editor/status (`src/chat.rs:668-722`). Under `b4357ed`, that whole panel is bottom-pinned. The user therefore first appears just above the bottom editor.
5. The first thinking or assistant event appends a second entry (`src/chat.rs:568-620`). On the next draw, the “exclude last entry” rule makes the now-first user entry eligible for scrollback.
6. `draw_chat()` calls `take_scrollback()`, then `screen.emit()`, and only afterward computes/repaints the new panel (`src/repl.rs:572-590`). `emit()` writes the user entry at the native content cursor—row 0 after takeover—and repaints the old bottom panel (`b4357ed:core.rs:69-105`).
7. The following `render_panel()` removes the user from the panel, but leaves the streaming response there. Final geometry: user at the top, response and editor at the bottom.

This behavior is asserted by `take_scrollback_keeps_streaming_assistant_live_while_busy`: the user must flush while the partial assistant must not (`tests/chat_app.rs:555-572`). The state policy is internally consistent; its combination with a bottom-pinned panel is visually broken.

There are two secondary motion problems:

- As the live response gains rows, a bottom-anchored panel grows upward. The response's first row changes position even when its own text prefix did not.
- `panel_lines()` drops the head when live rows exceed the cap (`src/chat.rs:681-684`), and `draw_chat()` imposes another whole-panel tail cap (`src/repl.rs:584-588`). A long last response is neither in native scrollback nor fully accessible until finalization.

The fundamental rule is: **a turn must not be split across two anchors**. OpenCode avoids the jump even with a fixed composer because both user and assistant stay in one scrollbox. Pi and Qwen avoid it because both stay in one vertical main-buffer flow.

## Assessment of current `master` (`0df7963`)

The new commit moves in the right direction but should be treated as a partial implementation, not the completed architecture.

### What it already fixes

- `Screen::clear_screen()` now emits synchronized `2J` + home, omits `3J`, clears panel state, and resets origin to zero (`crates/harness-tui/src/core.rs:151-163`).
- `run_chat_tui()` calls that method instead of newline `takeover()` (`src/repl.rs:408-412`). This removes the window-height scroll-push and starts the first frame at row 0.
- Normal `render_panel()` uses `origin` rather than `height - panel_height`, and grows downward until it must naturally scroll (`core.rs:99-132`).
- `emit()` advances that same origin and repaints the old panel directly below emitted content (`core.rs:61-96`). In steady state this keeps the user and answer adjacent.

The focused suites currently pass:

- `cargo test -p harness-tui --test core`: 12/12;
- `cargo test --test chat_app`: 40/40.

Those green tests validate the current mechanics, but one of them explicitly preserves the remaining resize defect.

### Remaining findings

#### High: resize reinstates the bottom-pinned layout

`Screen::resize()` sets `origin = height - panel_len` and redraws there (`crates/harness-tui/src/core.rs:135-148`). Because the next `render_panel()` may see equal content/height and produce no diff, the panel can remain at the bottom after any resize. The test `resize_pins_panel_to_bottom_and_redraws` explicitly requires this behavior (`crates/harness-tui/tests/core.rs:123-135`).

Suggested correction: resize must never derive flow origin from the bottom edge. On a deterministic reflow/replay, reset origin to 0 and replay. On a non-reflow height update, preserve/recalculate the content-following origin and redraw the live region there.

#### High: committing and painting the next live frame are still two terminal frames

`draw_chat()` first calls `take_scrollback()`/`screen.emit()`, then separately calls `panel_lines()`/`screen.render_panel()` (`src/repl.rs:572-590`). `emit()` therefore reserves and repaints `self.panel`, which is the **old** live frame (`core.rs:67-95`). Only the second synchronized write replaces it.

Consequences:

- a terminal can visibly show a duplicated/stale panel for one frame;
- finalizing a response reserves the old large response+spinner panel even though the next panel may contain only editor/status;
- that old reserve can scroll the conversation farther upward than necessary at finalization;
- the app has no atomic guarantee that the committed prefix and next live suffix meet on adjacent rows.

Suggested correction: replace the two calls with one `Screen::present(committed, next_live)` operation and calculate scroll using `next_live.len()`, not the old panel length.

#### Medium: scrollback state advances before terminal I/O succeeds

`ChatApp::take_scrollback()` sets `self.emitted = limit` before returning (`src/chat.rs:730-752`). `draw_chat()` writes those rows only afterward (`src/repl.rs:577-580`). If output fails, the app believes entries were emitted even though they were not, so a retry omits them.

Suggested correction: make planning side-effect free and acknowledge the committed boundary only after `Screen::present()` succeeds.

#### High: tall streaming output is still truncated rather than progressively committed

`panel_lines()` drains old live rows over its cap (`src/chat.rs:681-684`), and `draw_chat()` tail-clips the full panel (`src/repl.rs:584-588`). During a long assistant response, those rows are not yet in native scrollback because the last entry remains live. The user cannot reach the missing head with native scrolling.

Suggested correction: copy Qwen's Static/pending strategy. Freeze safe, immutable response prefixes into committed chunks and retain only a bounded suffix as mutable. For a mutable running tool card, bound/collapse its detail rows rather than silently dropping arbitrary transcript rows.

#### Low: comments and names still describe the old model

Examples include `core.rs:1-6`, `core.rs:14`, `core.rs:56`, `repl.rs:384-387`, and `repl.rs:572-573`. More importantly, the startup comment says this is Pi's startup behavior (`core.rs:151-153` and test comment at `crates/harness-tui/tests/core.rs:162-163`), but Pi only defines the primitive and does not call it at coding-agent startup.

Suggested correction: use “flowing live region” terminology and describe top-clear as a harness product choice inspired by Pi's primitive, not Pi's runtime sequence.

## Recommended target model

### Non-negotiable invariants

1. Chat uses the main terminal screen; no alternate-screen enter/exit.
2. After startup claim, the first application row is terminal row 0.
3. Every displayed chat/control row belongs to one ordered flow.
4. `live_origin` is always the first row after committed content.
5. There is no vertical layout term based on `height - live_height`.
6. A finalized row is rewritten/confirmed at its current row before ownership advances; it is never reinserted at a distant row.
7. Commit plus next-live repaint is one synchronized output transaction.
8. When the flow reaches the bottom, only then does CRLF advance the terminal into native scrollback.
9. A live response that exceeds its budget freezes safe prefixes; it is not silently head-clipped.
10. On successful exit, all final transcript entries remain in the main buffer and only controls/live artifacts are erased.

### Startup sequence

Terminal setup already hides the cursor and enables bracketed paste (`crates/harness-tui/src/terminal.rs:196-200`; constants at lines 47-65). Keep that setup. Replace any scroll-push with this exact claim frame:

```text
ESC [ ? 2026 h     begin synchronized output
ESC [ 2 J          erase the visible viewport
ESC [ H            cursor home (row 1, column 1)
ESC [ ? 2026 l     end synchronized output
```

In Rust constants:

```rust
frame.push_str(esc::SYNC_BEGIN);
frame.push_str(esc::CLEAR_ALL);       // "\x1b[2J"
frame.push_str(&esc::move_to(0, 0)); // "\x1b[1;1H"; CSI H is equivalent
frame.push_str(esc::SYNC_END);
```

Explicit omissions:

- no `\r\n` takeover loop;
- no `ESC[3J` at startup, so existing scrollback is not erased;
- no `ESC[?1049h`, so chat output remains on the primary screen.

`CSI 2 J` erases the currently visible shell cells; it does not push those cells into scrollback. That is the unavoidable trade-off for an instant non-scrolling top claim. Older existing scrollback remains. The newly written chat transcript then survives exit.

### Layout when content is short and tall

Short conversation:

```text
row 0   resumed/final history (if any)
        > user message
        assistant response / spinner
        +---------------- editor ----------------+
        status / key hints
        [unused rows are below the UI]
```

Tall conversation:

```text
native scrollback: older committed history
visible row 0:     tail of committed history
                   > current user message
                   current assistant/tool tail
                   editor
                   status
```

The editor is not “pinned” in the layout algorithm. It happens to sit near the bottom only after enough content has naturally filled the viewport. This is the Pi/default-Qwen behavior and is exactly what makes short chat read naturally.

### Submit → stream → finalize

1. **Submit**
   - Capture the editor text and clear the editor.
   - Append the user entry with lifecycle `Final`; user text never needs later mutation.
   - Build a render plan whose committed prefix includes the user entry and whose live rows contain spinner + empty editor + status.
   - Atomically erase the old editor frame, write the user card at the old flow origin, and draw the reset editor immediately below. A normal small vertical shift within the same local block is acceptable; no row jumps to the opposite end of the window.

2. **First response event**
   - Create one mutable assistant/thinking/tool entry immediately after the user.
   - Draw it at `live_origin`; do not change the user's physical position.

3. **Streaming deltas**
   - Update that same mutable entry in place.
   - Grow downward. If the live frame crosses the last terminal row, emit only the required CRLF overflow and redraw the live frame at its shifted origin.
   - If the response becomes too large, split at a safe boundary (prefer a completed newline/Markdown block), commit the frozen prefix, and retain a small suffix that may still rewrap. Adjacent assistant chunks must render as one visual answer, without repeated labels or extra card gaps.

4. **Tool activity**
   - A Running tool card remains mutable.
   - Entries before the first mutable tool/card are committable.
   - When the tool result arrives, mark that card final; the next render plan can commit it at the same location.

5. **Finalize**
   - Mark the assistant suffix final.
   - In one frame, rewrite/commit its final rendering at the existing live origin and draw only editor/status immediately after it.
   - Compute terminal reserve from the **new** editor/status frame, not from the old response+spinner frame.

6. **Cancel/error**
   - Convert any partial assistant/tool state into a final visible entry and append the interruption/error entry.
   - Commit both before returning to idle. No permanently Running card may hold the commit boundary forever.

7. **Exit**
   - Commit every final entry.
   - Move to `live_origin`, erase down, and leave the cursor there; terminal teardown disables bracketed paste and restores cursor/raw mode.
   - The shell prompt then appears immediately below the surviving transcript.

### Resize

There is no portable way to rewrap rows that have already entered native scrollback while also preserving a single unduplicated copy of arbitrary pre-session scrollback. The references make a choice: Pi and Qwen clear scrollback and replay.

Recommended deterministic policy:

1. Debounce resize bursts for 150-200 ms (Qwen uses 200 ms).
2. On a settled width change—and preferably on height change for the first implementation—snapshot finalized and live state from `ChatApp`.
3. Send one synchronized hard-reflow frame:

```text
ESC [ ? 2026 h
ESC [ 2 J
ESC [ 3 J
ESC [ H
...replay all finalized history at the new width...
...draw the live suffix and controls...
ESC [ ? 2026 l
```

4. Reset the committed boundary/origin only as part of that replay and acknowledge it after a successful write.

This erases pre-session scrollback on the first resize, but the complete chat is re-emitted and still survives exit. It avoids duplicated transcript copies and width-fragment artifacts. If preserving pre-session scrollback through resize later becomes a stronger requirement, the alternative is an emulator-dependent mode that trusts terminal reflow and obtains a fresh cursor-position report through the running input parser; it is more complex and less deterministic.

Height-only optimization can be added after correctness: preserve `live_origin`, repaint the live region there, and scroll only if it no longer fits. It must never assign `origin = height - live_len`.

## Proposed code changes

The following is implementation-level pseudocode, not a patch.

### `crates/harness-tui/src/core.rs`

#### Before

The complaint baseline has separate `cursor` and a computed bottom `panel_row`; current `0df7963` improves this to one `origin`, but `emit()` still paints the old panel and `resize()` still bottom-pins.

#### After

Use explicit flow terminology and make commit + live paint one API:

```rust
pub struct Screen {
    terminal: Terminal,
    width: u16,
    height: u16,
    /// First physical row occupied by the mutable live frame.
    live_origin: u16,
    /// Last successfully painted mutable frame.
    live: Vec<Line>,
}

pub fn claim_main_screen(&mut self) -> io::Result<()> {
    let mut frame = String::from(esc::SYNC_BEGIN);
    frame.push_str(esc::CLEAR_ALL);
    frame.push_str(&esc::move_to(0, 0));
    frame.push_str(esc::SYNC_END);
    self.terminal.write_all(frame.as_bytes())?;
    self.live_origin = 0;
    self.live.clear();
    Ok(())
}

/// Permanently append `committed` at the existing live origin and paint
/// `next_live` immediately after it. Both changes become visible together.
pub fn present_flow(
    &mut self,
    committed: &[Line],
    next_live: Vec<Line>,
) -> io::Result<()> {
    let height = usize::from(self.height.max(1));
    assert!(next_live.len() <= height);

    let row0 = usize::from(self.live_origin);
    let after_commit = row0.saturating_add(committed.len());
    let after_live = after_commit.saturating_add(next_live.len());

    // A CRLF after a committed line on the last row scrolls naturally.
    let natural_scroll = after_commit.saturating_sub(height - 1);
    // Additional scroll needed so the *new* live frame fits.
    let fit_scroll = after_live.saturating_sub(height);
    let total_scroll = natural_scroll.max(fit_scroll);
    let extra_scroll = total_scroll.saturating_sub(natural_scroll);
    let next_origin = after_commit
        .saturating_sub(total_scroll)
        .min(height - 1);

    let mut frame = String::from(esc::SYNC_BEGIN);
    frame.push_str(&esc::move_to(row0, 0));
    frame.push_str(esc::CLEAR_DOWN);
    for line in committed {
        frame.push_str(&render_ansi(line));
        frame.push_str("\r\n");
    }
    if extra_scroll > 0 {
        frame.push_str(&esc::move_to(u16::try_from(height - 1).unwrap(), 0));
        frame.push_str(&"\r\n".repeat(extra_scroll));
    }
    let next_origin = u16::try_from(next_origin).unwrap();
    push_rows(&mut frame, &next_live, next_origin);
    frame.push_str(esc::SYNC_END);

    // Update cached state only after the write succeeds.
    self.terminal.write_all(frame.as_bytes())?;
    self.live_origin = next_origin;
    self.live = next_live;
    Ok(())
}
```

The first correctness implementation may fully redraw the bounded live frame every tick. Reintroduce `diff_frames()` only when `committed.is_empty()`, origin is unchanged, and the old/new frame mapping is unambiguous.

Add an explicit replay API rather than abusing normal resize:

```rust
pub enum ResizeAction {
    None,
    ReplayRequired,
}

pub fn note_size(&mut self, width: u16, height: u16) -> ResizeAction;
pub fn begin_replay(&mut self) -> io::Result<()>; // 2J + 3J + home
```

Do not keep `panel_row()` or any equivalent bottom-origin formula in flowing chat mode. If setup UI later requires a bottom-fixed mode, represent it as a separate explicit layout mode/API rather than allowing chat and setup to share ambiguous “panel” semantics.

### `src/chat.rs`

#### Before

`take_scrollback()` both decides and mutates ownership (`src/chat.rs:725-752`), while `panel_lines()` independently renders whatever remains (`chat.rs:668-722`). Mutability is inferred from “last entry while busy” plus the first Running tool.

#### After

Represent lifecycle explicitly and create a side-effect-free render plan:

```rust
enum EntryLifecycle {
    Final,
    Streaming,
    RunningTool,
}

struct ChatEntryState {
    entry: ChatEntry,
    lifecycle: EntryLifecycle,
    continuation_of: Option<TurnId>,
}

pub struct ChatRenderPlan {
    /// Entry boundary to acknowledge after terminal success.
    commit_through: usize,
    /// Final rows not previously acknowledged.
    committed_lines: Vec<Line>,
    /// Mutable transcript suffix + spinner/editor/menu/status.
    live_lines: Vec<Line>,
}

impl ChatApp {
    pub fn render_plan(&self, width: usize, height: usize) -> ChatRenderPlan;

    pub fn acknowledge_commit(&mut self, through: usize) {
        self.emitted = self.emitted.max(through);
    }
}
```

Specific state rules:

- `User`, ordinary `System`, finished tool, and finished thinking/assistant chunks are `Final` immediately.
- Only the current assistant/thinking suffix and Running tool cards are mutable.
- The commit boundary is the first mutable entry, not automatically `len - 1`.
- Split a large assistant suffix at safe boundaries into `Final` continuation chunks plus one `Streaming` suffix. Rendering adjacent chunks from the same turn must suppress duplicate spacing/labels.
- Never use `live.drain(..)` merely to satisfy viewport height unless those drained rows were first made immutable and committed.

### `src/repl.rs`

#### Before

Current `draw_chat()` performs two writes and acknowledges before the first one (`src/repl.rs:572-590`).

#### After

```rust
fn draw_chat(screen: &mut Screen, app: &mut ChatApp) -> Result<(), ReplError> {
    let plan = app.render_plan(screen.width() as usize, screen.height() as usize);
    screen
        .present_flow(&plan.committed_lines, plan.live_lines)
        .map_err(ReplError::Io)?;
    app.acknowledge_commit(plan.commit_through);
    Ok(())
}
```

`run_chat_tui()` should call `claim_main_screen()` once. `check_resize()` should schedule/debounce a replay instead of immediately calling a method that bottom-pins an old-width panel. The replay path must ask `ChatApp` to render from transcript index 0 at the settled width and include the current mutable suffix in the same final frame.

### `crates/harness-tui/tests/core.rs` and `tests/chat_app.rs`

Replace tests that encode bottom pinning with behavior-level invariants:

1. Startup bytes contain `SYNC_BEGIN`, `2J`, home, `SYNC_END`; they contain neither `3J`, `?1049h`, nor a window-height CRLF run.
2. Empty chat paints editor/status starting at row 0/next sequential row.
3. After committing one user row, the next live frame begins exactly one row (plus intentional card separator) after it.
4. A commit and live update are one synchronized write and never repaint the old live snapshot.
5. Finalizing a large response uses the new small controls height and emits no unnecessary reserve scroll.
6. Growing live output scrolls only when its end crosses the bottom.
7. Resize never computes a bottom-derived origin; replay produces exactly one transcript copy.
8. A terminal write failure does not advance `ChatApp.emitted`.
9. A long streaming answer freezes prefixes into committed output and leaves no missing head.
10. Exit clears controls but leaves all committed user/assistant text in the captured main-buffer byte stream.

Add one integration-style frame-sequence test that records each terminal write:

```text
draw idle -> submit -> first assistant delta -> second delta -> finalize
```

For every frame, reconstruct row occupancy and assert:

- the user row's coordinate does not change when the first delta arrives;
- assistant row begins directly below the user;
- editor begins directly below the assistant/live status;
- no frame places user near row 0 while assistant is derived from `height - panel_len`.

## Verification plan for a future implementation

### Automated

Run at minimum:

```powershell
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -p harness-tui
cargo test --test chat_app
cargo test
```

Add a byte-level terminal emulator/test helper that understands `CUP`, `ED`, `EL`, CRLF scrolling, and synchronized output. String containment alone cannot detect row jumps, stale-frame duplication, or unnecessary scrolling.

### Manual terminal matrix

Test in Windows Terminal/PowerShell first, then at least one xterm-compatible terminal and tmux if supported.

1. Launch from a shell prompt in the middle and at the bottom of the viewport.
2. Confirm startup is an instant clear, with no downward sweep and no alternate-screen switch.
3. Empty session: editor begins at the top, not at the bottom.
4. Submit a one-line prompt with a delayed first token: user card remains stationary while spinner is shown.
5. Stream a short answer: every line grows downward directly below the prompt.
6. Stream a response taller than the window: older response rows become wheel-scrollable before finalization; no head disappears.
7. Include Thinking → Running tool → Tool result → final answer transitions.
8. Resize narrower/wider and shorter/taller during streaming and while idle.
9. Cancel during thinking, running tool, and final response.
10. Exit and verify the complete transcript is selectable in native scrollback and the next shell prompt appears immediately below it.

### Acceptance criteria

- No startup CRLF loop proportional to terminal height.
- No startup `CSI 3J` or alternate-screen sequence.
- On a short session, no unused rows appear between the last chat row and the editor.
- The first assistant event cannot change the user's physical row except when actual bottom overflow scrolls the entire flow uniformly.
- Committed and live rows are presented atomically.
- Finished history survives normal exit in native scrollback.
- Resize cannot restore bottom pinning.
- Long live output is committed progressively, not head-clipped.

## Final architectural recommendation

Adopt the **Qwen default Static/pending ownership model** and the **Pi sequential main-buffer geometry**, with the harness-specific `2J` + home startup required by the user. Do not adopt OpenCode's alternate-screen/fixed-composer shell for chat because it conflicts with native transcript persistence. Do not keep a generic “bottom panel” abstraction for the chat path: the correct primitive is a flowing mutable suffix attached to an append-only committed prefix.

The key implementation change is not merely replacing `takeover()` or changing one origin formula. It is making ownership transfer atomic and location-preserving:

> commit the immutable prefix at the current live origin, then draw the next live suffix immediately after it, in the same synchronized terminal frame.

Everything else—natural reading order, absence of the giant gap, stable streaming, and native scrollback survival—follows from that invariant.
