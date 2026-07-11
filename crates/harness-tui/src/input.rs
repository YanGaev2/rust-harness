//! Byte-level input parser: raw stdin bytes → key/paste/mouse events.
//!
//! The parser is pure and incremental: `feed` consumes any prefix of the
//! stream it can decode and buffers partial escape/UTF-8 sequences until
//! the next `feed`; `flush` resolves an ambiguous trailing Esc when the
//! caller's read timeout expires.

/// One decoded input event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Key(KeyEvent),
    /// A bracketed-paste block (or a coalesced key burst, see
    /// [`coalesce_burst`]). Newlines are literal, never a submit.
    Paste(String),
    WheelUp,
    WheelDown,
    /// Terminal size changed (emitted by the event pump, not the parser).
    Resize(u16, u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub mods: Modifiers,
}

impl KeyEvent {
    pub fn plain(code: KeyCode) -> Self {
        KeyEvent {
            code,
            mods: Modifiers::default(),
        }
    }

    pub fn ctrl(code: KeyCode) -> Self {
        KeyEvent {
            code,
            mods: Modifiers {
                ctrl: true,
                ..Modifiers::default()
            },
        }
    }

    pub fn alt(code: KeyCode) -> Self {
        KeyEvent {
            code,
            mods: Modifiers {
                alt: true,
                ..Modifiers::default()
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    Char(char),
    Enter,
    Tab,
    BackTab,
    Backspace,
    Esc,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

const ESC: u8 = 0x1b;
/// Bracketed-paste terminator: ESC [ 2 0 1 ~
const PASTE_END: &[u8] = b"\x1b[201~";

/// Result of trying to decode one event from the head of the buffer.
enum Step {
    /// Not enough bytes yet; keep the buffer and wait for more.
    Incomplete,
    /// Consume `n` bytes, no event (unknown/ignored sequence).
    Consume(usize),
    /// Consume `n` bytes and emit the event.
    Emit(usize, Event),
    /// Consume `n` bytes and enter bracketed-paste mode.
    PasteStart(usize),
}

/// Incremental decoder from raw bytes to [`Event`]s.
#[derive(Default)]
pub struct Parser {
    pending: Vec<u8>,
    /// `Some` while inside a bracketed paste; holds the payload so far.
    paste: Option<Vec<u8>>,
}

impl Parser {
    pub fn new() -> Self {
        Parser::default()
    }

    /// Decode as many events as the accumulated bytes allow. Partial
    /// escape or UTF-8 sequences stay buffered for the next call.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        self.pending.extend_from_slice(bytes);
        let mut events = Vec::new();
        loop {
            if self.paste.is_some() {
                if !self.drain_paste(&mut events) {
                    break;
                }
                continue;
            }
            if self.pending.is_empty() {
                break;
            }
            match parse_one(&self.pending) {
                Step::Incomplete => break,
                Step::Consume(n) => {
                    self.pending.drain(..n);
                }
                Step::Emit(n, event) => {
                    self.pending.drain(..n);
                    events.push(event);
                }
                Step::PasteStart(n) => {
                    self.pending.drain(..n);
                    self.paste = Some(Vec::new());
                }
            }
        }
        events
    }

    /// Resolve pending ambiguity after a read timeout: a buffered lone
    /// Esc becomes the Esc key; a partial non-escape sequence (torn
    /// UTF-8) is dropped. Inside a paste we keep waiting.
    pub fn flush(&mut self) -> Vec<Event> {
        if self.paste.is_some() || self.pending.is_empty() {
            return Vec::new();
        }
        let mut events = Vec::new();
        if self.pending[0] == ESC {
            events.push(Event::Key(KeyEvent::plain(KeyCode::Esc)));
            let rest = self.pending.split_off(1);
            self.pending.clear();
            events.extend(self.feed(&rest));
        } else {
            self.pending.clear();
        }
        events
    }

    /// Move paste payload out of `pending`; emit the paste when the
    /// terminator arrives. Returns false when more bytes are needed.
    fn drain_paste(&mut self, events: &mut Vec<Event>) -> bool {
        let paste = self.paste.as_mut().expect("in paste mode");
        if let Some(at) = find_subslice(&self.pending, PASTE_END) {
            paste.extend_from_slice(&self.pending[..at]);
            let text = String::from_utf8_lossy(paste).into_owned();
            events.push(Event::Paste(text));
            self.paste = None;
            self.pending.drain(..at + PASTE_END.len());
            true
        } else {
            // Keep any tail that could be a prefix of the terminator.
            let safe = self.pending.len() - terminator_prefix_len(&self.pending);
            paste.extend_from_slice(&self.pending[..safe]);
            self.pending.drain(..safe);
            false
        }
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Length of the longest buffer suffix that is a proper prefix of the
/// paste terminator (so we never hand terminator bytes to the payload).
fn terminator_prefix_len(buf: &[u8]) -> usize {
    let max = (PASTE_END.len() - 1).min(buf.len());
    (1..=max)
        .rev()
        .find(|&k| buf[buf.len() - k..] == PASTE_END[..k])
        .unwrap_or(0)
}

fn parse_one(buf: &[u8]) -> Step {
    debug_assert!(!buf.is_empty());
    match buf[0] {
        ESC => parse_escape(buf),
        b'\r' | b'\n' => emit_key(1, KeyCode::Enter),
        b'\t' => emit_key(1, KeyCode::Tab),
        0x7f | 0x08 => emit_key(1, KeyCode::Backspace),
        0x00 => Step::Consume(1),
        byte @ 0x01..=0x1a => Step::Emit(
            1,
            Event::Key(KeyEvent::ctrl(KeyCode::Char((byte - 0x01 + b'a') as char))),
        ),
        0x1c..=0x1f => Step::Consume(1),
        _ => parse_utf8(buf),
    }
}

fn emit_key(n: usize, code: KeyCode) -> Step {
    Step::Emit(n, Event::Key(KeyEvent::plain(code)))
}

fn parse_escape(buf: &[u8]) -> Step {
    if buf.len() < 2 {
        return Step::Incomplete;
    }
    match buf[1] {
        b'[' => parse_csi(buf),
        b'O' => parse_ss3(buf),
        // ESC ESC: treat the first as a standalone Esc key; the second
        // is re-examined on the next iteration (it may start a sequence).
        ESC => emit_key(1, KeyCode::Esc),
        _ => parse_alt(buf),
    }
}

/// ESC O P..S / A..D / H / F — the SS3 function-key and application
/// arrow encodings.
fn parse_ss3(buf: &[u8]) -> Step {
    if buf.len() < 3 {
        return Step::Incomplete;
    }
    let code = match buf[2] {
        b'P' => KeyCode::F(1),
        b'Q' => KeyCode::F(2),
        b'R' => KeyCode::F(3),
        b'S' => KeyCode::F(4),
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        _ => return Step::Consume(3),
    };
    emit_key(3, code)
}

/// ESC + one key: the Alt-modified version of that key.
fn parse_alt(buf: &[u8]) -> Step {
    match parse_utf8(&buf[1..]) {
        Step::Incomplete => Step::Incomplete,
        Step::Emit(n, Event::Key(key)) => Step::Emit(
            n + 1,
            Event::Key(KeyEvent {
                code: key.code,
                mods: Modifiers {
                    alt: true,
                    ..key.mods
                },
            }),
        ),
        Step::Consume(n) | Step::Emit(n, _) => Step::Consume(n + 1),
        Step::PasteStart(_) => unreachable!("utf8 never starts a paste"),
    }
}

/// ESC [ params final — CSI sequences: arrows, nav keys, tilde keys,
/// SGR mouse, and the bracketed-paste markers.
fn parse_csi(buf: &[u8]) -> Step {
    // Find the final byte (0x40..=0x7e) after the parameter section.
    let mut i = 2;
    while i < buf.len() {
        match buf[i] {
            0x20..=0x3f => i += 1,
            0x40..=0x7e => break,
            // Malformed: drop the introducer and re-parse from there.
            _ => return Step::Consume(i),
        }
    }
    if i >= buf.len() {
        return Step::Incomplete;
    }
    let final_byte = buf[i];
    let params = &buf[2..i];
    let consumed = i + 1;

    if params.first() == Some(&b'<') {
        return parse_sgr_mouse(consumed, &params[1..], final_byte);
    }

    let numbers = parse_params(params);
    let mods = modifiers_from_param(numbers.get(1).copied());

    let code = match final_byte {
        b'A' => Some(KeyCode::Up),
        b'B' => Some(KeyCode::Down),
        b'C' => Some(KeyCode::Right),
        b'D' => Some(KeyCode::Left),
        b'H' => Some(KeyCode::Home),
        b'F' => Some(KeyCode::End),
        b'Z' => Some(KeyCode::BackTab),
        b'~' => match numbers.first().copied().unwrap_or(0) {
            200 => return Step::PasteStart(consumed),
            201 => return Step::Consume(consumed), // stray end marker
            n => tilde_key(n),
        },
        _ => None,
    };
    match code {
        Some(code) => Step::Emit(consumed, Event::Key(KeyEvent { code, mods })),
        None => Step::Consume(consumed),
    }
}

/// CSI < cb ; cx ; cy M/m — SGR mouse. Only the wheel matters to us.
fn parse_sgr_mouse(consumed: usize, params: &[u8], final_byte: u8) -> Step {
    if final_byte != b'M' && final_byte != b'm' {
        return Step::Consume(consumed);
    }
    let numbers = parse_params(params);
    match numbers.first().copied() {
        Some(64) if final_byte == b'M' => Step::Emit(consumed, Event::WheelUp),
        Some(65) if final_byte == b'M' => Step::Emit(consumed, Event::WheelDown),
        _ => Step::Consume(consumed),
    }
}

fn tilde_key(n: u16) -> Option<KeyCode> {
    match n {
        1 | 7 => Some(KeyCode::Home),
        2 => Some(KeyCode::Insert),
        3 => Some(KeyCode::Delete),
        4 | 8 => Some(KeyCode::End),
        5 => Some(KeyCode::PageUp),
        6 => Some(KeyCode::PageDown),
        11..=15 => Some(KeyCode::F((n - 10) as u8)),
        17..=21 => Some(KeyCode::F((n - 11) as u8)),
        23 => Some(KeyCode::F(11)),
        24 => Some(KeyCode::F(12)),
        _ => None,
    }
}

fn parse_params(params: &[u8]) -> Vec<u16> {
    params
        .split(|&b| b == b';')
        .map(|part| {
            part.iter()
                .filter(|b| b.is_ascii_digit())
                .fold(0u16, |acc, &b| {
                    acc.saturating_mul(10).saturating_add((b - b'0') as u16)
                })
        })
        .collect()
}

/// xterm modifier parameter: value-1 is a bitmask (1 shift, 2 alt, 4 ctrl).
fn modifiers_from_param(param: Option<u16>) -> Modifiers {
    let mask = param.unwrap_or(1).saturating_sub(1);
    Modifiers {
        shift: mask & 1 != 0,
        alt: mask & 2 != 0,
        ctrl: mask & 4 != 0,
    }
}

fn parse_utf8(buf: &[u8]) -> Step {
    if buf.is_empty() {
        return Step::Incomplete;
    }
    let width = match buf[0] {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => return Step::Consume(1), // invalid lead byte
    };
    if buf.len() < width {
        return Step::Incomplete;
    }
    match std::str::from_utf8(&buf[..width]) {
        Ok(text) => match text.chars().next() {
            Some(ch) => Step::Emit(width, Event::Key(KeyEvent::plain(KeyCode::Char(ch)))),
            None => Step::Consume(width),
        },
        Err(_) => Step::Consume(1),
    }
}

/// Coalesce a burst of events read in one poll window into paste blocks —
/// the same defense the previous REPL used: on the legacy Windows console a
/// paste arrives as a rapid stream of key events, so a pasted newline would
/// submit mid-paste. A lone event is always genuine; in a burst, plain text
/// keys (chars and Enter, no ctrl) merge into a single [`Event::Paste`].
pub fn coalesce_burst(events: Vec<Event>) -> Vec<Event> {
    if events.len() <= 1 {
        return events;
    }
    let mut out = Vec::new();
    let mut pasted = String::new();
    fn flush(pasted: &mut String, out: &mut Vec<Event>) {
        if !pasted.is_empty() {
            out.push(Event::Paste(std::mem::take(pasted)));
        }
    }
    for event in events {
        match event {
            Event::Key(key) if is_text_key(&key) => match key.code {
                KeyCode::Enter => pasted.push('\n'),
                KeyCode::Char(ch) => pasted.push(ch),
                _ => {}
            },
            Event::Paste(text) => {
                flush(&mut pasted, &mut out);
                out.push(Event::Paste(text));
            }
            other => {
                flush(&mut pasted, &mut out);
                out.push(other);
            }
        }
    }
    flush(&mut pasted, &mut out);
    out
}

/// Text keys are what a paste burst is made of: plain chars and Enter
/// without a control modifier.
fn is_text_key(key: &KeyEvent) -> bool {
    !key.mods.ctrl && matches!(key.code, KeyCode::Char(_) | KeyCode::Enter)
}
