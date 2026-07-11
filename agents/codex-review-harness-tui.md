# Adversarial correctness review: `harness-tui` and chat integration

Scope: `crates/harness-tui/src/core.rs`, `input.rs`, `terminal.rs`, `src/repl.rs`, and `src/chat.rs`. This is a report-only review; no source files were modified.

Severity meaning:

- **critical**: normal or documented use can corrupt terminal state, invoke an unsound FFI ABI, or make independent input consumers race.
- **major**: a realistic edge case causes a crash, lost/misinterpreted input, a hung TUI, incorrect scrollback, or an agent that survives its UI.
- **minor**: a narrow timing window causes a wrong user-visible action without corrupting persistent state.

## Findings

1. **`crates/harness-tui/src/core.rs:128-139` — major — `resize()` moves the panel without clearing its old footprint and changes the logical append point.**

   **Failure scenario:** Start a 10-row screen at `origin = 0`, render a two-row panel, then resize only the width (height remains 10). `resize()` unconditionally changes `origin` to row 8 and clears only from row 8 down. The old panel at rows 0-1 remains visible. The next `emit()` also begins at row 8, so rows 2-7 become a permanent blank gap in native scrollback. A height increase has the same problem. Terminal reflow does not know which cells are the live panel and does not erase the old copy.

   **Suggested fix:** Do not conflate the transcript append row with the physical panel row. Either preserve/clamp the logical content end across resize and keep the panel attached to it, or track `content_end` and `panel_origin` separately. If the panel is deliberately moved downward, clear its old footprint before changing coordinates.

   **Before:**

   ```rust
   self.origin = self.height.saturating_sub(panel_len.max(1));
   move_to(self.origin); clear_down(); draw_panel();
   ```

   **After (shape):**

   ```rust
   let old_origin = self.origin;
   let new_content_end = old_origin.min(new_height.saturating_sub(1));
   clear_old_panel_footprint(old_origin, old_panel_len);
   self.origin = new_content_end;
   ensure_panel_fits_by_scrolling();
   draw_panel_at(self.origin);
   ```

2. **`crates/harness-tui/src/core.rs:67-92` — major — `emit()` computes an impossible row when no panel has been painted.**

   **Failure scenario:** With height 10, `origin = 9`, and an empty panel, emitting one line scrolls once and leaves the real cursor on row 9. Line 91 calculates `min(9 + 1, 10 - 0) == 10`, although valid rows are 0-9. The next panel growth then scrolls one row too many, leaving a blank row between transcript and panel; another `emit()` sends `CSI 11;1H`, outside the viewport. This is reachable on the first `draw_chat()` when resumed history is flushed before the first panel is rendered.

   **Suggested fix:** Always reserve at least the cursor row even when `panel_len == 0`, and derive the new origin from the actual total scroll count rather than `min(row0 + k, height - panel_len)`.

   **Before:**

   ```rust
   self.origin = (row0 + k).min(height.saturating_sub(panel_len));
   ```

   **After (shape):**

   ```rust
   let reserve = panel_len.max(1);
   let bottom = height.saturating_sub(reserve);
   self.origin = logical_end.saturating_sub(total_scrolled).min(bottom);
   ```

3. **`crates/harness-tui/src/core.rs:67-91,105-120,131-137,152-156` — major — unchecked `usize -> u16` conversions and `u16` arithmetic can panic, wrap, or draw outside the screen.**

   **Failure scenario:** `render_panel()` with four rows on a three-row screen computes `overflow = 1` at `origin = 0`, then `self.origin -= overflow` underflows (debug panic; release wrap). `resize()` accepts a previously oversized panel and draws every row starting at 0, including rows beyond the viewport. Independently, `emit()` truncates `lines.len()` to `u16`; a sufficiently large history batch is fully written but row math accounts for only the truncated count. Even smaller batches can overflow `row0 + k` or `row0 + k + panel_len`.

   **Suggested fix:** Validate or clip panel input before any cast; perform all row/count math in `usize` (or checked `u32`), then convert only a proven in-range final coordinate to `u16`. `render_panel()` should return `InvalidInput` or deliberately tail-clip to `height`; `push_panel_rows()` must never receive more rows than fit.

   **Before:** `let new_len = lines.len() as u16; self.origin -= overflow;`

   **After (shape):** `let new_len = lines.len(); if new_len > height { return Err(InvalidInput); }`, followed by checked/saturating wide-integer arithmetic.

4. **`crates/harness-tui/src/input.rs:259-275` — major — `parse_alt()` decodes control/named keys as literal control characters.**

   **Failure scenario:** VT terminals encode Alt as an `ESC` prefix. `ESC CR` (Alt+Enter) is passed to `parse_utf8()`, which returns `KeyCode::Char('\r')` with only `alt = true`, not `KeyCode::Enter`. `ChatApp` therefore inserts a raw carriage return into the editor instead of the advertised newline. `ESC 0x03` similarly becomes Alt+`Char('\u{3}')`, losing `ctrl = true`, so Alt+Ctrl+C does not satisfy the busy cancel rule. Alt+Backspace/Tab are misclassified for the same reason.

   **Suggested fix:** Decode the byte(s) after the Alt prefix with the normal non-escape key decoder, then add `alt = true` while preserving existing modifiers.

   **Before:** `match parse_utf8(&buf[1..]) { ... }`

   **After (shape):** `match parse_one(&buf[1..]) { Step::Emit(n, Event::Key(mut key)) => { key.mods.alt = true; ... } }` (with the already-separate `ESC ESC` case retained).

5. **`crates/harness-tui/src/input.rs:146-161` and `src/repl.rs:626-637` — major — a short inactivity timeout destroys valid partial UTF-8 and escape sequences.**

   **Failure scenario:** `InputPump` feeds one OS chunk, waits only 3 ms for another, then always calls `Parser::flush()`. If a UTF-8 scalar is split between reads and the reader thread is scheduled more than 3 ms later, `flush()` clears the valid lead byte; continuation bytes are later discarded. An `ESC [` / CSI split across the same delay is reinterpreted as a literal Esc followed by text, potentially exiting the TUI. Arbitrary stream reads are allowed to split at byte boundaries, including at the 1024-byte buffer edge.

   **Suggested fix:** Separate Esc ambiguity from stream completeness. A timeout may resolve a truly lone Esc, but it must not clear incomplete UTF-8. Keep incomplete CSI/SS3 state until an explicit Esc deadline, and add a distinct EOF/finalize operation for permanently truncated input. Use a conventional configurable Esc delay rather than making every 3 ms burst boundary a parser flush.

6. **`crates/harness-tui/src/input.rs:401-442` — major — `coalesce_burst()` mistakes legitimate queued typing for paste and loses modifiers.**

   **Failure scenario:** If `ReadFile`/`read` returns `a\r` in one burst, the parser produces Char(`a`) + Enter, and `coalesce_burst()` converts both into `Paste("a\n")`; Enter no longer submits. This can occur with terminal automation, IME commits, queued fast input, or any read that observes both keypresses as available. The predicate also ignores `alt`, so Alt+character in any multi-event burst is silently converted to unmodified paste text.

   **Suggested fix:** On the VT path, trust bracketed-paste markers and do not infer paste solely from `events.len() > 1`. If a legacy fallback is still required, isolate it behind a platform/capability-specific reader with event timing or console input-record information; at minimum require `!ctrl && !alt` and do not silently absorb an ordinary submit key.

7. **`crates/harness-tui/src/terminal.rs:442-452` — critical — Windows output mode allows an exact-width panel row to scroll immediately.**

   **Failure scenario:** `enable_vt()` enables only `ENABLE_VIRTUAL_TERMINAL_PROCESSING`. With the normal `ENABLE_WRAP_AT_EOL_OUTPUT` behavior and no `DISABLE_NEWLINE_AUTO_RETURN`, Windows moves to the next row immediately when the final column is written. The status component deliberately pads a fitting status row to exactly `screen.width()`. Painting that row on the bottom line therefore scrolls the console even though `Screen` recorded no scroll, shifting transcript and panel while leaving `origin` unchanged. Repeated status updates make the panel drift.

   **Suggested fix:** Save the original mode and request `ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN`, with the documented fallback if the last flag is unsupported. Keep restoration to the exact saved mode. Also retain a renderer invariant that no line exceeds the viewport width.

8. **`crates/harness-tui/src/terminal.rs:473-485,511-537` — major — Windows input bytes are decoded as UTF-8 without making the console input code page UTF-8.**

   **Failure scenario:** `ReadFile` on a console uses the console input code page; it is not guaranteed to be CP_UTF8 and commonly starts as an OEM code page. The parser unconditionally interprets bytes as UTF-8. Cyrillic and other non-ASCII input is therefore dropped or corrupted on such consoles, while ASCII control sequences continue to appear healthy.

   **Suggested fix:** Prefer `ReadConsoleW`/a Unicode input path and encode returned Unicode scalars to UTF-8 before feeding `Parser`. Alternatively, save `GetConsoleCP()`, set CP_UTF8 for the guard lifetime, and restore it transactionally on every exit/failure. Add a real-console/ConPTY test with Cyrillic and an emoji.

9. **`crates/harness-tui/src/terminal.rs:260-299` — critical — Linux-specific FFI is compiled for every Unix target.**

   **Failure scenario:** The module is gated by `#[cfg(unix)]`, but `Termios`, `NCCS = 32`, the `c_line` field, and `TIOCGWINSZ = 0x5413` are Linux ABI details. On macOS/BSD (which the crate README describes as supported via “all Unix emulators”), `tcgetattr()` receives a pointer to the wrong layout and may write past the Rust allocation; the ioctl number is also wrong. On 32-bit Linux, declaring the C `unsigned long request` as Rust `u64` gives the variadic call the wrong ABI/argument placement.

   **Suggested fix:** Use target-specific definitions (ideally the `libc` crate). If WSL2 x86_64 is intentionally the only Unix target, gate the module accordingly and emit a compile-time unsupported-target error instead of compiling unsound code. Declare the ioctl request as the target C `c_ulong`/`usize`, not always `u64`.

10. **`crates/harness-tui/src/terminal.rs:337-350` — major — the Unix “raw” mode inherits arbitrary `VMIN`/`VTIME` and byte-transform flags.**

   **Failure scenario:** `RawModeGuard::enable()` clears a few flags but leaves `c_cc[VMIN]` and `c_cc[VTIME]` unchanged. If the inherited terminal settings have `VMIN = 0`, a timeout can make `read()` return 0 and the reader treats that as EOF; if `VMIN > 1`, a single key can block indefinitely. Inherited `ISTRIP`, `INLCR`, `IGNCR`, or parity handling can also alter the supposedly raw VT byte stream.

   **Suggested fix:** Apply `cfmakeraw` semantics to input/control/local flags while deliberately preserving the desired output flags, and explicitly set `VMIN = 1`, `VTIME = 0` using target-correct constants. Test under a PTY whose starting termios values are non-default.

11. **`crates/harness-tui/src/terminal.rs:120-150` — major — cursor-position probing can hang forever, reject valid interleaving, and discard user input.**

   **Failure scenario:** Each `read_input()` is blocking, so the 16-iteration limit supplies no time limit if a terminal never answers DSR. The loop stops on any byte `R`, including a user typing uppercase R before the report. `parse_cursor_report()` uses the first `ESC [` in the buffer; a pending arrow sequence before the real report makes parsing fail. All non-report bytes consumed around a successful response are discarded, so keys typed during startup vanish.

   **Suggested fix:** Poll with a real deadline; scan every CSI candidate for the exact `digits;digitsR` grammar and stop only when one is found; preserve bytes before/after the report in an input prebuffer that `read_input()` drains first. On Windows, avoid DSR entirely by using `GetConsoleScreenBufferInfo` and converting the absolute cursor coordinate to viewport-relative coordinates.

12. **`crates/harness-tui/src/terminal.rs:306-367,431-509` — major — global one-slot saved modes make guards non-composable and setup is not transactional.**

   **Failure scenario:** Opening a second `Terminal`/`RawModeHandle` overwrites the saved original modes with the already-raw/current modes. Dropping either guard consumes the one slot; dropping the other becomes a no-op, leaving the terminal raw/VT-enabled. Also, if Windows output VT enable succeeds and input raw-mode setup then fails, `Terminal::stdout()` returns before any guard owns/restores the output change.

   **Suggested fix:** Give each guard its own saved state and use a process-wide owner/refcount only for panic recovery, or reject nested activation explicitly. Construct setup as a rollback guard so every partially successful mode change is restored on subsequent failure. For a single successfully constructed `Terminal`, the current escape-before-raw field-drop ordering is correct; the bug is ownership/state multiplicity.

13. **`src/repl.rs:594-620` — critical — `InputPump` detaches a permanently blocking stdin reader that can race the next TUI.**

   **Failure scenario:** `start()` discards the `JoinHandle`; dropping `InputPump` only drops the channel receiver and cannot interrupt `read_input()`. The default setup flow returns from setup TUI and immediately starts chat in the same process, creating a second reader. The old and new threads race for stdin; if the old one wins, it consumes the first chat keystroke/chunk, fails to send because its receiver is gone, and discards the bytes. A library caller that returns to another interactive frontend has the same problem.

   **Suggested fix:** Make the pump own a `JoinHandle` plus a cancellation/unblock mechanism and join it before raw mode is restored. On Unix use `poll`/a wake fd or nonblocking reads; on Windows use a cancellable wait/read strategy. Another viable design is one process-lifetime input service reused across setup and chat, but there must never be two independent readers of stdin.

14. **`src/repl.rs:602-615,625-639` — major — EOF/read failure is converted into permanent idle polling.**

   **Failure scenario:** The reader exits on `Ok(0)` or any `Err`, dropping the sender. `poll()` maps `Disconnected` to `Ok(Vec::new())`. Both setup and chat loops then redraw and poll forever with no way to exit or report the original I/O error; because a disconnected channel returns immediately, this becomes a hot loop rather than honoring the requested timeout. An unterminated bracketed paste is also retained forever because `Parser::flush()` refuses to finalize paste mode.

   **Suggested fix:** Send an enum such as `Bytes(Vec<u8>) | Eof | Error(io::Error)` and make `poll()` surface EOF/error distinctly. The outer TUI should exit cleanly on EOF and return the I/O error on failure. Define explicit EOF behavior for incomplete UTF-8, escape sequences, and bracketed paste.

15. **`src/repl.rs:431-436,509-526,579-587` — major — terminal size is never refreshed during a busy run and is checked after drawing while idle.**

   **Failure scenario:** A long agent run draws every loop using the pre-run width/height; `check_resize()` exists only in the outer idle loop. If the user shrinks the terminal, `draw_chat()` caps to the stale height and `Screen` emits absolute rows beyond the real viewport, causing scroll/origin corruption. Even while idle, the code draws once, waits up to 400 ms, and only then notices the resize.

   **Suggested fix:** Check size before every draw in both loops. On a change, update screen geometry, rebuild all width-dependent lines, and render only the newly laid-out panel. This should be coordinated with finding 1 so `Screen::resize()` does not first repaint stale-width rows.

16. **`src/repl.rs:504-526` — major — an output error detaches an active agent/tool worker without cancellation or join.**

   **Failure scenario:** Any `draw_chat(...)?`/terminal-output failure can return from `run_chat_tui()` while `worker` still owns a runner. Dropping a Rust `JoinHandle` detaches the thread; the agent can continue network calls and workspace tools after its UI/session loop has exited, while final events are discarded and `ChatSession` is never completed/failed.

   **Suggested fix:** Wrap the worker in an RAII owner. Every early-return path must set the cancel flag, continue draining or discard explicitly, and join before returning. Replace `join().expect(...)` with an error path that marks the turn failed while still restoring the terminal.

17. **`src/repl.rs:510-525` and `src/chat.rs:310-317` — minor — a queued cancel key can become an idle “exit” after worker completion.**

   **Failure scenario:** The loop checks `worker.is_finished()` before polling input. If Esc is already queued when the worker finishes, it breaks without consuming Esc as `BusyAction::Cancel`. The next outer poll delivers the same key to `ChatApp::handle_key()`, where Esc exits the whole session. The observed action depends on a narrow completion race rather than on the mode in which the key was pressed.

   **Suggested fix:** Drain immediately available busy input before transitioning to idle, or tag input with a mode/generation and discard cancel keys from the completed generation. Reordering to poll/process pending input before the final `is_finished()` break is sufficient for the common case.

18. **`src/chat.rs:726-744` — major — an already-emitted stale `Running` card blocks the entire next busy turn from flushing.**

   **Failure scenario:** A cancelled/failed run can end after `ToolCallStarted` without `ToolResult`. Once `busy` becomes false, the explicit rule flushes that still-`Running` card and advances `emitted` past it. On the next run, `.iter().position(...)` finds the historical card before `emitted` and sets `limit` below `emitted`; every busy `take_scrollback()` returns empty. All finalized entries from the new turn remain in the capped live panel until the run ends, so most can disappear from view during a long run.

   **Suggested fix:** Search for `Running` only in `self.transcript[self.emitted..]` and add the offset back to `emitted`. Also finalize any still-running cards as cancelled/failed before the not-busy full flush.

   **Before:** `self.transcript.iter().position(is_running)`

   **After:** `self.transcript[self.emitted..].iter().position(is_running).map(|n| self.emitted + n)`

19. **`src/chat.rs:668-680,750-765,795-799,916-973` — major — panel row caps count logical lines that are not guaranteed to occupy one terminal row.**

   **Failure scenario:** `wrap_styled_line()` uses `char` count, not terminal display width or grapheme clusters. Forty CJK characters fit the `chars.len() <= 80` fast path but occupy 80 columns before prefixes; emoji/ZWJ sequences are similarly mishandled. Separately, `status_row()` passes an unbounded provider/workspace label to `status_line`; when the left side alone exceeds the terminal width, that component returns it unchanged. The terminal auto-wraps either “one” `Line` into multiple physical rows, while `panel.len()` and `Screen::origin` account for one. The final `split_off` cap is index-safe but cannot prevent the resulting panel overflow/origin drift.

   **Suggested fix:** Make `entry_lines()` and the status row enforce `Line::width() <= width` using grapheme-aware `visible_width` logic (the `harness_tui::text` module already provides this). Preserve hanging indent in display columns, and truncate/ellipsize the status left side when it alone is too wide. Add a debug assertion at the `Screen` boundary that every supplied row is newline-free and no wider than `screen.width()`.

## Checked paths with no additional finding

- `terminator_prefix_len()` correctly retains the longest suffix that may be the start of `ESC [ 201 ~`, including a terminator split at every chunk boundary.
- Every currently reachable `Step::Consume`, `Step::Emit`, and `Step::PasteStart` consumes at least one byte; the `feed()` loop has no zero-consumption infinite loop in the reviewed code.
- The post-join `event_rx.try_recv()` drain prevents the specific race where agent events arrive after the pre-`is_finished()` drain.
- With no historical stale `Running` card, `take_scrollback()` advances a monotonic entry prefix and does not double-emit it; `live.drain(...)` and `draw_chat()`'s `split_off(...)` indices are guarded and do not themselves panic.
- For one successfully constructed terminal guard, `Terminal::drop()` writes restore escapes before the raw-mode field is dropped, which is the correct order.

## Verification performed

- `cargo test --workspace --all-targets` — passed on `x86_64-pc-windows-msvc` (the existing suite does not cover the adversarial scenarios above).
- Compared the Linux x86_64 glibc `termios` layout and `TIOCGWINSZ` value with the installed WSL2 headers. They match the current struct/constant for that one target; the finding is the broader `cfg(unix)`/ABI claim and 32-bit request type.
- Validated Windows VT wrapping, DSR, input-mode, and console-code-page behavior against Microsoft’s primary documentation:
  - <https://learn.microsoft.com/en-us/windows/console/high-level-console-modes>
  - <https://learn.microsoft.com/en-us/windows/console/console-virtual-terminal-sequences>
  - <https://learn.microsoft.com/en-us/windows/console/console-code-pages>
- Validated detached `JoinHandle` semantics against the Rust standard-library documentation: <https://doc.rust-lang.org/stable/std/thread/struct.JoinHandle.html>.

## Regression tests to add with the fixes

1. **Core byte-backend/model tests:** resize a non-bottom panel without changing height; assert the old footprint is cleared and the next emit has no gap. Cover empty-panel emission at the last row, panel lengths `height` and `height + 1`, height 1, and emission counts around `u16::MAX` without allocating an unbounded frame.
2. **Parser table tests:** feed every escape/paste sequence at every byte split; test `ESC CR`, `ESC DEL`, `ESC 0x03`, delayed CSI tails, delayed UTF-8 tails, EOF in each parser state, and assert each loop step makes progress.
3. **Pump lifecycle tests with an injected reader:** prove Drop stops/joins the reader; setup-to-chat handoff has exactly one reader; EOF exits; read error propagates; a split UTF-8 character survives a delay longer than the burst window.
4. **PTY/ConPTY integration tests:** exact-width bottom status must not change the reported panel origin; Cyrillic/emoji input round-trips; DSR with pending `R`, arrow keys, and surrounding text preserves those bytes; no DSR response times out.
5. **Busy-loop tests:** resize during a blocked worker, terminal write failure during a worker, Esc queued immediately before completion, and final event delivery between the first drain and thread completion.
6. **Chat invariants:** after a cancelled run leaves a stale Running card and it is flushed, a second busy turn must continue advancing `emitted`; for long ASCII paths, CJK, combining marks, and emoji, assert every returned `Line` has `line.width() <= width` and the scrollback prefix is emitted exactly once.
7. Final gates after implementation: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --all-targets`, plus the PTY/ConPTY checks on both Windows and WSL2.
