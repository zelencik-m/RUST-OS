//! The boot screen, drawn with the [`theme::BOOT`] gradient palette:
//!
//! ```text
//!   · ∙ • ∙ · ·   ∙ • • ∙ ·     (an animated dot-matrix wave field)
//!   ██████╗ ██╗   ██╗███████╗████████╗       ██████╗ ███████╗
//!   ██╔══██╗██║   ██║██╔════╝╚══██╔══╝      ██╔═══██╗██╔════╝   (the logo:
//!   ██████╔╝██║   ██║███████╗   ██║   █████╗██║   ██║███████╗    letters in a
//!   ...                                                          vertical
//!                                                                gradient, a
//!                                                                dark shadow)
//!          X86_64 · HOBBY OPERATING SYSTEM · WRITTEN IN RUST
//!
//!          ■  CPU TABLES         GDT, TSS WITH FAULT STACKS, IDT
//!          ■  INTERRUPTS         8259 PIC: TIMER AND KEYBOARD IRQS
//!          ...                   (older lines fade)
//!   STARTING /BIN/SH  ████████████████▓··························  042%
//! ```
//!
//! [`begin`] loads the palette and draws the static parts; [`step`] adds a
//! line as each initialization step finishes (instantly: it takes
//! milliseconds). [`animate`] then plays ~3 s: the wave field moves, the
//! logo sweeps in and a shimmer runs across it, and the bar fills.
//! [`finish`] clears the screen.
//!
//! Everything is drawn at fixed cells with [`Writer::draw`] (raw CP437), so
//! nothing scrolls. [`step`] works before the heap exists. Only
//! `src/main.rs` uses this; test kernels boot silently.

use core::fmt::{self, Write as _};

use spin::Mutex;

use crate::theme::{self, shade, BOOT_AMBER, BOOT_GLOW};
use crate::vga_buffer::{Color, LineBuffer, Writer, SCREEN_HEIGHT, SCREEN_WIDTH, WRITER};

// Layout.
const FIELD_TOP: usize = 1;
const FIELD_BOTTOM: usize = 12; // exclusive
const LOGO_ROW: usize = 3;
const LOGO_HEIGHT: usize = 6;
const TAGLINE_ROW: usize = 12;
const STEPS_ROW: usize = 14;
/// Step lines, the last one being "SYSTEM READY".
const STEP_LINES: usize = 9;
/// The bottom band (rows from here on) has a lighter background.
const BAND_ROW: usize = 23;
const BAR_ROW: usize = 24;

const MARK_COLUMN: usize = 10;
const NAME_COLUMN: usize = 13;
const DETAIL_COLUMN: usize = 33;
const DETAIL_WIDTH: usize = SCREEN_WIDTH - DETAIL_COLUMN - 2;

/// Frames of the animation.
pub const ANIMATION_FRAMES: usize = 54;
/// How long the animation runs, in timer ticks (~55 ms each): about 3 s.
const ANIMATION_TICKS: u64 = 54;
/// Ticks the finished screen stays up before the shell starts.
const HOLD_TICKS: u64 = 8;

// CP437 characters.
const FULL: u8 = 0xdb;
const DARK: u8 = 0xb2;
const MEDIUM: u8 = 0xb1;
const LIGHT: u8 = 0xb0;
const DOT: u8 = 0xfa; // ·
const SMALL_BULLET: u8 = 0xf9; // ∙
const BULLET: u8 = 0x07; // •
const SQUARE: u8 = 0xfe; // ■
const ARROW: u8 = 0x10; // ►

// ---------------------------------------------------------------------------
// The logo: "RUST-OS" in the "ANSI Shadow" figlet font. █ is the letter, the
// box-drawing lines (╔═╗║╚╝) are its drop shadow. The VGA font has all of
// them; they're converted with `vga_buffer::cp437`.
// ---------------------------------------------------------------------------

type Glyph = [&'static str; LOGO_HEIGHT];

const R: Glyph = [
    "██████╗ ",
    "██╔══██╗",
    "██████╔╝",
    "██╔══██╗",
    "██║  ██║",
    "╚═╝  ╚═╝",
];
const U: Glyph = [
    "██╗   ██╗",
    "██║   ██║",
    "██║   ██║",
    "██║   ██║",
    "╚██████╔╝",
    " ╚═════╝ ",
];
const S: Glyph = [
    "███████╗",
    "██╔════╝",
    "███████╗",
    "╚════██║",
    "███████║",
    "╚══════╝",
];
const T: Glyph = [
    "████████╗",
    "╚══██╔══╝",
    "   ██║   ",
    "   ██║   ",
    "   ██║   ",
    "   ╚═╝   ",
];
const DASH: Glyph = [
    "      ",
    "      ",
    "█████╗",
    "╚════╝",
    "      ",
    "      ",
];
const O: Glyph = [
    " ██████╗ ",
    "██╔═══██╗",
    "██║   ██║",
    "██║   ██║",
    "╚██████╔╝",
    " ╚═════╝ ",
];

const LOGO: [&Glyph; 7] = [&R, &U, &S, &T, &DASH, &O, &S];

/// Characters (columns) in `text`: UTF-8 bytes that don't start with 0b10.
const fn columns(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut count = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] & 0xc0 != 0x80 {
            count += 1;
        }
        i += 1;
    }
    count
}

// Every row of a glyph must be equally wide, or the logo shears.
const _: () = {
    let mut i = 0;
    while i < LOGO.len() {
        let mut row = 1;
        while row < LOGO_HEIGHT {
            assert!(columns(LOGO[i][row]) == columns(LOGO[i][0]));
            row += 1;
        }
        i += 1;
    }
};

/// Width of the logo in cells.
pub const LOGO_WIDTH: usize = {
    let mut width = 0;
    let mut i = 0;
    while i < LOGO.len() {
        width += columns(LOGO[i][0]);
        i += 1;
    }
    width
};
const LOGO_COLUMN: usize = (SCREEN_WIDTH - LOGO_WIDTH) / 2;

/// Gradient step of the letters on each logo row: light at the top, teal
/// at the bottom.
const LOGO_SHADES: [i32; LOGO_HEIGHT] = [13, 13, 12, 11, 10, 9];
/// Gradient step of the shadow lines.
const SHADOW_SHADE: i32 = 5;

/// What the logo has at a cell.
enum LogoCell {
    /// Part of a letter (█).
    Letter,
    /// Part of the shadow: this CP437 character.
    Shadow(u8),
}

/// The logo at cell (`row`, `column`), relative to its top-left. `None`
/// for blank cells and anything outside the logo.
fn logo_cell(row: isize, column: isize) -> Option<LogoCell> {
    if row < 0 || column < 0 || row >= LOGO_HEIGHT as isize {
        return None;
    }
    let (row, column) = (row as usize, column as usize);
    let mut left = 0;
    for glyph in LOGO.iter() {
        let width = columns(glyph[0]);
        if column < left + width {
            return match glyph[row].chars().nth(column - left)? {
                ' ' => None,
                '█' => Some(LogoCell::Letter),
                c => Some(LogoCell::Shadow(crate::vga_buffer::cp437(c).unwrap_or(b'?'))),
            };
        }
        left += width;
    }
    None
}

// ---------------------------------------------------------------------------
// The wave field: dots whose size and brightness follow a moving wave.
// ---------------------------------------------------------------------------

/// One period of a sine, 64 steps, amplitude 64.
const SINE: [i32; 64] = [
    0, 6, 12, 19, 24, 30, 36, 41, 45, 49, 53, 56, 59, 61, 63, 64, 64, 64, 63, 61, 59, 56, 53, 49,
    45, 41, 36, 30, 24, 19, 12, 6, 0, -6, -12, -19, -24, -30, -36, -41, -45, -49, -53, -56, -59,
    -61, -63, -64, -64, -64, -63, -61, -59, -56, -53, -49, -45, -41, -36, -30, -24, -19, -12, -6,
];

fn sine(x: usize) -> i32 {
    SINE[x % 64]
}

/// Height of the field at a cell in `frame`, about -128..=128. Three waves
/// moving in different directions.
fn field_height(row: usize, column: usize, frame: usize) -> i32 {
    sine(column * 2 + frame * 3)
        + sine(row * 9 + column + 64 * 8 - (frame * 2) % 64) / 2
        + sine(column * 5 / 2 + row * 4 + frame) / 2
}

/// The field's character and color at a cell: brighter, bigger dots on the
/// crests, nothing in the troughs, only on even columns (a dot matrix).
fn field_cell(row: usize, column: usize, frame: usize) -> (u8, Color) {
    if column % 2 == 1 {
        return (b' ', shade(0));
    }
    let height = field_height(row, column, frame);
    // A little brighter towards the middle rows of the field.
    let middle = (FIELD_TOP + FIELD_BOTTOM) as i32 / 2;
    let lift = 2 - (row as i32 - middle).abs() / 3;
    match height {
        h if h < -40 => (b' ', shade(0)),
        h if h < 0 => (DOT, shade(1 + lift)),
        h if h < 50 => (DOT, shade(2 + lift)),
        h if h < 90 => (SMALL_BULLET, shade(3 + lift)),
        _ => (BULLET, shade(4 + lift)),
    }
}

// ---------------------------------------------------------------------------
// The step log
// ---------------------------------------------------------------------------

/// Steps reported so far (static: [`step`] runs before the heap exists).
struct StepLog {
    lines: [(LineBuffer, LineBuffer); STEP_LINES],
    count: usize,
}

static STEPS: Mutex<StepLog> = Mutex::new(StepLog {
    lines: [const { (LineBuffer::new(), LineBuffer::new()) }; STEP_LINES],
    count: 0,
});

fn with_writer<R>(f: impl FnOnce(&mut Writer) -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(|| f(&mut WRITER.lock()))
}

/// Background of a row: the bottom band is a step lighter.
fn background(row: usize) -> Color {
    if row >= BAND_ROW {
        shade(1)
    } else {
        shade(0)
    }
}

/// Draw `text` at a cell on the row's background.
fn text(w: &mut Writer, row: usize, column: usize, text: &[u8], fg: Color) {
    w.draw(row, column, text, fg, background(row));
}

/// Draw the step log: newest line brightest, older ones fading.
fn draw_steps(w: &mut Writer, log: &StepLog) {
    for (i, (name, detail)) in log.lines.iter().enumerate().take(log.count) {
        let row = STEPS_ROW + i;
        let age = (log.count - 1 - i) as i32;
        let level = 13 - age.min(6);
        w.fill(row, 0, SCREEN_WIDTH, b' ', shade(0), background(row));
        text(w, row, MARK_COLUMN, &[SQUARE], shade(level - 3));
        text(w, row, NAME_COLUMN, name.as_bytes(), shade(level));
        let detail = &detail.as_bytes()[..detail.as_bytes().len().min(DETAIL_WIDTH)];
        text(w, row, DETAIL_COLUMN, detail, shade(level - 3));
    }
}

/// Load the boot palette, clear the screen, hide the cursor, and draw the
/// static parts: the background, the tagline, the empty bar.
pub fn begin() {
    theme::load(&theme::BOOT, theme::FULL);
    STEPS.lock().count = 0;
    with_writer(|w| {
        w.reset();
        w.set_cursor_visible(false);
        for row in 0..SCREEN_HEIGHT {
            w.fill(row, 0, SCREEN_WIDTH, b' ', shade(0), background(row));
        }
        draw_tagline(w);
    });
    draw_frame(0, ANIMATION_FRAMES);
}

/// "X86_64 · HOBBY OPERATING SYSTEM · WRITTEN IN RUST", brightest in the
/// middle and darker towards both ends.
fn draw_tagline(w: &mut Writer) {
    let tagline = b"X86_64  \xfa  HOBBY OPERATING SYSTEM  \xfa  WRITTEN IN RUST";
    let column = (SCREEN_WIDTH - tagline.len()) / 2;
    let half = tagline.len() as i32 / 2;
    for (i, &byte) in tagline.iter().enumerate() {
        let distance = (i as i32 - half).abs();
        text(w, TAGLINE_ROW, column + i, &[byte], shade(12 - distance * 6 / half.max(1)));
    }
}

/// Report a finished initialization step: a new line in the step log (in
/// upper case), and an info log message.
pub fn step(name: &str, detail: fmt::Arguments) {
    crate::log_info!("{}: {}", name, detail);
    let mut log = STEPS.lock();
    if log.count >= STEP_LINES - 1 {
        return; // out of room (the last line is for "ready"); the log has it
    }
    let index = log.count;
    let (name_text, detail_text) = &mut log.lines[index];
    *name_text = LineBuffer::new();
    *detail_text = LineBuffer::new();
    let _ = name_text.write_str(name);
    name_text.make_ascii_uppercase();
    let _ = detail_text.write_fmt(detail);
    detail_text.make_ascii_uppercase();
    log.count += 1;
    with_writer(|w| draw_steps(w, &log));
}

/// Draw animation frame `frame` of `total` (`frame == total` is the
/// finished picture). Pure drawing, no waiting, so tests can call it.
///
/// - The wave field moves every frame.
/// - Frames 10–55%: the logo sweeps in from the left, with a glow-white
///   leading edge.
/// - Frames 60–95%: a bright diagonal shimmer runs across the logo.
/// - The bar fills over the whole animation; the last frame adds the
///   "SYSTEM READY" line.
pub fn draw_frame(frame: usize, total: usize) {
    let total = total.max(1);
    let frame = frame.min(total);
    let sweep_start = total / 10;
    let sweep_end = total * 55 / 100;
    let revealed = if frame >= sweep_end {
        LOGO_WIDTH + 4
    } else if frame <= sweep_start {
        0
    } else {
        (LOGO_WIDTH + 4) * (frame - sweep_start) / (sweep_end - sweep_start)
    };
    let shimmer_start = total * 60 / 100;
    let shimmer_end = total * 95 / 100;
    let shimmer = if frame >= shimmer_start && frame < shimmer_end {
        let span = LOGO_WIDTH as i32 + 30;
        Some((frame - shimmer_start) as i32 * span / (shimmer_end - shimmer_start) as i32 - 15)
    } else {
        None
    };

    with_writer(|w| {
        for row in FIELD_TOP..FIELD_BOTTOM {
            for column in 0..SCREEN_WIDTH {
                let logo_row = row as isize - LOGO_ROW as isize;
                let logo_column = column as isize - LOGO_COLUMN as isize;
                let shown = logo_column >= 0 && (logo_column as usize) < revealed;
                let logo = if shown { logo_cell(logo_row, logo_column) } else { None };
                let in_shimmer = shimmer.map_or(false, |position| {
                    let diagonal = logo_column as i32 + logo_row as i32 * 2 - position;
                    (0..4).contains(&diagonal)
                });
                let (cell, color) = match logo {
                    Some(LogoCell::Letter) => {
                        let edge = revealed <= LOGO_WIDTH && logo_column as usize + 3 >= revealed;
                        if edge || in_shimmer {
                            (FULL, BOOT_GLOW)
                        } else {
                            (FULL, shade(LOGO_SHADES[logo_row as usize]))
                        }
                    }
                    // The shimmer catches the shadow a little too.
                    Some(LogoCell::Shadow(byte)) if in_shimmer => (byte, shade(SHADOW_SHADE + 4)),
                    Some(LogoCell::Shadow(byte)) => (byte, shade(SHADOW_SHADE)),
                    None => field_cell(row, column, frame),
                };
                text(w, row, column, &[cell], color);
            }
        }

        draw_bar(w, frame, total);

        if frame == total {
            let mut log = STEPS.lock();
            let line = log.count.min(STEP_LINES - 1);
            let (name, detail) = &mut log.lines[line];
            *name = LineBuffer::new();
            *detail = LineBuffer::new();
            let _ = name.write_str("SYSTEM READY");
            let _ = detail.write_str("HANDING OVER TO /BIN/SH");
            log.count = line + 1;
            draw_steps(w, &log);
            let row = STEPS_ROW + line;
            text(w, row, MARK_COLUMN, &[ARROW], BOOT_AMBER);
            text(w, row, NAME_COLUMN, b"SYSTEM READY", BOOT_GLOW);
        }
    });
}

/// A smooth gradient across `width` cells from shade `from` to shade `to`:
/// cell `i`'s character and colors. Between two shades the cell blends
/// them with ░▒▓ (the next shade drawn over the previous one), so each
/// color step gets 4 sub-steps.
fn gradient_cell(i: usize, width: usize, from: i32, to: i32) -> (u8, Color, Color) {
    let steps = ((to - from) * 4).max(1) as usize;
    let position = i * steps / width.max(1);
    let base = from + (position / 4) as i32;
    match position % 4 {
        0 => (FULL, shade(base), shade(base)),
        1 => (LIGHT, shade(base + 1), shade(base)),
        2 => (MEDIUM, shade(base + 1), shade(base)),
        _ => (DARK, shade(base + 1), shade(base)),
    }
}

/// The bottom bar: label, a gradient-filled bar with a glowing head, and
/// the percentage.
fn draw_bar(w: &mut Writer, frame: usize, total: usize) {
    let label = b"STARTING /BIN/SH";
    let bar_column = 3 + label.len() + 3;
    let bar_width = SCREEN_WIDTH - bar_column - 8;
    let filled = bar_width * frame / total;
    text(w, BAR_ROW, 3, label, shade(10));
    let spinner = if frame == total {
        SQUARE
    } else {
        [b'|', b'/', b'-', b'\\'][frame % 4]
    };
    text(w, BAR_ROW, 3 + label.len() + 1, &[spinner], BOOT_AMBER);
    for i in 0..bar_width {
        let (cell, fg, bg) = if i + 1 == filled && frame < total {
            (FULL, BOOT_GLOW, background(BAR_ROW))
        } else if i < filled {
            gradient_cell(i, bar_width, 3, 13)
        } else {
            (DOT, shade(3), background(BAR_ROW))
        };
        w.draw(BAR_ROW, bar_column + i, &[cell], fg, bg);
    }
    let mut percent = LineBuffer::new();
    let _ = write!(percent, "{:3}%", frame * 100 / total);
    text(w, BAR_ROW, bar_column + bar_width + 2, percent.as_bytes(), shade(13));
}

/// Play the animation and hold the final picture (~3.4 s). Needs a running
/// scheduler: call it from a kernel thread with interrupts enabled.
///
/// The frame shown is picked from the elapsed time, not counted: a sleeping
/// thread only wakes at the next scheduling point, so a frame per
/// `sleep(1)` would stretch the animation. A late wakeup skips frames.
pub fn animate() {
    use crate::scheduler::scheduler::{sleep, SCHEDULER};
    let now = || SCHEDULER.lock().tick_count();
    let start = now();
    loop {
        let elapsed = now().wrapping_sub(start);
        let frame = (elapsed as usize * ANIMATION_FRAMES / ANIMATION_TICKS as usize)
            .min(ANIMATION_FRAMES);
        draw_frame(frame, ANIMATION_FRAMES);
        if frame == ANIMATION_FRAMES {
            break;
        }
        sleep(1);
    }
    sleep(HOLD_TICKS);
}

/// Clear the boot screen: an empty console with the cursor at the top. The
/// caller then loads the terminal palette.
pub fn finish() {
    with_writer(|w| w.reset());
}
