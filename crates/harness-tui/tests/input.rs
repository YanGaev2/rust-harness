use harness_tui::input::{Event, KeyCode, KeyEvent, Modifiers, Parser, coalesce_burst};

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::plain(code))
}

fn feed_all(bytes: &[u8]) -> Vec<Event> {
    let mut parser = Parser::new();
    let mut events = parser.feed(bytes);
    events.extend(parser.flush());
    events
}

#[test]
fn ascii_char_is_a_key() {
    assert_eq!(feed_all(b"a"), vec![key(KeyCode::Char('a'))]);
}

#[test]
fn utf8_multibyte_char() {
    assert_eq!(
        feed_all("ф".as_bytes()),
        vec![key(KeyCode::Char('\u{444}'))]
    );
}

#[test]
fn utf8_split_across_feeds() {
    let bytes = "ф".as_bytes();
    let mut parser = Parser::new();
    assert_eq!(parser.feed(&bytes[..1]), vec![]);
    assert_eq!(
        parser.feed(&bytes[1..]),
        vec![key(KeyCode::Char('\u{444}'))]
    );
}

#[test]
fn four_byte_emoji_char() {
    assert_eq!(
        feed_all("\u{1f44d}".as_bytes()),
        vec![key(KeyCode::Char('\u{1f44d}'))]
    );
}

#[test]
fn control_bytes_map_to_named_keys() {
    assert_eq!(feed_all(b"\r"), vec![key(KeyCode::Enter)]);
    assert_eq!(feed_all(b"\n"), vec![key(KeyCode::Enter)]);
    assert_eq!(feed_all(b"\t"), vec![key(KeyCode::Tab)]);
    assert_eq!(feed_all(b"\x7f"), vec![key(KeyCode::Backspace)]);
}

#[test]
fn ctrl_letter_bytes_carry_ctrl_modifier() {
    assert_eq!(
        feed_all(b"\x03"),
        vec![Event::Key(KeyEvent::ctrl(KeyCode::Char('c')))]
    );
    assert_eq!(
        feed_all(b"\x01"),
        vec![Event::Key(KeyEvent::ctrl(KeyCode::Char('a')))]
    );
}

#[test]
fn lone_esc_resolves_on_flush() {
    let mut parser = Parser::new();
    assert_eq!(parser.feed(b"\x1b"), vec![]);
    assert_eq!(parser.flush(), vec![key(KeyCode::Esc)]);
}

#[test]
fn csi_arrows_and_navigation() {
    assert_eq!(feed_all(b"\x1b[A"), vec![key(KeyCode::Up)]);
    assert_eq!(feed_all(b"\x1b[B"), vec![key(KeyCode::Down)]);
    assert_eq!(feed_all(b"\x1b[C"), vec![key(KeyCode::Right)]);
    assert_eq!(feed_all(b"\x1b[D"), vec![key(KeyCode::Left)]);
    assert_eq!(feed_all(b"\x1b[H"), vec![key(KeyCode::Home)]);
    assert_eq!(feed_all(b"\x1b[F"), vec![key(KeyCode::End)]);
}

#[test]
fn csi_tilde_keys() {
    assert_eq!(feed_all(b"\x1b[2~"), vec![key(KeyCode::Insert)]);
    assert_eq!(feed_all(b"\x1b[3~"), vec![key(KeyCode::Delete)]);
    assert_eq!(feed_all(b"\x1b[5~"), vec![key(KeyCode::PageUp)]);
    assert_eq!(feed_all(b"\x1b[6~"), vec![key(KeyCode::PageDown)]);
    assert_eq!(feed_all(b"\x1b[15~"), vec![key(KeyCode::F(5))]);
    assert_eq!(feed_all(b"\x1b[24~"), vec![key(KeyCode::F(12))]);
}

#[test]
fn csi_modifiers_are_decoded() {
    // CSI 1;5C = Ctrl+Right, 1;3A = Alt+Up, 1;2D = Shift+Left.
    assert_eq!(
        feed_all(b"\x1b[1;5C"),
        vec![Event::Key(KeyEvent::ctrl(KeyCode::Right))]
    );
    assert_eq!(
        feed_all(b"\x1b[1;3A"),
        vec![Event::Key(KeyEvent {
            code: KeyCode::Up,
            mods: Modifiers {
                alt: true,
                ..Modifiers::default()
            },
        })]
    );
    assert_eq!(
        feed_all(b"\x1b[1;2D"),
        vec![Event::Key(KeyEvent {
            code: KeyCode::Left,
            mods: Modifiers {
                shift: true,
                ..Modifiers::default()
            },
        })]
    );
}

#[test]
fn csi_z_is_backtab() {
    assert_eq!(feed_all(b"\x1b[Z"), vec![key(KeyCode::BackTab)]);
}

#[test]
fn ss3_function_and_arrow_keys() {
    assert_eq!(feed_all(b"\x1bOP"), vec![key(KeyCode::F(1))]);
    assert_eq!(feed_all(b"\x1bOS"), vec![key(KeyCode::F(4))]);
    assert_eq!(feed_all(b"\x1bOA"), vec![key(KeyCode::Up)]);
    assert_eq!(feed_all(b"\x1bOH"), vec![key(KeyCode::Home)]);
}

#[test]
fn alt_enter_is_enter_with_alt() {
    // ESC CR — the Alt+Enter compose-newline binding. Must decode as
    // Enter+alt, not Char('\r').
    assert_eq!(
        feed_all(b"\x1b\r"),
        vec![Event::Key(KeyEvent::alt(KeyCode::Enter))]
    );
}

#[test]
fn alt_ctrl_letter_keeps_both_modifiers() {
    assert_eq!(
        feed_all(b"\x1b\x03"),
        vec![Event::Key(KeyEvent {
            code: KeyCode::Char('c'),
            mods: Modifiers {
                ctrl: true,
                alt: true,
                shift: false,
            },
        })]
    );
}

#[test]
fn flush_preserves_partial_utf8() {
    let bytes = "\u{444}".as_bytes(); // ф
    let mut parser = Parser::new();
    assert_eq!(parser.feed(&bytes[..1]), vec![]);
    // A read-timeout flush must not destroy the valid lead byte.
    assert_eq!(parser.flush(), vec![]);
    assert_eq!(
        parser.feed(&bytes[1..]),
        vec![key(KeyCode::Char('\u{444}'))]
    );
}

#[test]
fn esc_char_is_alt_modified() {
    assert_eq!(
        feed_all(b"\x1ba"),
        vec![Event::Key(KeyEvent {
            code: KeyCode::Char('a'),
            mods: Modifiers {
                alt: true,
                ..Modifiers::default()
            },
        })]
    );
}

#[test]
fn csi_split_across_feeds() {
    let mut parser = Parser::new();
    assert_eq!(parser.feed(b"\x1b["), vec![]);
    assert_eq!(parser.feed(b"1;5"), vec![]);
    assert_eq!(
        parser.feed(b"C"),
        vec![Event::Key(KeyEvent::ctrl(KeyCode::Right))]
    );
}

#[test]
fn bracketed_paste_is_one_event() {
    assert_eq!(
        feed_all(b"\x1b[200~hello\nworld\x1b[201~"),
        vec![Event::Paste("hello\nworld".to_string())]
    );
}

#[test]
fn bracketed_paste_split_across_feeds() {
    let mut parser = Parser::new();
    assert_eq!(parser.feed(b"\x1b[200~hel"), vec![]);
    assert_eq!(parser.feed(b"lo\x1b[2"), vec![]);
    assert_eq!(
        parser.feed(b"01~x"),
        vec![Event::Paste("hello".to_string()), key(KeyCode::Char('x'))]
    );
}

#[test]
fn sgr_mouse_wheel() {
    assert_eq!(feed_all(b"\x1b[<64;10;5M"), vec![Event::WheelUp]);
    assert_eq!(feed_all(b"\x1b[<65;10;5M"), vec![Event::WheelDown]);
}

#[test]
fn sgr_mouse_clicks_are_ignored() {
    assert_eq!(feed_all(b"\x1b[<0;10;5M\x1b[<0;10;5m"), vec![]);
}

#[test]
fn mixed_stream_in_order() {
    assert_eq!(
        feed_all(b"a\x1b[Ab"),
        vec![
            key(KeyCode::Char('a')),
            key(KeyCode::Up),
            key(KeyCode::Char('b')),
        ]
    );
}

#[test]
fn invalid_utf8_byte_is_skipped() {
    assert_eq!(feed_all(b"\xffx"), vec![key(KeyCode::Char('x'))]);
}

// --- coalesce_burst: 1:1 semantics with the existing repl coalescer ---

#[test]
fn single_event_is_never_coalesced() {
    let events = vec![key(KeyCode::Enter)];
    assert_eq!(coalesce_burst(events.clone()), events);
}

#[test]
fn burst_of_text_keys_becomes_paste() {
    let events = vec![
        key(KeyCode::Char('h')),
        key(KeyCode::Char('i')),
        key(KeyCode::Enter),
        key(KeyCode::Char('!')),
    ];
    assert_eq!(
        coalesce_burst(events),
        vec![Event::Paste("hi\n!".to_string())]
    );
}

#[test]
fn non_text_key_flushes_burst_and_passes_through() {
    let events = vec![
        key(KeyCode::Char('a')),
        key(KeyCode::Char('b')),
        key(KeyCode::Up),
        key(KeyCode::Char('c')),
        key(KeyCode::Char('d')),
    ];
    assert_eq!(
        coalesce_burst(events),
        vec![
            Event::Paste("ab".to_string()),
            key(KeyCode::Up),
            Event::Paste("cd".to_string()),
        ]
    );
}

#[test]
fn ctrl_keys_are_not_paste_text() {
    // Parity with the original repl coalescer: the ctrl key passes
    // through unmerged; the remaining text chars still form a paste.
    let events = vec![
        Event::Key(KeyEvent::ctrl(KeyCode::Char('c'))),
        key(KeyCode::Char('x')),
    ];
    assert_eq!(
        coalesce_burst(events),
        vec![
            Event::Key(KeyEvent::ctrl(KeyCode::Char('c'))),
            Event::Paste("x".to_string()),
        ]
    );
}

#[test]
fn real_paste_inside_burst_stays_separate() {
    let events = vec![
        key(KeyCode::Char('a')),
        Event::Paste("block".to_string()),
        key(KeyCode::Char('b')),
    ];
    assert_eq!(
        coalesce_burst(events),
        vec![
            Event::Paste("a".to_string()),
            Event::Paste("block".to_string()),
            Event::Paste("b".to_string()),
        ]
    );
}
