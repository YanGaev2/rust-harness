# harness-tui — Design

**Date:** 2026-07-11
**Status:** Draft for review

## Purpose

`harness-tui` is our own terminal UI library, built from scratch, replacing
ratatui + crossterm for the harness front end. It does exactly what the harness
chat and setup screens need — nothing more — and is lightweight and fast.

Motivation (from the 2026-07-11 investigation of Pi, opencode, qwen-code and
hermes-agent): every mature harness draws its own caret, keeps the hardware
cursor hidden, renders line-diffed frames atomically, and (Pi, Claude Code)
writes chat history into the terminal's native scrollback. Our ratatui-based
TUI fights all of these points (hardware-cursor flicker, hand-written scroll,
untestable caret).

## Decisions (agreed in brainstorm)

| Question | Decision |
|---|---|
| Packaging | Separate crate `crates/harness-tui/` in this repo (cargo workspace); extractable later |
| Rendering model | **Line-based**, like Pi: components return lines for a width; the core diffs whole lines |
| Scope of "from scratch" | **Everything ours: output AND input.** No crossterm, no ratatui |
| Terminal support | Modern VT terminals only (Windows Terminal, VSCode, all Unix emulators; conhost on Win10+ works via the VT enable flag). Legacy non-VT consoles are a **non-goal** — harness falls back to its existing line mode |
| Screen mode | **Claude Code style**: chat history goes to the terminal's native scrollback (wheel-scrollable, selectable, survives exit); only the bottom panel (editor + spinner + status) is pinned and repainted |
| Unicode widths | Use `unicode-width` + `unicode-segmentation` (tiny, dependency-free, generated from the Unicode standard). All *logic* is ours; only reference data is imported |
| Migration | Coexistence: chat TUI migrates first, setup TUI second, then ratatui+crossterm are removed |
| Caret | Drawn (inverse-styled cell) in the Editor component; the hardware cursor is hidden for the whole session. Static (no blink) — same as Pi/qwen/opencode/Claude Code |

## Requirements

1. **Exactly what harness needs**: transcript emission, multi-line editor,
   spinner with elapsed time, status line, completion menu, select list,
   overlay stack. Non-goals: general widget zoo (tables, charts), async
   runtime, themes engine, legacy console support, Kitty keyboard protocol
   (phase 2 at the earliest).
2. **Line-based core**: `render(width) -> Vec<Line>`; the core compares the new
   frame with the previous one and rewrites only changed rows; full redraw on
   resize. Frames are wrapped in synchronized-output escapes (`CSI ?2026h/l`)
   so the terminal applies them atomically. Render requests are coalesced and
   throttled to ~60 fps.
3. **Terminal layer (output)**: raw mode (termios on Unix, `SetConsoleMode` +
   `ENABLE_VIRTUAL_TERMINAL_PROCESSING` on Windows), cursor hide/show,
   bracketed-paste and mouse-capture toggles, line clearing and cursor motion.
   **Guaranteed restore** on any exit — normal, error, or panic — via a Drop
   guard plus a panic hook.
4. **Input parser (ours)**: raw stdin bytes → events: UTF-8 text, arrows /
   Home / End / F-keys / Delete, Ctrl/Alt modifiers, Enter/Tab/Backspace/Esc,
   bracketed paste as one atomic event, SGR mouse wheel, terminal resize
   (SIGWINCH on Unix; size polling per frame on Windows). The existing
   paste-burst coalescing (legacy Windows paste arrives as a key stream)
   carries over unchanged.
5. **Text**: `Line`/`Span`/`Style` types (16/256/RGB colors, bold, italic,
   dim, underline, reverse); ANSI-aware visible width; width-aware wrapping
   that preserves styles; grapheme-cluster safety (emoji/CJK are 2 columns and
   never split).
6. **Components**: Editor (multi-line, drawn caret, prompt history),
   Spinner + elapsed timer, StatusLine, CompletionMenu, SelectList. Focus is
   owned by the core; overlays capture input while visible.
7. **Scrollback transcript**: finished chat blocks are printed into the
   terminal's native scrollback above the pinned bottom panel; streaming
   updates only repaint the in-progress tail. The hand-written transcript
   scroll from the ratatui implementation is deleted, not ported.
8. **Performance (measurable)**: streaming LLM tokens touches only the tail
   rows (the diff must not rescan the whole frame); a frame on a typical chat
   is single-digit milliseconds; no perceptible growth of the harness binary
   (`harness diagnostics` already gates size).
9. **Testability**: a headless terminal renders frames to a string buffer —
   snapshot tests see text, styles, AND the caret (impossible with the current
   ratatui setup). The input parser is tested as byte-fixture tables
   (`bytes in → events out`). The differ is tested as `(prev, next) → writes`.
10. **Dependencies**: `unicode-width` + `unicode-segmentation` only. std only,
    no async runtime, threads via `std::thread` (matches repo constraints).

## Architecture

```
crates/harness-tui/src/
├── terminal.rs   — platform layer: raw mode, VT enable, alt/inline setup,
│                   cursor hide, synchronized output, restore-on-drop + panic hook
├── input.rs      — byte parser: stdin → Event (Key/Paste/Mouse/Resize);
│                   paste-burst coalescing lives here
├── text.rs       — Line/Span/Style, visible_width, wrap, ANSI emission
├── diff.rs       — frame diff: (prev, next) → minimal row writes
├── core.rs       — component registry, focus, overlay stack, render throttle
├── components/   — editor.rs, spinner.rs, status.rs, menu.rs, select.rs
└── headless.rs   — TestTerminal: in-memory frames for snapshot tests
```

Data flow: `input::Event` → app handler mutates component state →
`request_render()` → (throttle) → core collects `Vec<Line>` from the pinned
components → `diff` against previous frame → `terminal` writes changed rows in
one synchronized burst. Transcript blocks bypass the diff: they are emitted
once into scrollback (scroll region / newline push), and the pinned panel is
repainted below them.

Unit boundaries: `text` and `diff` are pure (no I/O — fully unit-testable);
`terminal` and `input` are the only platform-specific modules; `core` and
`components` depend only on `text` + the `Event` type, so they run unchanged
against `headless`.

## Error handling

- Not a TTY / VT enable fails → constructor returns an error; harness keeps its
  existing non-TTY line mode.
- Any exit path (including panic) restores the terminal: raw mode off, cursor
  shown, mouse/paste modes off. Drop guard + `std::panic::update_hook`.
- Writes to a closed/broken stdout surface as errors to the caller (the REPL
  decides to exit); they never panic inside the library.

## Testing

Repo conventions: TDD, one test file per module.

- `text`: width/wrap tables incl. emoji, CJK, combining marks; style
  preservation across wraps.
- `diff`: row-level minimality (unchanged rows produce no writes; tail-append
  touches only the tail), resize forces full redraw.
- `input`: byte fixtures for every event class; paste-burst coalescing cases
  ported from `tests/repl.rs`; partial/split escape sequences across reads.
- `core` + `components` on `headless`: snapshot tests showing caret position
  and styles; overlay focus capture; spinner/timer row updates.
- Integration: the migrated chat TUI drives the same scenarios currently in
  `tests/chat_tui.rs` (they port, since assertions read rendered text).
- Cross-platform: full suite runs on Windows and under WSL2 Linux (same as the
  main crate today).

## Migration plan (coexistence)

1. **Foundation**: `terminal` + `text` + `diff` + `headless` with tests.
2. **Input**: the byte parser replaces crossterm event reading behind the
   existing `ChatInput` shape.
3. **Core + components**: reach feature parity with today's chat TUI (editor,
   spinner+timer, completions, overlays) against `headless`.
4. **Chat migration**: `repl.rs` switches to harness-tui; transcript moves to
   native scrollback; hand-written scroll code is deleted.
5. **Setup migration + removal**: setup TUI ports; ratatui and crossterm leave
   `Cargo.toml`.

Each phase lands green (`cargo test`, clippy, fmt, both platforms) before the
next starts; harness stays shippable throughout.

## Open questions (deferred, not blockers)

- IME hardware-cursor positioning (Pi's invisible marker trick) — phase 2+,
  needed only for CJK input.
- Kitty keyboard protocol — revisit if key-combo gaps show up in practice.
