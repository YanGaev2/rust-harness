# harness-tui

Our own terminal UI library for the harness front end — built from scratch
(output **and** input), line-based like Pi, lightweight, zero runtime
dependencies beyond `unicode-width`/`unicode-segmentation`.

Status: **complete and in production** — all five migration phases landed.
`harness` runs entirely on this library (chat TUI, setup TUI, line REPL
input); `ratatui` and `crossterm` are removed from the workspace. Modules:
`text`, `diff`, `headless`, `input`, `core` (inline Screen), `fullscreen`
(alternate-screen FullScreen), `components` (incl. `reveal` — paced text
reveal with fade-in), `theme`, `terminal`.

Core decisions:

- **Line-based rendering**: components return lines for a width; the core
  diffs rows and rewrites only what changed, inside synchronized-output frames.
- **Two screens**: `core::Screen` keeps history in the terminal's native
  scrollback and pins only a bottom panel (the setup TUI uses it).
  `fullscreen::FullScreen` owns the whole alternate screen: the app scrolls
  its own transcript, the editor stays on the bottom edge even while the
  user scrolls, and mouse events drive selection (the chat uses it).
- **Drawn caret, hidden hardware cursor** — the industry-standard fix for
  cursor flicker (verified against Pi, opencode, qwen-code, hermes-agent).
- **Own input parser**: stdin bytes → key/paste/mouse/resize events, including
  the paste-burst coalescing the harness already relies on.
- **VT terminals only** (Windows Terminal, VSCode, all Unix emulators);
  legacy non-VT consoles fall back to the harness line mode.
- **Coexistence migration**: chat TUI first, setup TUI second, then ratatui
  and crossterm are removed from the workspace.
