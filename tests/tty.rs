//! Terminal logic without threads: the line-discipline contract (see the
//! table in `src/tty.rs`), scancode decoding, and VGA control bytes.
//!
//! Every case uses its own `LineDiscipline` / `KeyDecoder`, so the global
//! console state is never touched. Interrupts stay off for the whole suite,
//! so no real keyboard IRQ can interfere either.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(support::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::vec::Vec;
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use pc_keyboard::{DecodedKey, KeyCode};
use rust_os::keyboard::KeyDecoder;
use rust_os::tty::{LineDiscipline, TtyMode, INPUT_CAPACITY, LINE_MAX};

entry_point!(test_kernel_main);

fn test_kernel_main(boot_info: &'static BootInfo) -> ! {
    rust_os::init(boot_info);
    test_main();
    rust_os::hlt_loop()
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    support::test_panic_handler(info)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const BACKSPACE: char = '\u{8}';
const CTRL_C: char = '\u{3}';
const CTRL_D: char = '\u{4}';

fn key(c: char) -> DecodedKey {
    DecodedKey::Unicode(c)
}

/// Type every char of `text`, returning everything that was echoed.
fn type_str(ld: &mut LineDiscipline, text: &str) -> Vec<u8> {
    let mut echoed = Vec::new();
    for c in text.chars() {
        echoed.extend_from_slice(ld.input(key(c)).as_bytes());
    }
    echoed
}

/// One `read` with a buffer of `size` bytes; panics if nothing is readable.
fn read_once(ld: &mut LineDiscipline, size: usize) -> Vec<u8> {
    assert!(ld.readable(), "read_once on a terminal with nothing readable");
    let mut buf = alloc::vec![0u8; size];
    let n = ld.read(&mut buf);
    buf.truncate(n);
    buf
}

fn raw() -> LineDiscipline {
    let mut ld = LineDiscipline::new();
    ld.set_mode(TtyMode::Raw);
    ld
}

// ---------------------------------------------------------------------------
// Cooked mode
// ---------------------------------------------------------------------------

#[test_case]
fn starts_cooked_and_empty() {
    let ld = LineDiscipline::new();
    assert_eq!(ld.mode(), TtyMode::Cooked);
    assert!(!ld.readable());
}

#[test_case]
fn cooked_echoes_and_commits_on_enter() {
    let mut ld = LineDiscipline::new();
    assert_eq!(type_str(&mut ld, "hi"), b"hi");
    assert!(!ld.readable(), "nothing is readable before Enter");

    assert_eq!(ld.input(key('\n')).as_bytes(), b"\n");
    assert!(ld.readable());
    assert_eq!(read_once(&mut ld, 64), b"hi\n");
    assert!(!ld.readable());
}

#[test_case]
fn cooked_backspace_edits_the_line() {
    let mut ld = LineDiscipline::new();
    type_str(&mut ld, "ab");
    assert_eq!(ld.input(key(BACKSPACE)).as_bytes(), b"\x08 \x08");
    type_str(&mut ld, "c\n");
    assert_eq!(read_once(&mut ld, 64), b"ac\n");

    // Backspace on an empty line does nothing, and doesn't eat the previous
    // (already committed) line either.
    type_str(&mut ld, "x\n");
    assert_eq!(ld.input(key(BACKSPACE)).as_bytes(), b"");
    assert_eq!(read_once(&mut ld, 64), b"x\n");
}

#[test_case]
fn cooked_read_returns_one_line_at_a_time() {
    let mut ld = LineDiscipline::new();
    type_str(&mut ld, "one\ntwo\n");
    assert_eq!(read_once(&mut ld, 64), b"one\n");
    assert_eq!(read_once(&mut ld, 64), b"two\n");
    assert!(!ld.readable());
}

#[test_case]
fn cooked_partial_reads_continue_the_line() {
    let mut ld = LineDiscipline::new();
    type_str(&mut ld, "hello\nyo\n");
    assert_eq!(read_once(&mut ld, 2), b"he");
    assert_eq!(read_once(&mut ld, 2), b"ll");
    assert_eq!(read_once(&mut ld, 64), b"o\n", "rest of the line, not the next one");
    assert_eq!(read_once(&mut ld, 64), b"yo\n");
}

#[test_case]
fn cooked_ctrl_d_on_empty_line_is_eof() {
    let mut ld = LineDiscipline::new();
    assert_eq!(ld.input(key(CTRL_D)).as_bytes(), b"");
    assert!(ld.readable(), "a pending EOF is readable");
    let mut buf = [0u8; 8];
    assert_eq!(ld.read(&mut buf), 0, "EOF reads as 0 bytes");
    assert!(!ld.readable(), "an EOF is consumed by one read");

    // Each Ctrl-D on an empty line is its own EOF, in order with lines.
    type_str(&mut ld, "a\n");
    ld.input(key(CTRL_D));
    ld.input(key(CTRL_D));
    assert_eq!(read_once(&mut ld, 8), b"a\n");
    assert_eq!(ld.read(&mut buf), 0);
    assert!(ld.readable());
    assert_eq!(ld.read(&mut buf), 0);
    assert!(!ld.readable());
}

#[test_case]
fn cooked_ctrl_d_flushes_a_partial_line() {
    let mut ld = LineDiscipline::new();
    type_str(&mut ld, "ab");
    ld.input(key(CTRL_D));
    assert_eq!(read_once(&mut ld, 64), b"ab", "no newline added, and not EOF");
    assert!(!ld.readable());
}

#[test_case]
fn cooked_ctrl_c_discards_the_line() {
    let mut ld = LineDiscipline::new();
    type_str(&mut ld, "abc");
    assert_eq!(ld.input(key(CTRL_C)).as_bytes(), b"^C\n");
    assert!(!ld.readable());
    type_str(&mut ld, "x\n");
    assert_eq!(read_once(&mut ld, 64), b"x\n");
}

#[test_case]
fn cooked_ignores_arrows_and_unsupported_keys() {
    let mut ld = LineDiscipline::new();
    for raw_key in [KeyCode::ArrowUp, KeyCode::ArrowLeft, KeyCode::F1] {
        assert_eq!(ld.input(DecodedKey::RawKey(raw_key)).as_bytes(), b"");
    }
    // Non-ASCII, and control characters with no cooked meaning (Tab, ESC,
    // Ctrl-L).
    for c in ['é', '\t', '\u{1b}', '\u{c}'] {
        assert_eq!(ld.input(key(c)).as_bytes(), b"", "{:?}", c);
    }
    assert!(!ld.readable());
    type_str(&mut ld, "ok\n");
    assert_eq!(read_once(&mut ld, 64), b"ok\n");
}

#[test_case]
fn cooked_line_is_capped_at_line_max() {
    let mut ld = LineDiscipline::new();
    for i in 0..LINE_MAX + 20 {
        let echo = ld.input(key('a'));
        let expected: &[u8] = if i < LINE_MAX { b"a" } else { b"" };
        assert_eq!(echo.as_bytes(), expected, "key {}", i);
    }
    // Editing still works at the cap.
    ld.input(key(BACKSPACE));
    ld.input(key('b'));
    ld.input(key('\n'));

    let line = read_once(&mut ld, LINE_MAX + 64);
    assert_eq!(line.len(), LINE_MAX + 1);
    assert!(line[..LINE_MAX - 1].iter().all(|&b| b == b'a'));
    assert_eq!(&line[LINE_MAX - 1..], b"b\n");
}

#[test_case]
fn cooked_line_that_does_not_fit_is_dropped() {
    let mut ld = LineDiscipline::new();
    // Fill the readable buffer to exactly 2 free bytes: 4-byte lines, plus
    // one 2-byte line to make up the difference.
    let lines = (INPUT_CAPACITY - 4) / 4;
    for _ in 0..lines {
        type_str(&mut ld, "abc\n");
    }
    let filler = INPUT_CAPACITY - 2 - lines * 4;
    for _ in 0..filler - 1 {
        ld.input(key('q'));
    }
    ld.input(key('\n'));

    type_str(&mut ld, "zz");
    assert_eq!(ld.input(key('\n')).as_bytes(), b"\n", "Enter still echoes");

    // "zz\n" needs 3 bytes but only 2 are free: dropped as a whole.
    for _ in 0..lines {
        assert_eq!(read_once(&mut ld, 64), b"abc\n");
    }
    assert_eq!(read_once(&mut ld, 64).len(), filler);
    assert!(!ld.readable(), "no truncated \"zz\" line");
}

// ---------------------------------------------------------------------------
// Raw mode
// ---------------------------------------------------------------------------

#[test_case]
fn raw_delivers_bytes_immediately_without_echo() {
    let mut ld = raw();
    assert_eq!(ld.mode(), TtyMode::Raw);
    assert_eq!(type_str(&mut ld, "ab"), b"", "raw mode never echoes");
    assert!(ld.readable());
    assert_eq!(read_once(&mut ld, 64), b"ab", "everything available at once");
    assert!(!ld.readable());
}

#[test_case]
fn raw_passes_control_characters_through() {
    let mut ld = raw();
    for c in [BACKSPACE, '\n', CTRL_C, CTRL_D, '\u{1b}', '\t'] {
        assert_eq!(ld.input(key(c)).as_bytes(), b"");
    }
    assert_eq!(read_once(&mut ld, 64), b"\x08\n\x03\x04\x1b\t");
    // Raw Ctrl-D is a byte, not EOF.
    assert!(!ld.readable());
}

#[test_case]
fn raw_arrows_become_escape_sequences() {
    let mut ld = raw();
    for (code, letter) in [
        (KeyCode::ArrowUp, b'A'),
        (KeyCode::ArrowDown, b'B'),
        (KeyCode::ArrowRight, b'C'),
        (KeyCode::ArrowLeft, b'D'),
    ] {
        ld.input(DecodedKey::RawKey(code));
        assert_eq!(read_once(&mut ld, 64), [0x1b, b'[', letter]);
    }

    // A small buffer splits a sequence across reads.
    ld.input(DecodedKey::RawKey(KeyCode::ArrowUp));
    assert_eq!(read_once(&mut ld, 2), b"\x1b[");
    assert_eq!(read_once(&mut ld, 2), b"A");
}

#[test_case]
fn raw_ignores_unsupported_keys() {
    let mut ld = raw();
    ld.input(DecodedKey::RawKey(KeyCode::F1));
    ld.input(key('é'));
    assert!(!ld.readable());
}

#[test_case]
fn raw_input_is_capped_at_input_capacity() {
    let mut ld = raw();
    for _ in 0..INPUT_CAPACITY + 100 {
        ld.input(key('x'));
    }
    let mut total = 0;
    while ld.readable() {
        let chunk = read_once(&mut ld, 100);
        assert!(!chunk.is_empty());
        total += chunk.len();
    }
    assert_eq!(total, INPUT_CAPACITY);
}

// ---------------------------------------------------------------------------
// Mode switches
// ---------------------------------------------------------------------------

#[test_case]
fn switching_mode_discards_the_edit_line_but_keeps_readable_bytes() {
    let mut ld = LineDiscipline::new();
    type_str(&mut ld, "kept\nlost");
    ld.set_mode(TtyMode::Raw);
    type_str(&mut ld, "r");
    assert_eq!(read_once(&mut ld, 64), b"kept\nr");

    ld.set_mode(TtyMode::Cooked);
    type_str(&mut ld, "again\n");
    assert_eq!(read_once(&mut ld, 64), b"again\n");
}

#[test_case]
fn raw_bytes_are_readable_after_switching_to_cooked() {
    let mut ld = raw();
    type_str(&mut ld, "xy");
    ld.set_mode(TtyMode::Cooked);
    assert!(ld.readable());
    assert_eq!(read_once(&mut ld, 64), b"xy");
}

// ---------------------------------------------------------------------------
// Scancode decoding
// ---------------------------------------------------------------------------

/// Decoded keys for `scancodes`, without modifier key presses.
///
/// pc-keyboard reports pressing Shift/Ctrl itself as `RawKey(LShift)` etc.
/// The TTY ignores those (like every raw key except the arrows), so they're
/// filtered out here. `decoder_reports_modifier_presses_as_raw_keys` pins
/// that behaviour down.
fn feed_all(decoder: &mut KeyDecoder, scancodes: &[u8]) -> Vec<DecodedKey> {
    scancodes
        .iter()
        .filter_map(|&s| decoder.feed(s))
        .filter(|key| !is_modifier(key))
        .collect()
}

fn is_modifier(key: &DecodedKey) -> bool {
    matches!(
        key,
        DecodedKey::RawKey(
            KeyCode::LShift | KeyCode::RShift | KeyCode::LControl | KeyCode::RControl
        )
    )
}

#[test_case]
fn decoder_maps_scancodes_to_keys() {
    let mut d = KeyDecoder::new();
    // 0x1E = 'a' pressed, 0x9E = released.
    assert_eq!(feed_all(&mut d, &[0x1E, 0x9E]), [key('a')]);
    // Shift (0x2A) + a, then release shift (0xAA).
    assert_eq!(feed_all(&mut d, &[0x2A, 0x1E, 0x9E, 0xAA]), [key('A')]);
    // Enter (0x1C) and Backspace (0x0E).
    assert_eq!(feed_all(&mut d, &[0x1C, 0x9C, 0x0E, 0x8E]), [key('\n'), key(BACKSPACE)]);
    // Arrow up: prefix 0xE0 then 0x48.
    assert_eq!(
        feed_all(&mut d, &[0xE0, 0x48, 0xE0, 0xC8]),
        [DecodedKey::RawKey(KeyCode::ArrowUp)]
    );
}

#[test_case]
fn decoder_reports_modifier_presses_as_raw_keys() {
    let mut d = KeyDecoder::new();
    let keys: Vec<DecodedKey> = [0x1D, 0x2A].iter().filter_map(|&s| d.feed(s)).collect();
    assert_eq!(
        keys,
        [DecodedKey::RawKey(KeyCode::LControl), DecodedKey::RawKey(KeyCode::LShift)]
    );
    // And the TTY ignores them in both modes.
    let mut cooked = LineDiscipline::new();
    let mut raw_ld = raw();
    for key in keys {
        assert_eq!(cooked.input(key).as_bytes(), b"");
        assert_eq!(raw_ld.input(key).as_bytes(), b"");
    }
    assert!(!cooked.readable() && !raw_ld.readable());
}

#[test_case]
fn decoder_maps_ctrl_letters_to_control_characters() {
    let mut d = KeyDecoder::new();
    // Left Ctrl (0x1D) held: c (0x2E), d (0x20), l (0x26); then release (0x9D).
    let keys = feed_all(&mut d, &[0x1D, 0x2E, 0xAE, 0x20, 0xA0, 0x26, 0xA6, 0x9D]);
    assert_eq!(keys, [key(CTRL_C), key(CTRL_D), key('\u{c}')]);
    assert_eq!(feed_all(&mut d, &[0x2E, 0xAE]), [key('c')], "ctrl released");
}

// ---------------------------------------------------------------------------
// VGA control bytes and the hardware cursor
// ---------------------------------------------------------------------------

const ROWS: usize = 25;
const COLS: usize = 80;

fn cell(row: usize, col: usize) -> u8 {
    let screen = 0xb8000 as *const u8;
    unsafe { core::ptr::read_volatile(screen.add((row * COLS + col) * 2)) }
}

/// The first `len` characters of the row the writer is on.
fn current_row(w: &rust_os::vga_buffer::Writer, len: usize) -> Vec<u8> {
    let (row, _) = w.position();
    (0..len).map(|col| cell(row, col)).collect()
}

/// Run `f` with exclusive access to the VGA writer, starting from a fresh
/// line with default colors.
fn with_writer(f: impl FnOnce(&mut rust_os::vga_buffer::Writer)) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut writer = rust_os::vga_buffer::WRITER.lock();
        writer.write_string("\x1b[0m\n");
        f(&mut writer);
    });
}

#[test_case]
fn vga_carriage_return_goes_to_column_zero() {
    with_writer(|w| {
        w.write_string("abc\rX");
        assert_eq!(current_row(w, 3), b"Xbc");
    });
}

#[test_case]
fn vga_backspace_moves_left_without_erasing() {
    with_writer(|w| {
        w.write_string("ab\x08");
        assert_eq!(current_row(w, 2), b"ab", "backspace alone doesn't erase");
        w.write_string("c");
        assert_eq!(current_row(w, 2), b"ac");
        // The line editor's erase sequence.
        w.write_string("\x08 \x08");
        assert_eq!(current_row(w, 2), b"a ");
        // Backspace at column 0 is a no-op.
        w.write_string("\r\x08Z");
        assert_eq!(current_row(w, 1), b"Z");
    });
}

#[test_case]
fn vga_form_feed_clears_the_screen_and_homes() {
    with_writer(|w| {
        w.write_string("junk\nmore junk");
        w.write_string("\x0c");
        for row in 0..ROWS {
            for col in 0..COLS {
                assert_eq!(cell(row, col), b' ', "cell {},{} not cleared", row, col);
            }
        }
        assert_eq!(w.position(), (0, 0), "after a clear, text starts at the top");
        w.write_string("top\nnext");
        assert_eq!(cell(0, 0), b't');
        assert_eq!(cell(1, 0), b'n', "rows fill downward");
    });
}

#[test_case]
fn vga_scrolls_only_at_the_bottom() {
    with_writer(|w| {
        w.write_string("\x0c");
        for i in 0..ROWS {
            w.write_string(if i == 0 { "first" } else { "\nline" });
        }
        assert_eq!(w.position().0, ROWS - 1);
        assert_eq!(cell(0, 0), b'f', "a full screen hasn't scrolled yet");
        w.write_string("\nlast");
        assert_eq!(cell(0, 0), b'l', "one more line scrolls the first one away");
        assert_eq!(current_row(w, 4), b"last");
    });
}

#[test_case]
fn vga_hardware_cursor_follows_the_text() {
    use rust_os::vga_buffer::hardware_cursor;
    with_writer(|w| {
        let base = w.position().0 * COLS;
        assert_eq!(hardware_cursor(), base);
        w.write_string("hello");
        assert_eq!(hardware_cursor(), base + 5);
        w.write_string("\x08\x08");
        assert_eq!(hardware_cursor(), base + 3);
        w.write_string("\r");
        assert_eq!(hardware_cursor(), base);
        w.write_byte(b'x');
        assert_eq!(hardware_cursor(), base + 1, "write_byte moves it too");
    });
}

/// VGA color numbers (foreground, background).
const LIGHT_GRAY: u8 = 7;
const BLACK: u8 = 0;

#[test_case]
fn vga_ansi_colors() {
    with_writer(|w| {
        let row = w.position().0;
        w.write_string("a\x1b[31mb\x1b[1;34mc\x1b[0md\x1b[97;41me\x1b[39;49mf\x1b[92mg\x1b[mh");
        assert_eq!(current_row(w, 8), b"abcdefgh", "escape sequences print nothing");
        assert_eq!(w.color_at(row, 0), (LIGHT_GRAY, BLACK), "default colors");
        assert_eq!(w.color_at(row, 1), (4, BLACK), "31: red");
        assert_eq!(w.color_at(row, 2), (9, BLACK), "1;34: bold blue is light blue");
        assert_eq!(w.color_at(row, 3), (LIGHT_GRAY, BLACK), "0: reset");
        assert_eq!(w.color_at(row, 4), (15, 4), "97;41: white on red");
        assert_eq!(w.color_at(row, 5), (LIGHT_GRAY, BLACK), "39;49: default colors");
        assert_eq!(w.color_at(row, 6), (10, BLACK), "92: light green");
        assert_eq!(w.color_at(row, 7), (LIGHT_GRAY, BLACK), "ESC[m: reset");
    });
}

#[test_case]
fn vga_escape_split_across_writes() {
    with_writer(|w| {
        let row = w.position().0;
        for piece in ["x\x1b", "[", "3", "2", "m", "y\x1b[0m"] {
            w.write_string(piece);
        }
        assert_eq!(current_row(w, 2), b"xy");
        assert_eq!(w.color_at(row, 1), (2, BLACK), "32: green");
    });
}

#[test_case]
fn vga_erase_and_cursor_sequences() {
    with_writer(|w| {
        w.write_string("abcdef\r\x1b[2C\x1b[K");
        assert_eq!(current_row(w, 6), b"ab    ", "ESC[2C moves right, ESC[K erases the rest");
        w.write_string("\x1b[D!");
        assert_eq!(current_row(w, 2), b"a!", "ESC[D moves left");
        w.write_string("\x1b[2J");
        assert_eq!(w.position(), (0, 0), "ESC[2J clears and homes");
        w.write_string("\x1b[3;5HX");
        assert_eq!(cell(2, 4), b'X', "ESC[row;colH is 1-based");
        w.write_string("\x1b[?25l\x1b[Zok");
        assert_eq!(cell(2, 5), b'o', "unknown sequences are swallowed");
    });
}

#[test_case]
fn vga_utf8_maps_to_the_vga_font() {
    with_writer(|w| {
        // Box drawing, shades, an arrow, an accented letter, the middle dot.
        w.write_string("\u{250c}\u{2500}\u{2510}\u{2502}\u{2591}\u{2593}\u{2588}\u{25ba}\u{e9}\u{b7}");
        assert_eq!(
            current_row(w, 10),
            [0xda, 0xc4, 0xbf, 0xb3, 0xb0, 0xb2, 0xdb, 0x10, 0x82, 0xfa]
        );
    });
}

#[test_case]
fn vga_unknown_and_broken_utf8_show_a_block() {
    with_writer(|w| {
        // — (no VGA glyph), then a 3-byte character cut off by "c", then a
        // stray continuation byte (ignored) and a control byte (ignored).
        w.write_string("a\u{2014}b");
        for &byte in b"\xe2\x94c\x80\x01d" {
            w.write_byte(byte);
        }
        assert_eq!(current_row(w, 6), b"a\xfeb\xfecd");
    });
}

#[test_case]
fn vga_utf8_split_across_writes() {
    with_writer(|w| {
        let box_corner = "\u{2514}".as_bytes(); // └, 3 bytes
        for byte in box_corner {
            w.write_byte(*byte);
        }
        assert_eq!(current_row(w, 1), [0xc0]);
    });
}

/// Program output goes through the console file. A write may end in the
/// middle of a UTF-8 character (ulib's printf flushes every 256 bytes); the
/// next write completes it. This used to come out as "â" plus junk.
#[test_case]
fn console_write_keeps_a_character_split_across_writes() {
    let mut out = rust_os::fs::devices::stdout_file();
    let rule = "\u{2500}".as_bytes(); // ─, 3 bytes
    out.write(b"\nx").unwrap();
    out.write(&rule[..1]).unwrap();
    out.write(&rule[1..]).unwrap();
    out.write(b"y").unwrap();
    let writer = rust_os::vga_buffer::WRITER.lock();
    let (row, column) = writer.position();
    let shown: Vec<u8> = (column - 3..column).map(|c| writer.char_at(row, c)).collect();
    assert_eq!(shown, [b'x', 0xc4, b'y']);
}

#[test_case]
fn vga_reserved_top_rows_are_left_alone() {
    with_writer(|w| {
        w.write_string("\x0c");
        w.draw(0, 0, b"STATUS", rust_os::vga_buffer::Color::White, rust_os::vga_buffer::Color::Blue);
        w.set_scroll_top(1);
        assert_eq!(w.position(), (1, 0), "text starts below the reserved row");
        for _ in 0..ROWS + 3 {
            w.write_string("line\n");
        }
        assert_eq!(cell(0, 0), b'S', "scrolling leaves the reserved row");
        w.write_string("\x0c");
        assert_eq!(cell(0, 0), b'S', "clearing leaves the reserved row");
        assert_eq!(w.position(), (1, 0));
        w.write_string("\x1b[2J\x1b[H");
        assert_eq!(cell(0, 0), b'S');
        assert_eq!(w.position(), (1, 0), "ESC[H homes to the region's top");
        w.reset();
        assert_eq!(w.position(), (0, 0), "reset gives the whole screen back");
        assert_eq!(cell(0, 0), b' ');
    });
}

#[test_case]
fn vga_palette_can_be_replaced() {
    use rust_os::vga_buffer::{palette_entry, set_palette};
    let mut colors = [(0u8, 0u8, 0u8); 16];
    for (i, color) in colors.iter_mut().enumerate() {
        *color = ((i * 16) as u8, 255 - (i * 16) as u8, 0x40);
    }
    set_palette(&colors);
    for i in 0..16u8 {
        let (r, g, b) = colors[i as usize];
        // The DAC keeps 6 bits per channel.
        assert_eq!(palette_entry(i), (r & !3, g & !3, b & !3), "palette entry {}", i);
    }
    rust_os::theme::apply();
    // The terminal background (6, 10, 14), at the DAC's 6-bit precision.
    assert_eq!(palette_entry(0), (4, 8, 12), "the terminal background");
}

mod support;
