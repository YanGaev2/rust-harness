# harness-tui Phase 1 (Foundation) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the foundation layer of the `harness-tui` crate — `text` (styles, width, wrap, ANSI emission), `diff` (minimal row updates), `headless` (snapshot test terminal), and `terminal` (platform layer with guaranteed restore) — fully tested, inside a new cargo workspace.

**Architecture:** Line-based rendering per the spec (`docs/superpowers/specs/2026-07-11-harness-tui-design.md`): components will produce `Vec<Line>`, `diff` computes minimal `RowUpdate`s, `terminal` writes them inside synchronized-output frames. Phase 1 delivers the pure core (`text`, `diff`), the test double (`headless`), and the platform layer (`terminal` with hand-rolled FFI — no crossterm). Input parsing, core loop, and components are later phases.

**Tech Stack:** Rust edition 2024, `unicode-width` 0.2 + `unicode-segmentation` 1 (only deps), hand-written FFI (kernel32 on Windows, libc symbols on Linux — both already linked by std).

## Global Constraints

- Dependencies of `harness-tui`: exactly `unicode-width = "0.2"` and `unicode-segmentation = "1"`. std only, no async runtime, no crossterm/ratatui/libc crates.
- Rust edition 2024 (extern blocks must be written `unsafe extern "C" { ... }` / `unsafe extern "system" { ... }`).
- Errors are hand-rolled enums implementing `Display` + `Error` + `From` — no `anyhow`/`thiserror` (repo convention).
- Tests live in `crates/harness-tui/tests/<module>.rs`, one file per module (repo convention).
- Every task ends green on: `cargo test -p harness-tui`, `cargo fmt -- --check`, `cargo clippy --all-targets -- -D warnings` (warnings are errors).
- The existing `harness-cli` crate must keep building; workspace-wide check is `cargo test --workspace`.
- Linux verification runs under WSL2 with `CARGO_TARGET_DIR=/tmp/rust-harness-target-linux` (Windows and Linux artifacts are incompatible).
- Commit after every task.

---

### Task 1: Cargo workspace + crate scaffold

**Files:**
- Modify: `F:\rust-harness\Cargo.toml` (add `[workspace]` table at the top)
- Create: `F:\rust-harness\crates\harness-tui\Cargo.toml`
- Create: `F:\rust-harness\crates\harness-tui\src\lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: an empty `harness-tui` library crate that all later tasks add modules to. Crate name for imports in tests: `harness_tui`.

- [ ] **Step 1: Add the workspace table to the root manifest**

The root `Cargo.toml` currently starts with `[package]`. Add this table **above** it (the root package stays a workspace member automatically):

```toml
[workspace]
members = ["crates/harness-tui"]
```

- [ ] **Step 2: Create the crate manifest**

`crates/harness-tui/Cargo.toml`:

```toml
[package]
name = "harness-tui"
version = "0.1.0"
edition = "2024"

[dependencies]
unicode-segmentation = "1"
unicode-width = "0.2"
```

- [ ] **Step 3: Create the empty library root**

`crates/harness-tui/src/lib.rs`:

```rust
//! harness-tui: the harness terminal UI library, built from scratch.
//!
//! Line-based rendering: components produce lines for a width, the diff
//! computes minimal row updates, and the terminal layer writes them
//! atomically. Design spec:
//! docs/superpowers/specs/2026-07-11-harness-tui-design.md
```

- [ ] **Step 4: Verify both crates build and tests run**

Run: `cargo build --workspace`
Expected: compiles both `harness-cli` and `harness-tui` with no errors.

Run: `cargo test -p harness-tui`
Expected: `running 0 tests ... test result: ok.`

Run: `cargo test --workspace` (sanity: the existing ~242 harness-cli tests still pass)
Expected: all green.

- [ ] **Step 5: Commit**

```powershell
git add Cargo.toml Cargo.lock crates/harness-tui/Cargo.toml crates/harness-tui/src/lib.rs
git commit -m "feat: add harness-tui crate scaffold in a cargo workspace"
```

---

### Task 2: `text` — Style/Span/Line types + `visible_width`

**Files:**
- Create: `crates/harness-tui/src/text.rs`
- Modify: `crates/harness-tui/src/lib.rs` (add `pub mod text;`)
- Test: `crates/harness-tui/tests/text.rs`

**Interfaces:**
- Consumes: `unicode-width`.
- Produces (used by every later task):
  - `pub enum Color { Default, Ansi(u8), Indexed(u8), Rgb(u8, u8, u8) }`
  - `pub struct Style { pub fg: Color, pub bg: Color, pub bold: bool, pub italic: bool, pub dim: bool, pub underline: bool, pub reverse: bool }` with `Style::default()` (all off) and `fn is_plain(&self) -> bool`
  - `pub struct Span { pub text: String, pub style: Style }` with `Span::raw(text)`, `Span::styled(text, style)`
  - `pub struct Line { pub spans: Vec<Span> }` with `Line::raw(text)`, `fn width(&self) -> usize`, `fn text(&self) -> String`; derives `Default`, `Clone`, `PartialEq`, `Eq`, `Debug`
  - `pub fn visible_width(text: &str) -> usize`

- [ ] **Step 1: Write the failing tests**

`crates/harness-tui/tests/text.rs`:

```rust
use harness_tui::text::{visible_width, Line, Span, Style};

#[test]
fn width_of_ascii() {
    assert_eq!(visible_width("hello"), 5);
}

#[test]
fn width_of_empty_string_is_zero() {
    assert_eq!(visible_width(""), 0);
}

#[test]
fn width_of_cjk_is_two_columns_per_char() {
    assert_eq!(visible_width("\u{4f60}\u{597d}"), 4); // 你好
}

#[test]
fn width_of_emoji_is_two_columns() {
    assert_eq!(visible_width("\u{1f44d}"), 2); // 👍
}

#[test]
fn width_of_combining_mark_is_zero() {
    assert_eq!(visible_width("e\u{0301}"), 1); // e + combining acute
}

#[test]
fn line_width_sums_span_widths() {
    let line = Line {
        spans: vec![Span::raw("ab"), Span::raw("\u{4f60}")],
    };
    assert_eq!(line.width(), 4);
}

#[test]
fn line_text_concatenates_spans() {
    let line = Line {
        spans: vec![Span::raw("foo"), Span::raw("bar")],
    };
    assert_eq!(line.text(), "foobar");
}

#[test]
fn line_raw_builds_single_plain_span() {
    assert_eq!(
        Line::raw("hi"),
        Line {
            spans: vec![Span::raw("hi")]
        }
    );
}

#[test]
fn span_styled_keeps_style() {
    let style = Style {
        bold: true,
        ..Style::default()
    };
    let span = Span::styled("x", style);
    assert_eq!(span.style, style);
    assert!(!span.style.is_plain());
    assert!(Style::default().is_plain());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p harness-tui --test text`
Expected: FAIL to compile — `unresolved import harness_tui::text` (module does not exist yet).

- [ ] **Step 3: Write the implementation**

`crates/harness-tui/src/text.rs`:

```rust
//! Text primitives: styles, spans, lines, and Unicode-aware measurement.

use unicode_width::UnicodeWidthStr;

/// Terminal color: default, 16-color palette (0-15), 256-color palette,
/// or 24-bit RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    #[default]
    Default,
    Ansi(u8),
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// Text attributes for a span. `Style::default()` is plain text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub italic: bool,
    pub dim: bool,
    pub underline: bool,
    pub reverse: bool,
}

impl Style {
    pub fn is_plain(&self) -> bool {
        *self == Style::default()
    }
}

/// A run of text with a single style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

impl Span {
    pub fn raw(text: impl Into<String>) -> Self {
        Span {
            text: text.into(),
            style: Style::default(),
        }
    }

    pub fn styled(text: impl Into<String>, style: Style) -> Self {
        Span {
            text: text.into(),
            style,
        }
    }
}

/// One visual row: a sequence of styled spans.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Line {
    pub spans: Vec<Span>,
}

impl Line {
    pub fn raw(text: impl Into<String>) -> Self {
        Line {
            spans: vec![Span::raw(text)],
        }
    }

    /// Visible width of the whole line in terminal columns.
    pub fn width(&self) -> usize {
        self.spans.iter().map(|span| visible_width(&span.text)).sum()
    }

    /// Plain text of the line, styles stripped.
    pub fn text(&self) -> String {
        self.spans.iter().map(|span| span.text.as_str()).collect()
    }
}

/// Visible terminal width of `text`: CJK and emoji count 2 columns,
/// combining marks 0.
pub fn visible_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}
```

Add to `crates/harness-tui/src/lib.rs`:

```rust
pub mod text;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p harness-tui --test text`
Expected: `9 passed`.

- [ ] **Step 5: Format, lint, commit**

Run: `cargo fmt` then `cargo clippy --all-targets -- -D warnings`
Expected: clean.

```powershell
git add crates/harness-tui/src/lib.rs crates/harness-tui/src/text.rs crates/harness-tui/tests/text.rs
git commit -m "feat: harness-tui text types and unicode-aware visible_width"
```

---

### Task 3: `text` — width-aware `wrap`

**Files:**
- Modify: `crates/harness-tui/src/text.rs`
- Test: `crates/harness-tui/tests/text.rs` (append)

**Interfaces:**
- Consumes: `Line`, `Span`, `Style`, `visible_width` from Task 2; `unicode-segmentation` graphemes.
- Produces: `pub fn wrap(line: &Line, width: usize) -> Vec<Line>` — greedy word wrap; breaks at spaces, consumes the break space, hard-breaks words longer than `width`, never splits a grapheme cluster, preserves span styles. `width == 0` returns the line unchanged.

- [ ] **Step 1: Write the failing tests**

Append to `crates/harness-tui/tests/text.rs`:

```rust
use harness_tui::text::wrap;

#[test]
fn wrap_short_line_unchanged() {
    assert_eq!(wrap(&Line::raw("hi"), 10), vec![Line::raw("hi")]);
}

#[test]
fn wrap_empty_line_stays_one_empty_line() {
    assert_eq!(wrap(&Line::default(), 10), vec![Line::default()]);
}

#[test]
fn wrap_breaks_at_space_and_consumes_it() {
    assert_eq!(
        wrap(&Line::raw("hello world"), 5),
        vec![Line::raw("hello"), Line::raw("world")]
    );
}

#[test]
fn wrap_keeps_words_that_fit_together() {
    assert_eq!(
        wrap(&Line::raw("hello world foo"), 11),
        vec![Line::raw("hello world"), Line::raw("foo")]
    );
}

#[test]
fn wrap_moves_whole_word_to_next_line() {
    // Break happens mid-word candidate: "aa bbbb" at width 5 must not
    // emit "aa bb" — the whole word moves down.
    assert_eq!(
        wrap(&Line::raw("aa bbbb"), 5),
        vec![Line::raw("aa"), Line::raw("bbbb")]
    );
}

#[test]
fn wrap_hard_breaks_long_word() {
    assert_eq!(
        wrap(&Line::raw("abcdefghij"), 4),
        vec![Line::raw("abcd"), Line::raw("efgh"), Line::raw("ij")]
    );
}

#[test]
fn wrap_never_splits_wide_cjk_char() {
    assert_eq!(
        wrap(&Line::raw("\u{4f60}\u{597d}\u{4e16}\u{754c}"), 5), // 你好世界
        vec![
            Line::raw("\u{4f60}\u{597d}"),
            Line::raw("\u{4e16}\u{754c}")
        ]
    );
}

#[test]
fn wrap_never_splits_emoji() {
    assert_eq!(
        wrap(&Line::raw("\u{1f44d}\u{1f44d}\u{1f44d}"), 5), // 👍👍👍
        vec![
            Line::raw("\u{1f44d}\u{1f44d}"),
            Line::raw("\u{1f44d}")
        ]
    );
}

#[test]
fn wrap_drops_trailing_space_line() {
    assert_eq!(wrap(&Line::raw("hello "), 5), vec![Line::raw("hello")]);
}

#[test]
fn wrap_preserves_styles_across_break() {
    let bold = Style {
        bold: true,
        ..Style::default()
    };
    let line = Line {
        spans: vec![Span::raw("hello "), Span::styled("world", bold)],
    };
    assert_eq!(
        wrap(&line, 5),
        vec![
            Line::raw("hello"),
            Line {
                spans: vec![Span::styled("world", bold)]
            }
        ]
    );
}

#[test]
fn wrap_zero_width_returns_line_unchanged() {
    assert_eq!(wrap(&Line::raw("abc"), 0), vec![Line::raw("abc")]);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p harness-tui --test text`
Expected: FAIL to compile — `unresolved import harness_tui::text::wrap`.

- [ ] **Step 3: Write the implementation**

Append to `crates/harness-tui/src/text.rs`:

```rust
use unicode_segmentation::UnicodeSegmentation;

/// One grapheme cluster with its display width and inherited style.
struct Cell<'a> {
    grapheme: &'a str,
    width: usize,
    style: Style,
}

/// Greedy word wrap. Breaks at spaces (the break space is consumed),
/// hard-breaks words longer than `width`, never splits a grapheme
/// cluster, and preserves span styles. `width == 0` disables wrapping.
pub fn wrap(line: &Line, width: usize) -> Vec<Line> {
    if width == 0 {
        return vec![line.clone()];
    }

    let cells: Vec<Cell<'_>> = line
        .spans
        .iter()
        .flat_map(|span| {
            span.text.graphemes(true).map(move |grapheme| Cell {
                grapheme,
                width: visible_width(grapheme),
                style: span.style,
            })
        })
        .collect();

    let mut lines: Vec<Vec<Cell<'_>>> = Vec::new();
    let mut current: Vec<Cell<'_>> = Vec::new();
    let mut current_width = 0usize;
    // Index in `current` of the last space we may break after.
    let mut last_break: Option<usize> = None;

    for cell in cells {
        if current_width + cell.width > width && !current.is_empty() {
            if cell.grapheme == " " {
                // Breaking exactly on the space: flush and consume it.
                lines.push(std::mem::take(&mut current));
                current_width = 0;
                last_break = None;
                continue;
            }
            if let Some(break_at) = last_break {
                // Move the word in progress down to the next line.
                let tail = current.split_off(break_at + 1);
                while current.last().is_some_and(|c| c.grapheme == " ") {
                    current.pop();
                }
                lines.push(std::mem::take(&mut current));
                current = tail;
                current_width = current.iter().map(|c| c.width).sum();
                last_break = None;
            } else {
                // No break point: hard-break the long word.
                lines.push(std::mem::take(&mut current));
                current_width = 0;
            }
        }
        if cell.grapheme == " " {
            last_break = Some(current.len());
        }
        current_width += cell.width;
        current.push(cell);
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }

    lines.into_iter().map(cells_to_line).collect()
}

/// Rebuild a line from cells, merging adjacent cells with equal style.
fn cells_to_line(cells: Vec<Cell<'_>>) -> Line {
    let mut spans: Vec<Span> = Vec::new();
    for cell in cells {
        match spans.last_mut() {
            Some(span) if span.style == cell.style => span.text.push_str(cell.grapheme),
            _ => spans.push(Span {
                text: cell.grapheme.to_string(),
                style: cell.style,
            }),
        }
    }
    Line { spans }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p harness-tui --test text`
Expected: `20 passed`.

- [ ] **Step 5: Format, lint, commit**

Run: `cargo fmt` then `cargo clippy --all-targets -- -D warnings`
Expected: clean.

```powershell
git add crates/harness-tui/src/text.rs crates/harness-tui/tests/text.rs
git commit -m "feat: harness-tui grapheme-safe styled word wrap"
```

---

### Task 4: `text` — ANSI emission

**Files:**
- Modify: `crates/harness-tui/src/text.rs`
- Test: `crates/harness-tui/tests/text.rs` (append)

**Interfaces:**
- Consumes: `Style`, `Color`, `Line` from Task 2.
- Produces:
  - `impl Style { pub fn sgr(&self) -> String }` — full SGR escape (`"\x1b[1;7m"` style), empty string for plain.
  - `pub fn render_ansi(line: &Line) -> String` — spans concatenated; each styled span is `sgr + text + "\x1b[0m"`, plain spans pass through raw. Used by `terminal::Terminal::present` (Task 9).

- [ ] **Step 1: Write the failing tests**

Append to `crates/harness-tui/tests/text.rs`:

```rust
use harness_tui::text::{render_ansi, Color};

#[test]
fn sgr_of_plain_style_is_empty() {
    assert_eq!(Style::default().sgr(), "");
}

#[test]
fn sgr_bold() {
    let style = Style {
        bold: true,
        ..Style::default()
    };
    assert_eq!(style.sgr(), "\x1b[1m");
}

#[test]
fn sgr_reverse() {
    let style = Style {
        reverse: true,
        ..Style::default()
    };
    assert_eq!(style.sgr(), "\x1b[7m");
}

#[test]
fn sgr_ansi_foreground_normal_and_bright() {
    let normal = Style {
        fg: Color::Ansi(1),
        ..Style::default()
    };
    assert_eq!(normal.sgr(), "\x1b[31m");
    let bright = Style {
        fg: Color::Ansi(9),
        ..Style::default()
    };
    assert_eq!(bright.sgr(), "\x1b[91m");
}

#[test]
fn sgr_indexed_foreground_and_rgb_background() {
    let indexed = Style {
        fg: Color::Indexed(196),
        ..Style::default()
    };
    assert_eq!(indexed.sgr(), "\x1b[38;5;196m");
    let rgb_bg = Style {
        bg: Color::Rgb(1, 2, 3),
        ..Style::default()
    };
    assert_eq!(rgb_bg.sgr(), "\x1b[48;2;1;2;3m");
}

#[test]
fn sgr_combines_codes_in_fixed_order() {
    let style = Style {
        bold: true,
        underline: true,
        fg: Color::Ansi(2),
        ..Style::default()
    };
    assert_eq!(style.sgr(), "\x1b[1;4;32m");
}

#[test]
fn render_ansi_plain_line_is_passthrough() {
    assert_eq!(render_ansi(&Line::raw("hi")), "hi");
}

#[test]
fn render_ansi_wraps_styled_span_with_reset() {
    let bold = Style {
        bold: true,
        ..Style::default()
    };
    let line = Line {
        spans: vec![Span::raw("a"), Span::styled("b", bold), Span::raw("c")],
    };
    assert_eq!(render_ansi(&line), "a\x1b[1mb\x1b[0mc");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p harness-tui --test text`
Expected: FAIL to compile — no method `sgr`, unresolved `render_ansi`.

- [ ] **Step 3: Write the implementation**

Append to `crates/harness-tui/src/text.rs`:

```rust
impl Style {
    /// Full SGR escape sequence for this style, or an empty string for
    /// plain text. Attribute order: bold, dim, italic, underline,
    /// reverse, fg, bg.
    pub fn sgr(&self) -> String {
        if self.is_plain() {
            return String::new();
        }
        let mut codes: Vec<String> = Vec::new();
        if self.bold {
            codes.push("1".to_string());
        }
        if self.dim {
            codes.push("2".to_string());
        }
        if self.italic {
            codes.push("3".to_string());
        }
        if self.underline {
            codes.push("4".to_string());
        }
        if self.reverse {
            codes.push("7".to_string());
        }
        push_color_codes(&mut codes, self.fg, 30, 38);
        push_color_codes(&mut codes, self.bg, 40, 48);
        format!("\x1b[{}m", codes.join(";"))
    }
}

/// `base` is 30 for foreground / 40 for background; `extended` is the
/// 38/48 introducer for indexed and RGB colors.
fn push_color_codes(codes: &mut Vec<String>, color: Color, base: u8, extended: u8) {
    match color {
        Color::Default => {}
        Color::Ansi(n) if n < 8 => codes.push((base + n).to_string()),
        Color::Ansi(n) => codes.push((base + 60 + (n - 8)).to_string()),
        Color::Indexed(n) => codes.push(format!("{extended};5;{n}")),
        Color::Rgb(r, g, b) => codes.push(format!("{extended};2;{r};{g};{b}")),
    }
}

/// Render a line to a string with ANSI styling. Each styled span is
/// wrapped `SGR .. text .. reset`; plain spans pass through untouched.
pub fn render_ansi(line: &Line) -> String {
    let mut out = String::new();
    for span in &line.spans {
        if span.style.is_plain() {
            out.push_str(&span.text);
        } else {
            out.push_str(&span.style.sgr());
            out.push_str(&span.text);
            out.push_str("\x1b[0m");
        }
    }
    out
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p harness-tui --test text`
Expected: `28 passed`.

- [ ] **Step 5: Format, lint, commit**

Run: `cargo fmt` then `cargo clippy --all-targets -- -D warnings`
Expected: clean.

```powershell
git add crates/harness-tui/src/text.rs crates/harness-tui/tests/text.rs
git commit -m "feat: harness-tui SGR emission for styled lines"
```

---

### Task 5: `diff` — minimal row updates

**Files:**
- Create: `crates/harness-tui/src/diff.rs`
- Modify: `crates/harness-tui/src/lib.rs` (add `pub mod diff;`)
- Test: `crates/harness-tui/tests/diff.rs`

**Interfaces:**
- Consumes: `text::Line`.
- Produces (used by `headless` in Task 6 and `terminal` in Task 9):
  - `pub enum RowUpdate { Write { row: usize, line: Line }, Clear { row: usize } }` (derives `Debug`, `Clone`, `PartialEq`, `Eq`)
  - `pub fn diff_frames(prev: &[Line], next: &[Line]) -> Vec<RowUpdate>` — row-by-row comparison; unchanged rows produce nothing; rows present in `prev` but not `next` produce `Clear`. Full redraw = call with `prev = &[]`.

- [ ] **Step 1: Write the failing tests**

`crates/harness-tui/tests/diff.rs`:

```rust
use harness_tui::diff::{diff_frames, RowUpdate};
use harness_tui::text::Line;

fn frame(texts: &[&str]) -> Vec<Line> {
    texts.iter().map(|text| Line::raw(*text)).collect()
}

#[test]
fn identical_frames_produce_no_updates() {
    let prev = frame(&["a", "b"]);
    assert_eq!(diff_frames(&prev, &prev.clone()), vec![]);
}

#[test]
fn changed_row_is_rewritten_alone() {
    let prev = frame(&["a", "b", "c"]);
    let next = frame(&["a", "X", "c"]);
    assert_eq!(
        diff_frames(&prev, &next),
        vec![RowUpdate::Write {
            row: 1,
            line: Line::raw("X")
        }]
    );
}

#[test]
fn tail_append_touches_only_new_rows() {
    let prev = frame(&["a", "b"]);
    let next = frame(&["a", "b", "c", "d"]);
    assert_eq!(
        diff_frames(&prev, &next),
        vec![
            RowUpdate::Write {
                row: 2,
                line: Line::raw("c")
            },
            RowUpdate::Write {
                row: 3,
                line: Line::raw("d")
            },
        ]
    );
}

#[test]
fn shrunk_frame_clears_removed_rows() {
    let prev = frame(&["a", "b", "c"]);
    let next = frame(&["a"]);
    assert_eq!(
        diff_frames(&prev, &next),
        vec![RowUpdate::Clear { row: 1 }, RowUpdate::Clear { row: 2 }]
    );
}

#[test]
fn empty_prev_is_full_redraw() {
    let next = frame(&["a", "b"]);
    assert_eq!(
        diff_frames(&[], &next),
        vec![
            RowUpdate::Write {
                row: 0,
                line: Line::raw("a")
            },
            RowUpdate::Write {
                row: 1,
                line: Line::raw("b")
            },
        ]
    );
}

#[test]
fn style_only_change_is_detected() {
    use harness_tui::text::{Span, Style};
    let bold = Style {
        bold: true,
        ..Style::default()
    };
    let prev = vec![Line::raw("a")];
    let next = vec![Line {
        spans: vec![Span::styled("a", bold)],
    }];
    assert_eq!(
        diff_frames(&prev, &next),
        vec![RowUpdate::Write {
            row: 0,
            line: next[0].clone()
        }]
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p harness-tui --test diff`
Expected: FAIL to compile — `unresolved import harness_tui::diff`.

- [ ] **Step 3: Write the implementation**

`crates/harness-tui/src/diff.rs`:

```rust
//! Frame diffing: compute the minimal row updates between two frames.

use crate::text::Line;

/// One write the terminal must perform to turn the previous frame into
/// the next one. Rows are 0-based, relative to the frame origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowUpdate {
    Write { row: usize, line: Line },
    Clear { row: usize },
}

/// Compare frames row by row. Unchanged rows produce no updates; rows
/// that exist only in `prev` are cleared. Passing `prev = &[]` forces a
/// full redraw (used after resize).
pub fn diff_frames(prev: &[Line], next: &[Line]) -> Vec<RowUpdate> {
    let mut updates = Vec::new();
    for (row, line) in next.iter().enumerate() {
        if prev.get(row) != Some(line) {
            updates.push(RowUpdate::Write {
                row,
                line: line.clone(),
            });
        }
    }
    for row in next.len()..prev.len() {
        updates.push(RowUpdate::Clear { row });
    }
    updates
}
```

Add to `crates/harness-tui/src/lib.rs`:

```rust
pub mod diff;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p harness-tui --test diff`
Expected: `6 passed`.

- [ ] **Step 5: Format, lint, commit**

Run: `cargo fmt` then `cargo clippy --all-targets -- -D warnings`
Expected: clean.

```powershell
git add crates/harness-tui/src/lib.rs crates/harness-tui/src/diff.rs crates/harness-tui/tests/diff.rs
git commit -m "feat: harness-tui minimal row diff between frames"
```

---

### Task 6: `headless` — TestTerminal for snapshot tests

**Files:**
- Create: `crates/harness-tui/src/headless.rs`
- Modify: `crates/harness-tui/src/lib.rs` (add `pub mod headless;`)
- Test: `crates/harness-tui/tests/headless.rs`

**Interfaces:**
- Consumes: `diff::RowUpdate`, `text::{Line, Style, Color}`.
- Produces (the test double every later phase renders against):
  - `pub struct TestTerminal` with `TestTerminal::new(width: u16)`, `fn width(&self) -> u16`, `fn apply(&mut self, updates: &[RowUpdate])`, `fn text(&self) -> String` (plain rows joined by `\n`), `fn styled(&self) -> String` (styled spans shown as `[text]{tags}` — this is how snapshot tests *see the caret*: a reverse-styled cell renders as `[ ]{reverse}`).

- [ ] **Step 1: Write the failing tests**

`crates/harness-tui/tests/headless.rs`:

```rust
use harness_tui::diff::{diff_frames, RowUpdate};
use harness_tui::headless::TestTerminal;
use harness_tui::text::{Color, Line, Span, Style};

#[test]
fn new_terminal_is_empty() {
    let term = TestTerminal::new(80);
    assert_eq!(term.width(), 80);
    assert_eq!(term.text(), "");
}

#[test]
fn apply_writes_rows_in_order() {
    let mut term = TestTerminal::new(80);
    let next = vec![Line::raw("one"), Line::raw("two")];
    term.apply(&diff_frames(&[], &next));
    assert_eq!(term.text(), "one\ntwo");
}

#[test]
fn apply_write_beyond_end_pads_with_empty_rows() {
    let mut term = TestTerminal::new(80);
    term.apply(&[RowUpdate::Write {
        row: 2,
        line: Line::raw("x"),
    }]);
    assert_eq!(term.text(), "\n\nx");
}

#[test]
fn apply_clear_blanks_the_row() {
    let mut term = TestTerminal::new(80);
    let next = vec![Line::raw("one"), Line::raw("two")];
    term.apply(&diff_frames(&[], &next));
    term.apply(&[RowUpdate::Clear { row: 0 }]);
    assert_eq!(term.text(), "\ntwo");
}

#[test]
fn styled_snapshot_shows_caret_as_reverse_cell() {
    let mut term = TestTerminal::new(80);
    let caret = Style {
        reverse: true,
        ..Style::default()
    };
    let line = Line {
        spans: vec![Span::raw("hi "), Span::styled(" ", caret)],
    };
    term.apply(&[RowUpdate::Write { row: 0, line }]);
    assert_eq!(term.styled(), "hi [ ]{reverse}");
}

#[test]
fn styled_snapshot_lists_attributes_and_colors() {
    let mut term = TestTerminal::new(80);
    let style = Style {
        bold: true,
        fg: Color::Ansi(1),
        ..Style::default()
    };
    let line = Line {
        spans: vec![Span::styled("err", style)],
    };
    term.apply(&[RowUpdate::Write { row: 0, line }]);
    assert_eq!(term.styled(), "[err]{bold,fg=Ansi(1)}");
}

#[test]
fn styled_snapshot_of_plain_text_has_no_markers() {
    let mut term = TestTerminal::new(80);
    term.apply(&[RowUpdate::Write {
        row: 0,
        line: Line::raw("plain"),
    }]);
    assert_eq!(term.styled(), "plain");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p harness-tui --test headless`
Expected: FAIL to compile — `unresolved import harness_tui::headless`.

- [ ] **Step 3: Write the implementation**

`crates/harness-tui/src/headless.rs`:

```rust
//! In-memory terminal for snapshot tests: apply row updates, read back
//! text, styles, and the drawn caret.

use crate::diff::RowUpdate;
use crate::text::{Color, Line, Style};

/// A fake terminal that stores the current frame. Components and the
/// core render against this in tests instead of a real terminal.
pub struct TestTerminal {
    width: u16,
    rows: Vec<Line>,
}

impl TestTerminal {
    pub fn new(width: u16) -> Self {
        TestTerminal {
            width,
            rows: Vec::new(),
        }
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    /// Apply row updates the way a real terminal would.
    pub fn apply(&mut self, updates: &[RowUpdate]) {
        for update in updates {
            match update {
                RowUpdate::Write { row, line } => {
                    if self.rows.len() <= *row {
                        self.rows.resize(*row + 1, Line::default());
                    }
                    self.rows[*row] = line.clone();
                }
                RowUpdate::Clear { row } => {
                    if *row < self.rows.len() {
                        self.rows[*row] = Line::default();
                    }
                }
            }
        }
    }

    /// Plain-text snapshot: rows joined with newlines, styles stripped.
    pub fn text(&self) -> String {
        self.rows
            .iter()
            .map(Line::text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Styled snapshot: styled spans render as `[text]{tags}` so tests
    /// can assert on attributes and the drawn caret (`[ ]{reverse}`).
    pub fn styled(&self) -> String {
        self.rows
            .iter()
            .map(styled_row)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn styled_row(line: &Line) -> String {
    let mut out = String::new();
    for span in &line.spans {
        if span.style.is_plain() {
            out.push_str(&span.text);
        } else {
            out.push('[');
            out.push_str(&span.text);
            out.push_str("]{");
            out.push_str(&style_tags(&span.style));
            out.push('}');
        }
    }
    out
}

/// Tags in fixed order: bold, dim, italic, underline, reverse, fg, bg.
fn style_tags(style: &Style) -> String {
    let mut tags: Vec<String> = Vec::new();
    if style.bold {
        tags.push("bold".to_string());
    }
    if style.dim {
        tags.push("dim".to_string());
    }
    if style.italic {
        tags.push("italic".to_string());
    }
    if style.underline {
        tags.push("underline".to_string());
    }
    if style.reverse {
        tags.push("reverse".to_string());
    }
    if style.fg != Color::Default {
        tags.push(format!("fg={:?}", style.fg));
    }
    if style.bg != Color::Default {
        tags.push(format!("bg={:?}", style.bg));
    }
    tags.join(",")
}
```

Add to `crates/harness-tui/src/lib.rs`:

```rust
pub mod headless;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p harness-tui --test headless`
Expected: `7 passed`.

- [ ] **Step 5: Format, lint, commit**

Run: `cargo fmt` then `cargo clippy --all-targets -- -D warnings`
Expected: clean.

```powershell
git add crates/harness-tui/src/lib.rs crates/harness-tui/src/headless.rs crates/harness-tui/tests/headless.rs
git commit -m "feat: harness-tui headless TestTerminal with styled snapshots"
```

---

### Task 7: `terminal` — escape builders, restore sequence, error type

**Files:**
- Create: `crates/harness-tui/src/terminal.rs`
- Modify: `crates/harness-tui/src/lib.rs` (add `pub mod terminal;`)
- Test: `crates/harness-tui/tests/terminal.rs`

**Interfaces:**
- Consumes: nothing (pure strings).
- Produces (used by Tasks 8–9 and later phases):
  - `pub enum TerminalError { NotATty, Io(std::io::Error), Platform(&'static str) }` with `Display`/`Error`/`From<io::Error>`
  - `pub mod esc` with consts `HIDE_CURSOR`, `SHOW_CURSOR`, `SYNC_BEGIN`, `SYNC_END`, `BRACKETED_PASTE_ON`, `BRACKETED_PASTE_OFF`, `MOUSE_ON`, `MOUSE_OFF`, `CLEAR_LINE` and `pub fn move_to(row: u16, col: u16) -> String` (0-based API, 1-based escape). Mouse capture is provided but NOT enabled in `Terminal` setup — capturing the mouse would break native-scrollback wheel scrolling; the input phase decides when to toggle it.
  - `pub fn restore_sequence() -> &'static str` — everything a broken session must write to leave the terminal usable (includes mouse-off defensively)

- [ ] **Step 1: Write the failing tests**

`crates/harness-tui/tests/terminal.rs`:

```rust
use harness_tui::terminal::{esc, restore_sequence, TerminalError};

#[test]
fn move_to_converts_zero_based_to_one_based() {
    assert_eq!(esc::move_to(0, 0), "\x1b[1;1H");
    assert_eq!(esc::move_to(4, 9), "\x1b[5;10H");
}

#[test]
fn cursor_and_sync_escapes_are_standard() {
    assert_eq!(esc::HIDE_CURSOR, "\x1b[?25l");
    assert_eq!(esc::SHOW_CURSOR, "\x1b[?25h");
    assert_eq!(esc::SYNC_BEGIN, "\x1b[?2026h");
    assert_eq!(esc::SYNC_END, "\x1b[?2026l");
    assert_eq!(esc::BRACKETED_PASTE_ON, "\x1b[?2004h");
    assert_eq!(esc::BRACKETED_PASTE_OFF, "\x1b[?2004l");
    assert_eq!(esc::MOUSE_ON, "\x1b[?1000h\x1b[?1006h");
    assert_eq!(esc::MOUSE_OFF, "\x1b[?1006l\x1b[?1000l");
    assert_eq!(esc::CLEAR_LINE, "\x1b[2K");
}

#[test]
fn restore_sequence_undoes_every_mode_we_set() {
    let seq = restore_sequence();
    assert!(seq.contains(esc::SYNC_END));
    assert!(seq.contains(esc::MOUSE_OFF));
    assert!(seq.contains(esc::BRACKETED_PASTE_OFF));
    assert!(seq.contains(esc::SHOW_CURSOR));
}

#[test]
fn terminal_error_displays_human_messages() {
    assert_eq!(
        TerminalError::NotATty.to_string(),
        "stdin/stdout is not a terminal"
    );
    assert_eq!(
        TerminalError::Platform("tcgetattr").to_string(),
        "terminal platform call failed: tcgetattr"
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p harness-tui --test terminal`
Expected: FAIL to compile — `unresolved import harness_tui::terminal`.

- [ ] **Step 3: Write the implementation**

`crates/harness-tui/src/terminal.rs`:

```rust
//! Platform layer: escape sequences, raw mode, and guaranteed restore.

use std::fmt;
use std::io;

/// Errors from the terminal layer. Hand-rolled per repo convention.
#[derive(Debug)]
pub enum TerminalError {
    NotATty,
    Io(io::Error),
    Platform(&'static str),
}

impl fmt::Display for TerminalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TerminalError::NotATty => write!(f, "stdin/stdout is not a terminal"),
            TerminalError::Io(err) => write!(f, "terminal io error: {err}"),
            TerminalError::Platform(call) => {
                write!(f, "terminal platform call failed: {call}")
            }
        }
    }
}

impl std::error::Error for TerminalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TerminalError::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for TerminalError {
    fn from(err: io::Error) -> Self {
        TerminalError::Io(err)
    }
}

/// VT escape sequences. `move_to` takes 0-based coordinates (the escape
/// itself is 1-based).
pub mod esc {
    pub const HIDE_CURSOR: &str = "\x1b[?25l";
    pub const SHOW_CURSOR: &str = "\x1b[?25h";
    pub const SYNC_BEGIN: &str = "\x1b[?2026h";
    pub const SYNC_END: &str = "\x1b[?2026l";
    pub const BRACKETED_PASTE_ON: &str = "\x1b[?2004h";
    pub const BRACKETED_PASTE_OFF: &str = "\x1b[?2004l";
    /// SGR mouse capture (1000 = button tracking, 1006 = SGR encoding).
    /// Not enabled by `Terminal` setup: capturing the mouse breaks
    /// native-scrollback wheel scrolling. The input layer toggles it.
    pub const MOUSE_ON: &str = "\x1b[?1000h\x1b[?1006h";
    pub const MOUSE_OFF: &str = "\x1b[?1006l\x1b[?1000l";
    pub const CLEAR_LINE: &str = "\x1b[2K";

    pub fn move_to(row: u16, col: u16) -> String {
        format!("\x1b[{};{}H", row + 1, col + 1)
    }
}

/// Everything a session must write to leave the terminal usable, in
/// safe order: close any open synchronized frame, release the mouse,
/// turn bracketed paste off, show the cursor. Raw-mode restore is
/// separate (console modes, not escapes).
pub fn restore_sequence() -> &'static str {
    "\x1b[?2026l\x1b[?1006l\x1b[?1000l\x1b[?2004l\x1b[?25h"
}
```

Add to `crates/harness-tui/src/lib.rs`:

```rust
pub mod terminal;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p harness-tui --test terminal`
Expected: `4 passed`.

- [ ] **Step 5: Format, lint, commit**

Run: `cargo fmt` then `cargo clippy --all-targets -- -D warnings`
Expected: clean.

```powershell
git add crates/harness-tui/src/lib.rs crates/harness-tui/src/terminal.rs crates/harness-tui/tests/terminal.rs
git commit -m "feat: harness-tui escape builders, restore sequence, error type"
```

---

### Task 8: `terminal` — platform layer (`sys`): tty check, size, raw mode

**Files:**
- Modify: `crates/harness-tui/src/terminal.rs` (add private `mod sys` + public wrappers)
- Test: `crates/harness-tui/tests/terminal.rs` (append)

**Interfaces:**
- Consumes: `TerminalError` from Task 7.
- Produces:
  - `pub fn is_tty() -> bool` — true only when **both** stdin and stdout are terminals
  - `pub fn size() -> Result<(u16, u16), TerminalError>` — (width, height) of the stdout terminal
  - private `sys::RawModeGuard::enable() -> Result<RawModeGuard, TerminalError>` (restores on `Drop`), `sys::enable_vt()`, `sys::restore_console()` — consumed by `Terminal` in Task 9.
- Platform notes: hand-written FFI. Windows: kernel32 (`GetStdHandle`, `GetConsoleMode`, `SetConsoleMode`, `GetConsoleScreenBufferInfo`) — kernel32 is already linked by std. Linux: libc symbols (`isatty`, `tcgetattr`, `tcsetattr`, `ioctl`) — libc is already linked by std; the `Termios` layout below is linux-gnu (glibc), which matches our supported Linux (WSL2). Raw mode keeps `OPOST` untouched so `\n` still works for scrollback printing; it disables `ECHO|ICANON|ISIG|IEXTEN` and `IXON|ICRNL` so keys (including Ctrl+C) arrive as bytes.

- [ ] **Step 1: Write the failing tests**

Append to `crates/harness-tui/tests/terminal.rs`:

```rust
// Platform queries must be safe to call in any environment. Under the
// test harness stdio may or may not be a real console, so assert
// consistency, not a fixed answer.
#[test]
fn size_is_consistent_with_tty_state() {
    let tty = harness_tui::terminal::is_tty();
    let size = harness_tui::terminal::size();
    if tty {
        let (width, height) = size.expect("a tty must report its size");
        assert!(width > 0 && height > 0);
    }
    // Not a tty: size may fail — the important part is it returns an
    // error instead of panicking, which reaching this line proves.
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p harness-tui --test terminal`
Expected: FAIL to compile — `is_tty`/`size` not found.

- [ ] **Step 3: Write the implementation**

Append to `crates/harness-tui/src/terminal.rs`:

```rust
/// True only when both stdin and stdout are terminals — the TUI needs
/// raw key input *and* a screen to draw on.
pub fn is_tty() -> bool {
    sys::is_tty()
}

/// (width, height) of the stdout terminal in cells.
pub fn size() -> Result<(u16, u16), TerminalError> {
    sys::size()
}

#[cfg(unix)]
mod sys {
    use super::TerminalError;
    use std::sync::Mutex;

    // linux-gnu (glibc) layout; our supported Linux is WSL2.
    #[repr(C)]
    #[derive(Clone, Copy)]
    #[allow(dead_code)]
    struct Termios {
        c_iflag: u32,
        c_oflag: u32,
        c_cflag: u32,
        c_lflag: u32,
        c_line: u8,
        c_cc: [u8; 32],
        c_ispeed: u32,
        c_ospeed: u32,
    }

    #[repr(C)]
    struct WinSize {
        row: u16,
        col: u16,
        xpixel: u16,
        ypixel: u16,
    }

    unsafe extern "C" {
        fn isatty(fd: i32) -> i32;
        fn tcgetattr(fd: i32, termios: *mut Termios) -> i32;
        fn tcsetattr(fd: i32, optional_actions: i32, termios: *const Termios) -> i32;
        fn ioctl(fd: i32, request: u64, ...) -> i32;
    }

    const STDIN_FD: i32 = 0;
    const STDOUT_FD: i32 = 1;
    const TCSANOW: i32 = 0;
    const TIOCGWINSZ: u64 = 0x5413;
    const ISIG: u32 = 0o1;
    const ICANON: u32 = 0o2;
    const ECHO: u32 = 0o10;
    const IEXTEN: u32 = 0o100000;
    const ICRNL: u32 = 0o400;
    const IXON: u32 = 0o2000;

    static SAVED: Mutex<Option<Termios>> = Mutex::new(None);

    pub fn is_tty() -> bool {
        unsafe { isatty(STDIN_FD) == 1 && isatty(STDOUT_FD) == 1 }
    }

    /// Unix terminals speak VT natively; nothing to enable.
    pub fn enable_vt() -> Result<(), TerminalError> {
        Ok(())
    }

    pub fn size() -> Result<(u16, u16), TerminalError> {
        let mut ws = WinSize {
            row: 0,
            col: 0,
            xpixel: 0,
            ypixel: 0,
        };
        let rc = unsafe { ioctl(STDOUT_FD, TIOCGWINSZ, &mut ws as *mut WinSize) };
        if rc != 0 || ws.col == 0 {
            return Err(TerminalError::Platform("ioctl(TIOCGWINSZ)"));
        }
        Ok((ws.col, ws.row))
    }

    /// Raw input mode; restores the saved termios on drop. OPOST stays
    /// on so `\n` keeps working for scrollback printing.
    pub struct RawModeGuard {
        _private: (),
    }

    impl RawModeGuard {
        pub fn enable() -> Result<Self, TerminalError> {
            let mut original: Termios = unsafe { std::mem::zeroed() };
            if unsafe { tcgetattr(STDIN_FD, &mut original) } != 0 {
                return Err(TerminalError::Platform("tcgetattr"));
            }
            *SAVED.lock().unwrap() = Some(original);
            let mut raw = original;
            raw.c_lflag &= !(ECHO | ICANON | ISIG | IEXTEN);
            raw.c_iflag &= !(IXON | ICRNL);
            if unsafe { tcsetattr(STDIN_FD, TCSANOW, &raw) } != 0 {
                return Err(TerminalError::Platform("tcsetattr"));
            }
            Ok(RawModeGuard { _private: () })
        }
    }

    impl Drop for RawModeGuard {
        fn drop(&mut self) {
            restore_console();
        }
    }

    /// Idempotent: restores the saved termios once, then becomes a no-op.
    pub fn restore_console() {
        if let Ok(mut saved) = SAVED.lock() {
            if let Some(original) = saved.take() {
                unsafe {
                    tcsetattr(STDIN_FD, TCSANOW, &original);
                }
            }
        }
    }
}

#[cfg(windows)]
mod sys {
    use super::TerminalError;
    use std::sync::atomic::{AtomicU32, Ordering};

    type Handle = *mut core::ffi::c_void;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Coord {
        x: i16,
        y: i16,
    }

    #[repr(C)]
    struct SmallRect {
        left: i16,
        top: i16,
        right: i16,
        bottom: i16,
    }

    #[repr(C)]
    #[allow(dead_code)]
    struct ConsoleScreenBufferInfo {
        size: Coord,
        cursor_position: Coord,
        attributes: u16,
        window: SmallRect,
        maximum_window_size: Coord,
    }

    unsafe extern "system" {
        fn GetStdHandle(std_handle: u32) -> Handle;
        fn GetConsoleMode(handle: Handle, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: Handle, mode: u32) -> i32;
        fn GetConsoleScreenBufferInfo(
            handle: Handle,
            info: *mut ConsoleScreenBufferInfo,
        ) -> i32;
    }

    const STD_INPUT_HANDLE: u32 = 0xFFFF_FFF6; // (DWORD)-10
    const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5; // (DWORD)-11
    const ENABLE_PROCESSED_INPUT: u32 = 0x0001;
    const ENABLE_LINE_INPUT: u32 = 0x0002;
    const ENABLE_ECHO_INPUT: u32 = 0x0004;
    const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;

    /// u32::MAX = "nothing saved" sentinel.
    static SAVED_IN: AtomicU32 = AtomicU32::new(u32::MAX);
    static SAVED_OUT: AtomicU32 = AtomicU32::new(u32::MAX);

    pub fn is_tty() -> bool {
        let mut mode = 0u32;
        let stdin_ok =
            unsafe { GetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), &mut mode) } != 0;
        let stdout_ok =
            unsafe { GetConsoleMode(GetStdHandle(STD_OUTPUT_HANDLE), &mut mode) } != 0;
        stdin_ok && stdout_ok
    }

    /// Turn on VT escape processing for stdout (Windows 10+).
    pub fn enable_vt() -> Result<(), TerminalError> {
        let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        let mut mode = 0u32;
        if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
            return Err(TerminalError::Platform("GetConsoleMode(stdout)"));
        }
        SAVED_OUT.store(mode, Ordering::SeqCst);
        if unsafe { SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) } == 0
        {
            return Err(TerminalError::Platform("SetConsoleMode(stdout, VT)"));
        }
        Ok(())
    }

    pub fn size() -> Result<(u16, u16), TerminalError> {
        let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        let mut info: ConsoleScreenBufferInfo = unsafe { std::mem::zeroed() };
        if unsafe { GetConsoleScreenBufferInfo(handle, &mut info) } == 0 {
            return Err(TerminalError::Platform("GetConsoleScreenBufferInfo"));
        }
        let width = (info.window.right - info.window.left + 1) as u16;
        let height = (info.window.bottom - info.window.top + 1) as u16;
        Ok((width, height))
    }

    /// Raw input mode; restores saved console modes on drop.
    pub struct RawModeGuard {
        _private: (),
    }

    impl RawModeGuard {
        pub fn enable() -> Result<Self, TerminalError> {
            let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
            let mut mode = 0u32;
            if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
                return Err(TerminalError::Platform("GetConsoleMode(stdin)"));
            }
            SAVED_IN.store(mode, Ordering::SeqCst);
            let raw = (mode
                & !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT))
                | ENABLE_VIRTUAL_TERMINAL_INPUT;
            if unsafe { SetConsoleMode(handle, raw) } == 0 {
                return Err(TerminalError::Platform("SetConsoleMode(stdin, raw)"));
            }
            Ok(RawModeGuard { _private: () })
        }
    }

    impl Drop for RawModeGuard {
        fn drop(&mut self) {
            restore_console();
        }
    }

    /// Idempotent: swaps the sentinel back in, so a second call is a no-op.
    pub fn restore_console() {
        let saved_in = SAVED_IN.swap(u32::MAX, Ordering::SeqCst);
        if saved_in != u32::MAX {
            unsafe {
                SetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), saved_in);
            }
        }
        let saved_out = SAVED_OUT.swap(u32::MAX, Ordering::SeqCst);
        if saved_out != u32::MAX {
            unsafe {
                SetConsoleMode(GetStdHandle(STD_OUTPUT_HANDLE), saved_out);
            }
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p harness-tui --test terminal`
Expected: `5 passed` (the new consistency test plus Task 7's four).

Note: `RawModeGuard`/`enable_vt`/`restore_console` are `pub` inside a private `mod sys`, used by Task 9 — until then clippy may flag them as dead code. If it does, add `#[allow(dead_code)]` on those items with a `// used by Terminal (next commit)` note, and remove the allows in Task 9.

- [ ] **Step 5: Format, lint, commit**

Run: `cargo fmt` then `cargo clippy --all-targets -- -D warnings`
Expected: clean.

```powershell
git add crates/harness-tui/src/terminal.rs crates/harness-tui/tests/terminal.rs
git commit -m "feat: harness-tui platform layer: tty check, size, raw mode via own FFI"
```

---

### Task 9: `terminal` — `Terminal` struct, panic restore, smoke example

**Files:**
- Modify: `crates/harness-tui/src/terminal.rs`
- Test: `crates/harness-tui/tests/terminal.rs` (append)
- Create: `crates/harness-tui/examples/smoke.rs`

**Interfaces:**
- Consumes: `esc`, `restore_sequence`, `sys` (Task 7/8); `diff::RowUpdate` (Task 5); `text::render_ansi` (Task 4).
- Produces:
  - `pub struct Terminal` with:
    - `Terminal::stdout() -> Result<Terminal, TerminalError>` — errors `NotATty` off a terminal; enables VT + raw mode, hides cursor, turns bracketed paste on
    - `Terminal::with_backend(out: Box<dyn io::Write + Send>) -> Terminal` — same escape behavior, no tty/raw-mode requirement; this is the injection point for byte-level tests
    - `fn present(&mut self, updates: &[RowUpdate], origin_row: u16) -> io::Result<()>` — one synchronized frame: `SYNC_BEGIN`, then per update `move_to(origin_row + row, 0)` + `CLEAR_LINE` (+ `render_ansi(line)` for writes), then `SYNC_END`, flush
    - `Drop` writes `restore_sequence()` and flushes; the raw-mode guard then restores console modes
  - `pub fn install_panic_restore()` — idempotent; chains the previous panic hook
  - `pub fn restore_now()` — writes `restore_sequence()` to stderr and restores console modes; safe to call anytime

- [ ] **Step 1: Write the failing tests**

Append to `crates/harness-tui/tests/terminal.rs`:

```rust
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use harness_tui::diff::RowUpdate;
use harness_tui::terminal::Terminal;
use harness_tui::text::Line;

/// Shared byte sink so the test can read what Terminal wrote after
/// handing ownership of the writer to it.
#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl SharedBuf {
    fn contents(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn stdout_errors_when_not_a_tty() {
    // Only assert in a piped environment (CI); on a real console the
    // constructor legitimately succeeds.
    if harness_tui::terminal::is_tty() {
        return;
    }
    match Terminal::stdout() {
        Err(harness_tui::terminal::TerminalError::NotATty) => {}
        Err(other) => panic!("expected NotATty, got: {other}"),
        Ok(_) => panic!("expected NotATty error off a terminal"),
    }
}

#[test]
fn construction_hides_cursor_and_enables_bracketed_paste() {
    let buf = SharedBuf::default();
    let _term = Terminal::with_backend(Box::new(buf.clone()));
    let out = buf.contents();
    assert!(out.contains(esc::HIDE_CURSOR));
    assert!(out.contains(esc::BRACKETED_PASTE_ON));
}

#[test]
fn present_wraps_updates_in_synchronized_frame() {
    let buf = SharedBuf::default();
    let mut term = Terminal::with_backend(Box::new(buf.clone()));
    let updates = vec![RowUpdate::Write {
        row: 1,
        line: Line::raw("hi"),
    }];
    term.present(&updates, 10).unwrap();
    let out = buf.contents();
    let frame_start = out.find(esc::SYNC_BEGIN).expect("sync begin");
    let frame_end = out.find(esc::SYNC_END).expect("sync end");
    assert!(frame_start < frame_end);
    // origin 10 + row 1 → escape row 12 (1-based), column 1.
    assert!(out.contains("\x1b[12;1H"));
    assert!(out.contains(esc::CLEAR_LINE));
    assert!(out.contains("hi"));
}

#[test]
fn present_clear_row_writes_no_text() {
    let buf = SharedBuf::default();
    let mut term = Terminal::with_backend(Box::new(buf.clone()));
    let before = buf.contents();
    term.present(&[RowUpdate::Clear { row: 0 }], 5).unwrap();
    let frame = buf.contents()[before.len()..].to_string();
    assert!(frame.contains("\x1b[6;1H"));
    assert!(frame.contains(esc::CLEAR_LINE));
}

#[test]
fn present_with_no_updates_writes_nothing() {
    let buf = SharedBuf::default();
    let mut term = Terminal::with_backend(Box::new(buf.clone()));
    let before = buf.contents();
    term.present(&[], 0).unwrap();
    assert_eq!(buf.contents(), before);
}

#[test]
fn drop_writes_restore_sequence() {
    let buf = SharedBuf::default();
    {
        let _term = Terminal::with_backend(Box::new(buf.clone()));
    }
    let out = buf.contents();
    assert!(out.contains(esc::SHOW_CURSOR));
    assert!(out.contains(esc::BRACKETED_PASTE_OFF));
}

#[test]
fn install_panic_restore_is_idempotent() {
    harness_tui::terminal::install_panic_restore();
    harness_tui::terminal::install_panic_restore();
}

#[test]
fn restore_now_is_safe_anytime() {
    harness_tui::terminal::restore_now();
    harness_tui::terminal::restore_now();
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p harness-tui --test terminal`
Expected: FAIL to compile — `Terminal`, `install_panic_restore`, `restore_now` not found.

- [ ] **Step 3: Write the implementation**

Append to `crates/harness-tui/src/terminal.rs` (and remove any temporary `#[allow(dead_code)]` from Task 8):

```rust
use crate::diff::RowUpdate;
use crate::text::render_ansi;
use std::io::Write;
use std::sync::Once;

/// The live terminal: owns the output writer, keeps raw mode for its
/// lifetime, restores everything on drop.
pub struct Terminal {
    out: Box<dyn Write + Send>,
    _raw: Option<sys::RawModeGuard>,
}

impl Terminal {
    /// Attach to the real terminal. Errors with `NotATty` when stdio is
    /// piped — callers fall back to line mode.
    pub fn stdout() -> Result<Terminal, TerminalError> {
        if !sys::is_tty() {
            return Err(TerminalError::NotATty);
        }
        sys::enable_vt()?;
        let raw = sys::RawModeGuard::enable()?;
        let mut terminal = Terminal {
            out: Box::new(io::stdout()),
            _raw: Some(raw),
        };
        terminal.write_setup()?;
        Ok(terminal)
    }

    /// Same escape behavior against an injected writer; no tty or raw
    /// mode. This is how tests observe exact bytes.
    pub fn with_backend(out: Box<dyn Write + Send>) -> Terminal {
        let mut terminal = Terminal { out, _raw: None };
        let _ = terminal.write_setup();
        terminal
    }

    fn write_setup(&mut self) -> Result<(), TerminalError> {
        self.out.write_all(esc::HIDE_CURSOR.as_bytes())?;
        self.out.write_all(esc::BRACKETED_PASTE_ON.as_bytes())?;
        self.out.flush()?;
        Ok(())
    }

    /// Write one frame of row updates atomically: the whole batch is
    /// wrapped in synchronized-output markers so the terminal applies
    /// it without intermediate states. `origin_row` is the terminal row
    /// (0-based) where the pinned panel starts; update rows are
    /// relative to it.
    pub fn present(&mut self, updates: &[RowUpdate], origin_row: u16) -> io::Result<()> {
        if updates.is_empty() {
            return Ok(());
        }
        let mut frame = String::new();
        frame.push_str(esc::SYNC_BEGIN);
        for update in updates {
            match update {
                RowUpdate::Write { row, line } => {
                    frame.push_str(&esc::move_to(origin_row + *row as u16, 0));
                    frame.push_str(esc::CLEAR_LINE);
                    frame.push_str(&render_ansi(line));
                }
                RowUpdate::Clear { row } => {
                    frame.push_str(&esc::move_to(origin_row + *row as u16, 0));
                    frame.push_str(esc::CLEAR_LINE);
                }
            }
        }
        frame.push_str(esc::SYNC_END);
        self.out.write_all(frame.as_bytes())?;
        self.out.flush()
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.out.write_all(restore_sequence().as_bytes());
        let _ = self.out.flush();
        // `_raw` drops after this, restoring console modes.
    }
}

static PANIC_RESTORE: Once = Once::new();

/// Install a panic hook that restores the terminal before the default
/// hook prints the panic. Idempotent; chains the previous hook.
pub fn install_panic_restore() {
    PANIC_RESTORE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_now();
            previous(info);
        }));
    });
}

/// Best-effort immediate restore: escapes to stderr (stdout may be the
/// panicking writer) plus console-mode restore. Safe to call anytime.
pub fn restore_now() {
    let mut err = io::stderr();
    let _ = err.write_all(restore_sequence().as_bytes());
    let _ = err.flush();
    sys::restore_console();
}
```

`crates/harness-tui/examples/smoke.rs` (the manual runtime check for real terminals):

```rust
//! Manual smoke test: `cargo run -p harness-tui --example smoke`.
//! Draws a two-row pinned panel with a bold title, a frame counter,
//! and a drawn caret for ~3 seconds, then restores the terminal.

use std::thread::sleep;
use std::time::Duration;

use harness_tui::diff::diff_frames;
use harness_tui::terminal::{install_panic_restore, Terminal};
use harness_tui::text::{Color, Line, Span, Style};

fn main() {
    install_panic_restore();
    let mut term = match Terminal::stdout() {
        Ok(term) => term,
        Err(err) => {
            eprintln!("harness-tui smoke: {err}");
            return;
        }
    };
    let (width, height) = harness_tui::terminal::size().unwrap_or((80, 24));
    let origin = height.saturating_sub(3);
    let bold = Style {
        bold: true,
        ..Style::default()
    };
    let caret = Style {
        reverse: true,
        fg: Color::Ansi(6),
        ..Style::default()
    };
    let mut prev: Vec<Line> = Vec::new();
    for i in 0..30 {
        let next = vec![
            Line {
                spans: vec![Span::styled(
                    format!("harness-tui smoke on {width}x{height}"),
                    bold,
                )],
            },
            Line {
                spans: vec![Span::raw(format!("frame {i} ")), Span::styled(" ", caret)],
            },
        ];
        let updates = diff_frames(&prev, &next);
        term.present(&updates, origin).expect("present frame");
        prev = next;
        sleep(Duration::from_millis(100));
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p harness-tui --test terminal`
Expected: `13 passed`.

- [ ] **Step 5: Manual smoke on a real terminal**

Run (in an interactive terminal, not piped): `cargo run -p harness-tui --example smoke`
Expected: bottom two rows show a bold title and a counting frame with a static drawn caret block; no hardware-cursor flicker; after ~3s the prompt returns and the terminal echoes input normally (raw mode restored).

- [ ] **Step 6: Format, lint, commit**

Run: `cargo fmt` then `cargo clippy --all-targets -- -D warnings`
Expected: clean.

```powershell
git add crates/harness-tui/src/terminal.rs crates/harness-tui/tests/terminal.rs crates/harness-tui/examples/smoke.rs
git commit -m "feat: harness-tui Terminal with synchronized frames and guaranteed restore"
```

---

### Task 10: Docs + full verification sweep

**Files:**
- Modify: `crates/harness-tui/README.md` (status line)
- Modify: `README.md` (mention the workspace crate)
- Modify: `goal.md` (append changelog bullet)

**Interfaces:**
- Consumes: everything above.
- Produces: updated docs; phase 1 verified green on both platforms.

- [ ] **Step 1: Update the crate README status**

In `crates/harness-tui/README.md`, replace the line
`Status: **design approved, implementation not started.**` with:

```markdown
Status: **phase 1 (foundation) implemented** — `text`, `diff`, `headless`,
`terminal` with tests; input parser, core loop, and components are next.
```

- [ ] **Step 2: Update repo docs**

Append to the appropriate section of `goal.md` (convention: one bullet per capability):

```markdown
- harness-tui phase 1 (foundation): new `crates/harness-tui` workspace crate — styled
  line/span text model with unicode-aware width and grapheme-safe wrap, minimal row
  diff, headless snapshot terminal (sees styles and the drawn caret), and a platform
  terminal layer (own FFI: raw mode, VT enable, size, synchronized frames,
  restore-on-drop + panic hook). No crossterm/ratatui in the new crate.
```

In `README.md`, add a short subsection near the architecture/development notes:

```markdown
## Workspace

The repo is a cargo workspace. `crates/harness-tui/` is our own terminal UI
library (line-based rendering, native-scrollback screen model) that will
replace ratatui + crossterm for the interactive front ends. Run its tests with
`cargo test -p harness-tui`; run everything with `cargo test --workspace`.
```

- [ ] **Step 3: Full Windows sweep**

Run: `cargo fmt -- --check`
Expected: no diff.

Run: `cargo clippy --all-targets -- -D warnings`
Expected: clean.

Run: `cargo test --workspace`
Expected: all tests pass (existing harness-cli suite + ~40 new harness-tui tests).

- [ ] **Step 4: Linux (WSL2) sweep**

Run via Bash tool:

```bash
cd /f/rust-harness && CARGO_TARGET_DIR=/tmp/rust-harness-target-linux cargo test --workspace
cd /f/rust-harness && CARGO_TARGET_DIR=/tmp/rust-harness-target-linux cargo clippy --all-targets -- -D warnings
```

Expected: all green (this exercises the `#[cfg(unix)]` sys module).

- [ ] **Step 5: Commit**

```powershell
git add crates/harness-tui/README.md README.md goal.md
git commit -m "docs: harness-tui phase 1 foundation landed"
```
