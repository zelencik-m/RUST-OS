//! The console's status bar: the top row, kept out of the text area
//! (`Writer::set_scroll_top`), redrawn twice a second by a kernel thread.
//!
//! ```text
//!  RUST-OS ▓▒░ /BIN/SH · TTY0                     PROCS 01 ░▒▓ UPTIME 00:01:23 ■
//! ```
//!
//! The two tags fade into the bar through shade characters (▓▒░ in the tag
//! color on the bar color), a gradient without spending palette slots.

use core::fmt::Write as _;

use crate::process::process_manager::PROCESS_MANAGER;
use crate::theme::{ACCENT, BG, BRIGHT, DIM, PANEL, PANEL_HIGH, TEXT};
use crate::vga_buffer::{LineBuffer, Writer, SCREEN_WIDTH, WRITER};

pub const ROW: usize = 0;

/// Ticks between redraws (~0.5 s).
const REFRESH_TICKS: u64 = 9;

/// ▓▒░ (dark to light shade).
const FADE: [u8; 3] = [0xb2, 0xb1, 0xb0];

/// Reserve the top row for the bar and draw it.
pub fn install() {
    x86_64::instructions::interrupts::without_interrupts(|| {
        WRITER.lock().set_scroll_top(ROW + 1);
    });
    refresh();
}

/// Draw the bar for this uptime and process count. `beat` toggles the
/// activity square. Pure drawing, so tests can call it.
pub fn draw(w: &mut Writer, uptime_millis: u64, processes: usize, beat: bool) {
    w.fill(ROW, 0, SCREEN_WIDTH, b' ', TEXT, PANEL);

    // Left: the name tag, fading into the bar.
    w.draw(ROW, 0, b" RUST-OS ", BG, ACCENT);
    w.draw(ROW, 9, &FADE, ACCENT, PANEL);
    w.draw(ROW, 13, b"/BIN/SH \xfa TTY0", TEXT, PANEL);

    // Right: the clock tag (on the lighter panel shade), fading in from the
    // bar, with the process count before it.
    let seconds = uptime_millis / 1000;
    let mut clock = LineBuffer::new();
    let _ = write!(
        clock,
        " UPTIME {:02}:{:02}:{:02}   ",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    );
    let clock_column = SCREEN_WIDTH - clock.as_bytes().len();
    w.draw(ROW, clock_column, clock.as_bytes(), TEXT, PANEL_HIGH);
    w.draw(ROW, SCREEN_WIDTH - 2, &[0xfe], if beat { BRIGHT } else { DIM }, PANEL_HIGH);
    let fade = [FADE[2], FADE[1], FADE[0]];
    w.draw(ROW, clock_column - 3, &fade, PANEL_HIGH, PANEL);

    let mut procs = LineBuffer::new();
    let _ = write!(procs, "PROCS {:02}", processes);
    w.draw(ROW, clock_column - 4 - procs.as_bytes().len(), procs.as_bytes(), TEXT, PANEL);
}

/// Redraw with the current uptime and process count.
pub fn refresh() {
    let processes = PROCESS_MANAGER.lock().live_user_process_count();
    let uptime = crate::debug::uptime_millis();
    let beat = (uptime / 500) % 2 == 0;
    x86_64::instructions::interrupts::without_interrupts(|| {
        draw(&mut WRITER.lock(), uptime, processes, beat);
    });
}

/// Kernel thread: keep the bar up to date.
pub extern "C" fn run() -> ! {
    loop {
        refresh();
        crate::scheduler::scheduler::sleep(REFRESH_TICKS);
    }
}
