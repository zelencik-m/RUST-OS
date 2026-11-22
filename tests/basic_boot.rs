#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(support::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;
use rust_os::println;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    test_main();

    loop {}
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    support::test_panic_handler(info)
}

#[test_case]
fn test_println() {
    println!("test_println output");
}

#[test_case]
fn test_vga_buffer_content() {
    const MARKER: &[u8] = b"vga-buffer-test-marker";

    // Write the marker on a fresh line and read it back from the row the
    // writer is on, while holding the writer lock so other output can't
    // interfere.
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut writer = rust_os::vga_buffer::WRITER.lock();
        writer.write_string("\n");
        let (row, _) = writer.position();
        writer.write_string("vga-buffer-test-marker");
        for (column, expected) in MARKER.iter().enumerate() {
            assert_eq!(
                writer.char_at(row, column),
                *expected,
                "VGA character mismatch at column {}",
                column
            );
        }
    });
}

#[test_case]
fn debug_logging_is_disabled_in_tests() {
    assert!(!rust_os::debug::is_enabled());
    assert_eq!(rust_os::debug::level(), None);
}

#[test_case]
fn log_levels_filter_by_verbosity() {
    use rust_os::debug::{enabled, set_level, Level};
    set_level(Some(Level::Info));
    let checks = [
        enabled(Level::Error),
        enabled(Level::Warn),
        enabled(Level::Info),
        !enabled(Level::Debug),
        !enabled(Level::Trace),
    ];
    set_level(Some(Level::Trace));
    let all = enabled(Level::Trace);
    set_level(None);
    assert!(checks.iter().all(|&ok| ok), "Info enables Error..Info only: {:?}", checks);
    assert!(all, "Trace enables everything");
    assert!(!enabled(Level::Error), "None disables everything");
}

/// Whether the screen shows `expected` at `row`, `column`.
fn screen_has_at(row: usize, column: usize, expected: &[u8]) -> bool {
    let writer = rust_os::vga_buffer::WRITER.lock();
    expected
        .iter()
        .enumerate()
        .all(|(i, &byte)| writer.char_at(row, column + i) == byte)
}

/// Whether `expected` appears anywhere on the screen.
fn screen_contains(expected: &[u8]) -> bool {
    (0..25).any(|row| (0..=80 - expected.len()).any(|column| screen_has_at(row, column, expected)))
}

/// The boot screen draws with fixed cells; its frames can be drawn without
/// the timer.
#[test_case]
fn boot_screen_shows_steps_and_the_final_frame() {
    use rust_os::boot_screen::{self, Frame, LOGO_FRAMES};
    boot_screen::begin(4);
    boot_screen::step("First step", format_args!("detail {}", 42));
    boot_screen::step("Second step", format_args!("more"));

    assert!(screen_contains(b"HOBBY OPERATING SYSTEM"), "the tagline");
    // Step lines from row 14: a marker, then upper-case name and detail.
    assert!(screen_has_at(14, 10, &[0xfe]), "the line marker (■)");
    assert!(screen_has_at(14, 13, b"FIRST STEP"));
    assert!(screen_has_at(14, 33, b"DETAIL 42"));
    assert!(screen_has_at(15, 13, b"SECOND STEP"));
    {
        let writer = rust_os::vga_buffer::WRITER.lock();
        let (older, _) = writer.color_at(14, 13);
        let (newer, _) = writer.color_at(15, 13);
        assert!(newer > older, "older lines fade (gradient slots {} vs {})", older, newer);
    }

    // A slow step shows a spinner until it's finished.
    boot_screen::start_step("Slow step", 2);
    assert!(screen_has_at(16, 13, b"SLOW STEP"));
    assert!(screen_has_at(16, 33, b"WORKING"), "a running step says WORKING");
    boot_screen::finish_step(format_args!("took a while"));
    assert!(screen_has_at(16, 33, b"TOOK A WHILE"), "finished: its detail");
    assert!(screen_has_at(16, 10, &[0xfe]), "finished: the ■ marker");

    let frame = |logo, percent, ready| Frame { logo, phase: logo, percent, ready };
    boot_screen::draw_frame(&frame(0, 0, false));
    assert!(screen_contains(b"STARTING /BIN/SH"));
    assert!(screen_contains(b"  0%"), "the bar starts empty");
    assert!(!screen_contains(&[0xdb; 4]), "the logo hasn't appeared yet");

    boot_screen::draw_frame(&frame(LOGO_FRAMES / 2, 42, false));
    assert!(screen_contains(b" 42%"), "the bar shows the progress it's given");

    boot_screen::draw_frame(&frame(LOGO_FRAMES, 100, true));
    assert!(screen_contains(b"100%"), "the progress bar reaches 100%");
    assert!(screen_contains(b"SYSTEM READY"), "the last step line");
    // The top row of the logo starts with R's six blocks, T has eight.
    assert!(screen_contains(&[0xdb; 8]), "the logo is fully drawn");

    boot_screen::finish();
    let writer = rust_os::vga_buffer::WRITER.lock();
    assert_eq!(writer.position(), (0, 0), "finish leaves a clear console");
    assert_eq!(writer.char_at(14, 13), b' ');
}

#[test_case]
fn logo_fits_on_the_screen() {
    assert!(rust_os::boot_screen::LOGO_WIDTH <= 78);
}

#[test_case]
fn status_bar_shows_uptime_and_processes() {
    {
        let mut writer = rust_os::vga_buffer::WRITER.lock();
        rust_os::status_bar::draw(&mut writer, 3_723_000, 2, true);
    }
    assert!(screen_has_at(0, 1, b"RUST-OS"));
    assert!(screen_contains(b"PROCS 02"));
    assert!(screen_contains(b"UPTIME 01:02:03"), "3723 s is 1 h 2 min 3 s");
}

mod support;
