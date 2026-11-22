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
//!          /  ROOT FILESYSTEM    WORKING...       (a step in progress)
//!   STARTING /BIN/SH  ████████████████▓··························  042%
//! ```
//!
//! Boot is split across two kernel threads (see `src/main.rs`): one does
//! the slow loading (mounting the disk, installing programs) and reports
//! it, the other runs [`play`], which keeps animating meanwhile.
//!
//! - [`begin`] loads the palette and draws the static parts.
//! - [`step`] adds a finished step (the fast ones in early boot);
//!   [`start_step`] adds a running one (spinner), and [`finish_step`] or
//!   [`fail_step`] completes it. [`loading_done`] says everything is loaded.
//! - [`play`] animates: the wave field moves, the logo sweeps in and a
//!   shimmer runs across it, and the bar follows the steps' progress. It
//!   returns once loading is done and the logo sequence has played.
//! - [`finish`] clears the screen.
//!
//! Everything is drawn at fixed cells with [`Writer::draw`] (raw CP437), so
//! nothing scrolls. [`step`] works before the heap exists. Only
//! `src/main.rs` uses this; test kernels boot silently.

use core::fmt::{self, Write as _};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::sync::IrqSafeMutex;

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

/// Frames of the logo's sequence (sweep-in, then shimmer), one per timer
/// tick (~55 ms): about 3 s.
pub const LOGO_FRAMES: usize = 54;
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

/// The logo decoded into cells, once: 0 = blank, 1 = letter, anything
/// else = that shadow character (CP437). `logo_cell` runs for every logo
/// cell on every frame, and walking the UTF-8 glyph strings each time was
/// slow enough to take CPU time away from the boot's disk work.
static LOGO_CELLS: spin::Once<[[u8; LOGO_WIDTH]; LOGO_HEIGHT]> = spin::Once::new();

const BLANK_CELL: u8 = 0;
const LETTER_CELL: u8 = 1;

fn logo_cells() -> &'static [[u8; LOGO_WIDTH]; LOGO_HEIGHT] {
    LOGO_CELLS.call_once(|| {
        let mut cells = [[BLANK_CELL; LOGO_WIDTH]; LOGO_HEIGHT];
        for (row, cells_row) in cells.iter_mut().enumerate() {
            let mut column = 0;
            for glyph in LOGO.iter() {
                for c in glyph[row].chars() {
                    cells_row[column] = match c {
                        ' ' => BLANK_CELL,
                        '█' => LETTER_CELL,
                        c => crate::vga_buffer::cp437(c).unwrap_or(b'?'),
                    };
                    column += 1;
                }
            }
        }
        cells
    })
}

/// The logo at cell (`row`, `column`), relative to its top-left. `None`
/// for blank cells and anything outside the logo.
fn logo_cell(row: isize, column: isize) -> Option<LogoCell> {
    if row < 0 || column < 0 || row >= LOGO_HEIGHT as isize || column >= LOGO_WIDTH as isize {
        return None;
    }
    match logo_cells()[row as usize][column as usize] {
        BLANK_CELL => None,
        LETTER_CELL => Some(LogoCell::Letter),
        byte => Some(LogoCell::Shadow(byte)),
    }
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
// The step log and the progress bar
// ---------------------------------------------------------------------------

/// What a step line shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepState {
    /// Finished: ■ and its detail.
    Done,
    /// In progress: a spinner and "WORKING...".
    Running,
    /// Failed: X and the error, in amber.
    Failed,
    /// The last line: ► SYSTEM READY.
    Ready,
}

struct StepLine {
    name: LineBuffer,
    detail: LineBuffer,
    state: StepState,
}

/// The step lines and the progress they add up to. Static, because
/// [`step`] runs before the heap exists.
///
/// An `IrqSafeMutex`, not a spin lock: the loader thread updates it while
/// the animation thread draws it with interrupts off (inside the VGA
/// writer's lock). If the loader could be preempted while holding a plain
/// spin lock, the animation thread would spin on it forever.
struct StepLog {
    lines: [StepLine; STEP_LINES],
    count: usize,
    /// The bar measures work, not lines: each step has a weight (fast
    /// steps 1, slow ones more, see [`start_step`]). `total` is the sum of
    /// all weights expected ([`begin`]), `done` of the finished ones.
    total: usize,
    done: usize,
    /// The running step's weight, and the tick it started at (the bar
    /// creeps through its share meanwhile).
    running_weight: usize,
    running_since: u64,
}

static STEPS: IrqSafeMutex<StepLog> = IrqSafeMutex::new(StepLog {
    lines: [const {
        StepLine {
            name: LineBuffer::new(),
            detail: LineBuffer::new(),
            state: StepState::Done,
        }
    }; STEP_LINES],
    count: 0,
    total: 1,
    done: 0,
    running_weight: 0,
    running_since: 0,
});

/// Set by [`loading_done`]: everything is loaded, the bar may reach 100%.
static LOADED: AtomicBool = AtomicBool::new(false);

/// While a step runs, the bar creeps through a tenth of its weight every
/// this many ticks, up to nine tenths, so a slow step doesn't look stuck.
const CREEP_TICKS: u64 = 3;

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

/// What the boot screen last wrote to each cell (`encode`d), so a frame
/// only writes the cells that changed. Under emulation every write to VGA
/// memory is expensive, and redrawing all ~1700 animated cells on every
/// tick took CPU time away from the disk work the animation runs beside.
/// `UNKNOWN` forces a write (after `begin` cleared the screen).
static SHOWN: IrqSafeMutex<[[u32; SCREEN_WIDTH]; SCREEN_HEIGHT]> =
    IrqSafeMutex::new([[UNKNOWN; SCREEN_WIDTH]; SCREEN_HEIGHT]);
const UNKNOWN: u32 = u32::MAX;

fn encode(byte: u8, fg: Color, bg: Color) -> u32 {
    byte as u32 | (fg as u32) << 8 | (bg as u32) << 12
}

/// Draw `bytes` from a cell on, skipping cells that already show the same.
fn put(w: &mut Writer, row: usize, column: usize, bytes: &[u8], fg: Color, bg: Color) {
    let mut shown = SHOWN.lock();
    for (i, &byte) in bytes.iter().enumerate() {
        let col = column + i;
        if col >= SCREEN_WIDTH {
            break;
        }
        let cell = encode(byte, fg, bg);
        if shown[row][col] != cell {
            shown[row][col] = cell;
            w.draw(row, col, &[byte], fg, bg);
        }
    }
}

/// Draw `text` at a cell on the row's background.
fn text(w: &mut Writer, row: usize, column: usize, text: &[u8], fg: Color) {
    put(w, row, column, text, fg, background(row));
}

/// One screen row being composed in memory, then written with `put` (so
/// cells that didn't change aren't touched).
struct RowBuffer {
    cells: [(u8, Color); SCREEN_WIDTH],
}

impl RowBuffer {
    fn blank() -> Self {
        RowBuffer {
            cells: [(b' ', shade(0)); SCREEN_WIDTH],
        }
    }

    fn text(&mut self, column: usize, text: &[u8], fg: Color) {
        for (i, &byte) in text.iter().enumerate() {
            if let Some(cell) = self.cells.get_mut(column + i) {
                *cell = (byte, fg);
            }
        }
    }

    fn flush(&self, w: &mut Writer, row: usize) {
        for (column, &(byte, fg)) in self.cells.iter().enumerate() {
            put(w, row, column, &[byte], fg, background(row));
        }
    }
}

const SPINNER: [u8; 4] = [b'|', b'/', b'-', b'\\'];

/// The spinners' character for `phase` (ticks): a step every two ticks,
/// about 2.3 turns a second. Frames come every tick (~18 a second); one
/// spinner step per frame looked frantic.
fn spinner(phase: usize) -> u8 {
    SPINNER[(phase / 2) % 4]
}

/// Draw the step lines: newest brightest, older ones fading. A running step
/// gets a spinner (`phase` animates it).
fn draw_steps(w: &mut Writer, log: &StepLog, phase: usize) {
    for (i, line) in log.lines.iter().enumerate().take(log.count) {
        let age = (log.count - 1 - i) as i32;
        let level = 13 - age.min(6);
        let detail = &line.detail.as_bytes()[..line.detail.as_bytes().len().min(DETAIL_WIDTH)];
        let mut row = RowBuffer::blank();
        match line.state {
            StepState::Done => {
                row.text(MARK_COLUMN, &[SQUARE], shade(level - 3));
                row.text(NAME_COLUMN, line.name.as_bytes(), shade(level));
                row.text(DETAIL_COLUMN, detail, shade(level - 3));
            }
            StepState::Running => {
                row.text(MARK_COLUMN, &[spinner(phase)], BOOT_AMBER);
                row.text(NAME_COLUMN, line.name.as_bytes(), BOOT_GLOW);
                let dots = &b"..."[..(phase / 4) % 4];
                row.text(DETAIL_COLUMN, b"WORKING", BOOT_AMBER);
                row.text(DETAIL_COLUMN + 7, dots, BOOT_AMBER);
            }
            StepState::Failed => {
                row.text(MARK_COLUMN, b"X", BOOT_AMBER);
                row.text(NAME_COLUMN, line.name.as_bytes(), BOOT_AMBER);
                row.text(DETAIL_COLUMN, detail, BOOT_AMBER);
            }
            StepState::Ready => {
                row.text(MARK_COLUMN, &[ARROW], BOOT_AMBER);
                row.text(NAME_COLUMN, line.name.as_bytes(), BOOT_GLOW);
                row.text(DETAIL_COLUMN, detail, shade(level - 3));
            }
        }
        row.flush(w, STEPS_ROW + i);
    }
}

/// Load the boot palette, clear the screen, hide the cursor, and draw the
/// static parts: the background, the tagline, the empty bar.
/// `total_weight` is the sum of the weights of all steps that will be
/// reported (1 for each [`step`], plus each [`start_step`]'s); it scales
/// the progress bar.
pub fn begin(total_weight: usize) {
    theme::load(&theme::BOOT, theme::FULL);
    {
        let mut log = STEPS.lock();
        log.count = 0;
        log.done = 0;
        log.total = total_weight.max(1);
    }
    LOADED.store(false, Ordering::SeqCst);
    with_writer(|w| {
        w.reset();
        w.set_cursor_visible(false);
        for row in 0..SCREEN_HEIGHT {
            w.fill(row, 0, SCREEN_WIDTH, b' ', shade(0), background(row));
        }
        // The screen was just written behind SHOWN's back.
        *SHOWN.lock() = [[UNKNOWN; SCREEN_WIDTH]; SCREEN_HEIGHT];
        draw_tagline(w);
    });
    draw_frame(&Frame {
        logo: 0,
        phase: 0,
        percent: 0,
        ready: false,
    });
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

/// Add a line (upper case). The last row is kept for "SYSTEM READY"; once
/// the others are used up, lines are dropped (the kernel log still has
/// them).
fn push_line(log: &mut StepLog, name: &str, detail: fmt::Arguments, state: StepState) {
    if log.count >= STEP_LINES - 1 {
        return;
    }
    let line = &mut log.lines[log.count];
    line.name = LineBuffer::new();
    line.detail = LineBuffer::new();
    let _ = line.name.write_str(name);
    line.name.make_ascii_uppercase();
    let _ = line.detail.write_fmt(detail);
    line.detail.make_ascii_uppercase();
    line.state = state;
    log.count += 1;
}

/// The running step's line, if the last line is one.
fn running_line(log: &mut StepLog) -> Option<&mut StepLine> {
    let last = log.count.checked_sub(1)?;
    let line = &mut log.lines[last];
    (line.state == StepState::Running).then_some(line)
}

/// Redraw the step lines now (the animation thread also does every frame).
fn redraw_steps(log: &StepLog) {
    let phase = crate::debug::uptime_ticks() as usize;
    with_writer(|w| draw_steps(w, log, phase));
}

/// Report a step that has already finished (the fast ones, during early
/// boot): a new line, and an info log message.
pub fn step(name: &str, detail: fmt::Arguments) {
    crate::log_info!("{}: {}", name, detail);
    let mut log = STEPS.lock();
    push_line(&mut log, name, detail, StepState::Done);
    log.done += 1;
    redraw_steps(&log);
}

/// Start a slow step: a new line with a spinner, until [`finish_step`] (or
/// [`fail_step`]). `weight` is its share of the progress bar, relative to
/// the fast steps' 1 (roughly: how much longer it takes).
pub fn start_step(name: &str, weight: usize) {
    crate::log_info!("{}: started", name);
    let mut log = STEPS.lock();
    push_line(&mut log, name, format_args!(""), StepState::Running);
    log.running_weight = weight;
    log.running_since = crate::debug::uptime_ticks();
    redraw_steps(&log);
}

/// Finish the running step with its detail text.
pub fn finish_step(detail: fmt::Arguments) {
    let mut log = STEPS.lock();
    let ticks = crate::debug::uptime_ticks().wrapping_sub(log.running_since);
    if let Some(line) = running_line(&mut log) {
        crate::log_info!(
            "{}: {} ({} ms)",
            core::str::from_utf8(line.name.as_bytes()).unwrap_or("?"),
            detail,
            ticks * 55
        );
        line.detail = LineBuffer::new();
        let _ = line.detail.write_fmt(detail);
        line.detail.make_ascii_uppercase();
        line.state = StepState::Done;
    }
    log.done += log.running_weight;
    log.running_weight = 0;
    redraw_steps(&log);
}

/// Mark the running step as failed, with the error. The caller then panics
/// (or carries on without it).
pub fn fail_step(error: fmt::Arguments) {
    crate::log_error!("boot step failed: {}", error);
    let mut log = STEPS.lock();
    if let Some(line) = running_line(&mut log) {
        line.detail = LineBuffer::new();
        let _ = line.detail.write_fmt(error);
        line.detail.make_ascii_uppercase();
        line.state = StepState::Failed;
    }
    redraw_steps(&log);
}

/// Everything is loaded: the bar may fill up, and [`play`] can finish.
pub fn loading_done() {
    LOADED.store(true, Ordering::SeqCst);
}

/// Where the bar is heading, in percent: the finished steps' share, plus a
/// creep for the running one; 100 only once [`loading_done`].
fn progress_target(log: &StepLog, now: u64) -> usize {
    if LOADED.load(Ordering::SeqCst) {
        return 100;
    }
    let mut tenths = log.done * 10;
    if log.count > 0 && log.lines[log.count - 1].state == StepState::Running {
        let creep = (now.wrapping_sub(log.running_since) / CREEP_TICKS).min(9) as usize;
        tenths += creep * log.running_weight;
    }
    (tenths * 100 / (log.total * 10)).min(99)
}

/// One animation frame.
#[derive(Debug, Clone, Copy)]
pub struct Frame {
    /// Position in the logo's sequence (sweep, then shimmer), `0..=`
    /// [`LOGO_FRAMES`]; it stays complete after that.
    pub logo: usize,
    /// Keeps counting for as long as the screen is up: moves the wave field
    /// and the spinners.
    pub phase: usize,
    /// The bar, 0–100.
    pub percent: usize,
    /// The final picture: the "SYSTEM READY" line.
    pub ready: bool,
}

/// Draw one frame. Pure drawing, no waiting, so tests can call it.
///
/// - The wave field moves with `phase`.
/// - Logo frames 10–55%: the logo sweeps in from the left, with a
///   glow-white leading edge; 60–95%: a bright diagonal shimmer.
/// - The bar shows `percent`.
pub fn draw_frame(frame: &Frame) {
    let total = LOGO_FRAMES;
    let logo_frame = frame.logo.min(total);
    let sweep_start = total / 10;
    let sweep_end = total * 55 / 100;
    let revealed = if logo_frame >= sweep_end {
        LOGO_WIDTH + 4
    } else if logo_frame <= sweep_start {
        0
    } else {
        (LOGO_WIDTH + 4) * (logo_frame - sweep_start) / (sweep_end - sweep_start)
    };
    let shimmer_start = total * 60 / 100;
    let shimmer_end = total * 95 / 100;
    let shimmer = if logo_frame >= shimmer_start && logo_frame < shimmer_end {
        let span = LOGO_WIDTH as i32 + 30;
        Some((logo_frame - shimmer_start) as i32 * span / (shimmer_end - shimmer_start) as i32 - 15)
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
                    None => field_cell(row, column, frame.phase),
                };
                text(w, row, column, &[cell], color);
            }
        }

        draw_bar(w, frame);

        let mut log = STEPS.lock();
        if frame.ready {
            let last = log.count.checked_sub(1).map(|i| log.lines[i].state);
            if last != Some(StepState::Ready) {
                let line = log.count.min(STEP_LINES - 1);
                let ready = &mut log.lines[line];
                ready.name = LineBuffer::new();
                ready.detail = LineBuffer::new();
                let _ = ready.name.write_str("SYSTEM READY");
                let _ = ready.detail.write_str("HANDING OVER TO /BIN/SH");
                ready.state = StepState::Ready;
                log.count = line + 1;
            }
        }
        draw_steps(w, &log, frame.phase);
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
fn draw_bar(w: &mut Writer, frame: &Frame) {
    let percent = frame.percent.min(100);
    let label = b"STARTING /BIN/SH";
    let bar_column = 3 + label.len() + 3;
    let bar_width = SCREEN_WIDTH - bar_column - 8;
    let filled = bar_width * percent / 100;
    text(w, BAR_ROW, 3, label, shade(10));
    let mark = if frame.ready { SQUARE } else { spinner(frame.phase) };
    text(w, BAR_ROW, 3 + label.len() + 1, &[mark], BOOT_AMBER);
    for i in 0..bar_width {
        let (cell, fg, bg) = if i + 1 == filled && percent < 100 {
            (FULL, BOOT_GLOW, background(BAR_ROW))
        } else if i < filled {
            gradient_cell(i, bar_width, 3, 13)
        } else {
            (DOT, shade(3), background(BAR_ROW))
        };
        put(w, BAR_ROW, bar_column + i, &[cell], fg, bg);
    }
    let mut label = LineBuffer::new();
    let _ = write!(label, "{:3}%", percent);
    text(w, BAR_ROW, bar_column + bar_width + 2, label.as_bytes(), shade(13));
}

/// Animate until loading is done, then hold the final picture briefly.
/// Runs in its own kernel thread (with interrupts on) while another thread
/// does the loading and reports steps.
///
/// Each pass draws a frame and sleeps a tick. The logo's sequence and the
/// wave field follow the elapsed time (a late wakeup skips frames). The bar
/// eases towards [`progress_target`] and never goes back. It ends when
/// loading is done, the bar is full, and the logo sequence has played, so
/// a fast boot still shows the whole animation.
pub fn play() {
    use crate::scheduler::scheduler::sleep;
    let start = crate::debug::uptime_ticks();
    let mut percent = 0usize;
    loop {
        let now = crate::debug::uptime_ticks();
        let elapsed = now.wrapping_sub(start) as usize;
        let target = progress_target(&STEPS.lock(), now);
        if percent < target {
            // An eighth of the way each frame (~18 frames a second), at
            // least one percent.
            percent += (target - percent + 7) / 8;
        }
        let logo = elapsed.min(LOGO_FRAMES);
        let ready = LOADED.load(Ordering::SeqCst) && percent >= 100 && logo == LOGO_FRAMES;
        draw_frame(&Frame {
            logo,
            phase: elapsed,
            percent,
            ready,
        });
        if ready {
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
