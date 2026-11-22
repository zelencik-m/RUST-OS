use core::sync::atomic::AtomicU64;

pub static KERNEL_CR3_PHYS: AtomicU64 = AtomicU64::new(0);

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Color {
    Black = 0,
    Blue = 1,
    Green = 2,
    Cyan = 3,
    Red = 4,
    Magenta = 5,
    Brown = 6,
    LightGray = 7,
    DarkGray = 8,
    LightBlue = 9,
    LightGreen = 10,
    LightCyan = 11,
    LightRed = 12,
    Pink = 13,
    Yellow = 14,
    White = 15,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
struct ColorCode(u8);

impl ColorCode {
    fn new(foreground: Color, background: Color) -> ColorCode {
        ColorCode((background as u8) << 4 | (foreground as u8))
    }
}


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct ScreenChar {
    ascii_character: u8,
    color_code: ColorCode,
}

const BUFFER_HEIGHT: usize = 25;
const BUFFER_WIDTH: usize = 80;
use volatile::Volatile;

struct Buffer {
    chars: [[Volatile<ScreenChar>; BUFFER_WIDTH]; BUFFER_HEIGHT],
}


/// Default text color: light gray on black, like a PC console.
pub const DEFAULT_FG: Color = Color::LightGray;
pub const DEFAULT_BG: Color = Color::Black;

/// ANSI color numbers (30–37, 40–47) in VGA order.
const ANSI_COLORS: [Color; 8] = [
    Color::Black,
    Color::Red,
    Color::Green,
    Color::Brown,
    Color::Blue,
    Color::Magenta,
    Color::Cyan,
    Color::LightGray,
];

/// The bright variant of a VGA color (its value + 8).
fn bright(color: Color) -> Color {
    match color {
        Color::Black => Color::DarkGray,
        Color::Blue => Color::LightBlue,
        Color::Green => Color::LightGreen,
        Color::Cyan => Color::LightCyan,
        Color::Red => Color::LightRed,
        Color::Magenta => Color::Pink,
        Color::Brown => Color::Yellow,
        Color::LightGray => Color::White,
        other => other,
    }
}

/// Most parameters kept from one escape sequence (`ESC [ 1;34 m` has 2).
const MAX_ESCAPE_PARAMS: usize = 4;

/// Where the writer is in an escape sequence. Sequences may be split across
/// writes (a program can print them byte by byte), so this is state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Escape {
    None,
    /// Saw `ESC`.
    Esc,
    /// Inside `ESC [ params`.
    Csi {
        params: [u16; MAX_ESCAPE_PARAMS],
        count: usize,
    },
}

/// The text console: an 80x25 terminal on the VGA text buffer.
///
/// Text starts at the top-left after a clear and fills the screen downward;
/// once the cursor is on the bottom row, a newline scrolls everything up.
/// Rows above [`Writer::set_scroll_top`] are reserved (the kernel's status
/// bar): text output never writes, scrolls or clears them.
///
/// Control bytes:
/// - `\n` new line, `\r` column 0, `\t` next multiple of 8
/// - `\x08` (backspace): one column left, without erasing; no-op at column 0
/// - `\x0c` (form feed): clear the screen, cursor to the top-left
///
/// ANSI escape sequences (`ESC [ params final`), the subset a shell needs:
/// - `m` (SGR) colors: `0` reset, `1` bold (bright foreground), `22` not
///   bold, `30`–`37`/`90`–`97` foreground, `40`–`47`/`100`–`107`
///   background, `39`/`49` default foreground/background
/// - `J` clear the screen (cursor to the top-left), `K` clear to the end of
///   the row, `H` move to row;column (1-based, default the top-left)
/// - `A`/`B`/`C`/`D` cursor up/down/right/left
/// Other sequences are swallowed.
///
/// Non-ASCII text is decoded as UTF-8. Characters the VGA font has (box
/// drawing `─│┌┐`, blocks `░▒▓█▀▄`, arrows `►◄↑↓`, `·•°`, some accented
/// letters; see [`cp437`]) are shown as such, anything else as a block (■).
/// Raw CP437 bytes can be drawn with [`Writer::draw`].
pub struct Writer {
    row: usize,
    column: usize,
    /// First row of the scrolling region; rows above it are reserved.
    top: usize,
    /// A UTF-8 character being decoded: its bits so far, and how many
    /// continuation bytes are still missing (0: none in progress).
    utf8_bits: u32,
    utf8_missing: u8,
    fg: Color,
    bg: Color,
    bold: bool,
    escape: Escape,
    buffer: &'static mut Buffer,
    /// Whether the hardware cursor's shape has been programmed yet.
    cursor_enabled: bool,
}

/// VGA CRT controller ports: write a register index to 0x3D4, then read or
/// write that register through 0x3D5.
const CRTC_INDEX: u16 = 0x3D4;
const CRTC_DATA: u16 = 0x3D5;
const CRTC_CURSOR_START: u8 = 0x0A;
const CRTC_CURSOR_END: u8 = 0x0B;
const CRTC_CURSOR_HIGH: u8 = 0x0E;
const CRTC_CURSOR_LOW: u8 = 0x0F;
/// Bit 5 of the cursor-start register hides the cursor.
const CURSOR_DISABLE: u8 = 0x20;

fn crtc_write(register: u8, value: u8) {
    use x86_64::instructions::port::Port;
    unsafe {
        Port::<u8>::new(CRTC_INDEX).write(register);
        Port::<u8>::new(CRTC_DATA).write(value);
    }
}

fn crtc_read(register: u8) -> u8 {
    use x86_64::instructions::port::Port;
    unsafe {
        Port::<u8>::new(CRTC_INDEX).write(register);
        Port::<u8>::new(CRTC_DATA).read()
    }
}

/// The hardware cursor's position as a cell index (`row * 80 + column`).
pub fn hardware_cursor() -> usize {
    let high = crtc_read(CRTC_CURSOR_HIGH) as usize;
    let low = crtc_read(CRTC_CURSOR_LOW) as usize;
    (high << 8) | low
}

impl Writer {
    fn new() -> Self {
        Writer {
            row: 0,
            column: 0,
            top: 0,
            utf8_bits: 0,
            utf8_missing: 0,
            fg: DEFAULT_FG,
            bg: DEFAULT_BG,
            bold: false,
            escape: Escape::None,
            // SAFETY: the VGA text buffer is identity-mapped at 0xb8000 and
            // this is its only writer.
            buffer: unsafe { &mut *(0xb8000 as *mut Buffer) },
            cursor_enabled: false,
        }
    }

    /// `(row, column)` where the next character goes.
    pub fn position(&self) -> (usize, usize) {
        (self.row, self.column)
    }

    /// Write one byte (see the type docs) and move the hardware cursor.
    pub fn write_byte(&mut self, byte: u8) {
        self.put_byte(byte);
        self.update_cursor();
    }

    pub fn write_string(&mut self, s: &str) {
        for byte in s.bytes() {
            self.put_byte(byte);
        }
        self.update_cursor();
    }

    fn color_code(&self) -> ColorCode {
        let fg = if self.bold { bright(self.fg) } else { self.fg };
        ColorCode::new(fg, self.bg)
    }

    fn put_byte(&mut self, byte: u8) {
        match self.escape {
            Escape::None => {}
            Escape::Esc => {
                self.escape = if byte == b'[' {
                    Escape::Csi {
                        params: [0; MAX_ESCAPE_PARAMS],
                        count: 0,
                    }
                } else {
                    Escape::None
                };
                return;
            }
            Escape::Csi { mut params, mut count } => {
                match byte {
                    b'0'..=b'9' => {
                        if count == 0 {
                            count = 1;
                        }
                        if count <= MAX_ESCAPE_PARAMS {
                            let p = &mut params[count - 1];
                            *p = p.saturating_mul(10).saturating_add((byte - b'0') as u16);
                        }
                        self.escape = Escape::Csi { params, count };
                    }
                    b';' => {
                        // An empty parameter counts as 0: "ESC[;5H".
                        count = count.max(1) + 1;
                        self.escape = Escape::Csi { params, count };
                    }
                    0x40..=0x7e => {
                        self.escape = Escape::None;
                        let count = count.min(MAX_ESCAPE_PARAMS);
                        self.run_escape(byte, &params[..count]);
                    }
                    // Intermediate and private-marker bytes ('?', ' ', ...).
                    _ => self.escape = Escape::Csi { params, count },
                }
                return;
            }
        }

        if byte >= 0x80 {
            self.put_utf8(byte);
            return;
        }
        if self.utf8_missing != 0 {
            // A character was cut off.
            self.utf8_missing = 0;
            self.put_char(UNKNOWN_CHAR);
        }
        match byte {
            0x1b => self.escape = Escape::Esc,
            b'\n' => self.new_line(),
            b'\r' => self.column = 0,
            b'\t' => {
                let next = (self.column / 8 + 1) * 8;
                while self.column < next.min(BUFFER_WIDTH) {
                    self.put_char(b' ');
                }
            }
            0x08 => self.column = self.column.saturating_sub(1),
            0x0c => self.clear_screen(),
            0x20..=0x7e => self.put_char(byte),
            // Other control bytes.
            _ => {}
        }
    }

    /// One byte of a multi-byte UTF-8 character.
    fn put_utf8(&mut self, byte: u8) {
        if byte & 0xc0 == 0x80 {
            if self.utf8_missing == 0 {
                return; // a stray continuation byte
            }
            self.utf8_bits = (self.utf8_bits << 6) | (byte & 0x3f) as u32;
            self.utf8_missing -= 1;
            if self.utf8_missing == 0 {
                let shown = char::from_u32(self.utf8_bits)
                    .and_then(cp437)
                    .unwrap_or(UNKNOWN_CHAR);
                self.put_char(shown);
            }
            return;
        }
        if self.utf8_missing != 0 {
            self.put_char(UNKNOWN_CHAR); // the previous one was cut off
        }
        let (bits, missing) = match byte {
            0xc0..=0xdf => (byte & 0x1f, 1),
            0xe0..=0xef => (byte & 0x0f, 2),
            0xf0..=0xf7 => (byte & 0x07, 3),
            _ => {
                self.utf8_missing = 0;
                self.put_char(UNKNOWN_CHAR);
                return;
            }
        };
        self.utf8_bits = bits as u32;
        self.utf8_missing = missing;
    }

    fn put_char(&mut self, byte: u8) {
        if self.column >= BUFFER_WIDTH {
            self.new_line();
        }
        let color_code = self.color_code();
        self.buffer.chars[self.row][self.column].write(ScreenChar {
            ascii_character: byte,
            color_code,
        });
        self.column += 1;
    }

    /// The first parameter, or `default` if it's missing or 0.
    fn param(params: &[u16], default: usize) -> usize {
        match params.first() {
            Some(&value) if value != 0 => value as usize,
            _ => default,
        }
    }

    fn run_escape(&mut self, command: u8, params: &[u16]) {
        match command {
            b'm' => self.set_graphics(params),
            b'J' => self.clear_screen(),
            b'K' => {
                let blank = self.blank();
                for column in self.column.min(BUFFER_WIDTH)..BUFFER_WIDTH {
                    self.buffer.chars[self.row][column].write(blank);
                }
            }
            b'H' | b'f' => {
                let row = Self::param(params, 1);
                let column = match params.get(1) {
                    Some(&value) if value != 0 => value as usize,
                    _ => 1,
                };
                // Rows count from the top of the scrolling region.
                self.row = (self.top + row - 1).min(BUFFER_HEIGHT - 1);
                self.column = (column - 1).min(BUFFER_WIDTH - 1);
            }
            b'A' => {
                self.row = self.row.saturating_sub(Self::param(params, 1)).max(self.top)
            }
            b'B' => self.row = (self.row + Self::param(params, 1)).min(BUFFER_HEIGHT - 1),
            b'C' => self.column = (self.column + Self::param(params, 1)).min(BUFFER_WIDTH - 1),
            b'D' => self.column = self.column.saturating_sub(Self::param(params, 1)),
            _ => {}
        }
    }

    /// SGR: `ESC [ ... m`. No parameters means reset.
    fn set_graphics(&mut self, params: &[u16]) {
        if params.is_empty() {
            self.reset_colors();
            return;
        }
        for &param in params {
            match param {
                0 => self.reset_colors(),
                1 => self.bold = true,
                22 => self.bold = false,
                30..=37 => self.fg = ANSI_COLORS[(param - 30) as usize],
                39 => self.fg = DEFAULT_FG,
                40..=47 => self.bg = ANSI_COLORS[(param - 40) as usize],
                49 => self.bg = DEFAULT_BG,
                90..=97 => self.fg = bright(ANSI_COLORS[(param - 90) as usize]),
                100..=107 => self.bg = bright(ANSI_COLORS[(param - 100) as usize]),
                _ => {}
            }
        }
    }

    fn reset_colors(&mut self) {
        self.fg = DEFAULT_FG;
        self.bg = DEFAULT_BG;
        self.bold = false;
    }

    fn new_line(&mut self) {
        self.column = 0;
        if self.row + 1 < BUFFER_HEIGHT {
            self.row += 1;
            return;
        }
        for row in self.top + 1..BUFFER_HEIGHT {
            for col in 0..BUFFER_WIDTH {
                let character = self.buffer.chars[row][col].read();
                self.buffer.chars[row - 1][col].write(character);
            }
        }
        self.clear_row(BUFFER_HEIGHT - 1);
    }

    fn blank(&self) -> ScreenChar {
        ScreenChar {
            ascii_character: b' ',
            color_code: ColorCode::new(DEFAULT_FG, self.bg),
        }
    }

    /// Blank the scrolling region and put the cursor at its top-left.
    pub fn clear_screen(&mut self) {
        for row in self.top..BUFFER_HEIGHT {
            self.clear_row(row);
        }
        self.row = self.top;
        self.column = 0;
    }

    /// Reserve the rows above `top` (at most 24): text output starts at
    /// `top`, and scrolling and clearing leave the rows above alone. They
    /// can still be drawn with [`Writer::draw`].
    pub fn set_scroll_top(&mut self, top: usize) {
        self.top = top.min(BUFFER_HEIGHT - 1);
        if self.row < self.top {
            self.row = self.top;
            self.column = 0;
        }
        self.update_cursor();
    }

    /// Back to a fresh console: the whole screen scrolls again, default
    /// colors, no pending escape sequence, cleared screen, visible cursor at
    /// the top-left.
    pub fn reset(&mut self) {
        self.reset_colors();
        self.escape = Escape::None;
        self.utf8_missing = 0;
        self.top = 0;
        self.clear_screen();
        self.set_cursor_visible(true);
        self.update_cursor();
    }

    fn clear_row(&mut self, row: usize) {
        let blank = self.blank();
        for col in 0..BUFFER_WIDTH {
            self.buffer.chars[row][col].write(blank);
        }
    }

    /// Draw raw CP437 bytes at a fixed cell, clipped at the right edge. It
    /// doesn't move the text cursor or scroll; the boot screen uses it.
    pub fn draw(&mut self, row: usize, column: usize, bytes: &[u8], fg: Color, bg: Color) {
        if row >= BUFFER_HEIGHT {
            return;
        }
        let color_code = ColorCode::new(fg, bg);
        for (i, &byte) in bytes.iter().enumerate() {
            let col = column + i;
            if col >= BUFFER_WIDTH {
                break;
            }
            self.buffer.chars[row][col].write(ScreenChar {
                ascii_character: byte,
                color_code,
            });
        }
    }

    /// Fill `length` cells of `row` from `column` with one character.
    pub fn fill(&mut self, row: usize, column: usize, length: usize, byte: u8, fg: Color, bg: Color) {
        for col in column..(column + length).min(BUFFER_WIDTH) {
            self.draw(row, col, &[byte], fg, bg);
        }
    }

    /// The character at a cell (for tests and the boot screen).
    pub fn char_at(&self, row: usize, column: usize) -> u8 {
        self.buffer.chars[row][column].read().ascii_character
    }

    /// The `(foreground, background)` VGA color numbers at a cell.
    pub fn color_at(&self, row: usize, column: usize) -> (u8, u8) {
        let code = self.buffer.chars[row][column].read().color_code.0;
        (code & 0x0f, code >> 4)
    }

    /// Show or hide the blinking hardware cursor.
    pub fn set_cursor_visible(&mut self, visible: bool) {
        let start = crtc_read(CRTC_CURSOR_START) & 0xC0;
        let hidden = if visible { 0 } else { CURSOR_DISABLE };
        crtc_write(CRTC_CURSOR_START, start | hidden | 14);
        let end = crtc_read(CRTC_CURSOR_END) & 0xE0;
        crtc_write(CRTC_CURSOR_END, end | 15);
        self.cursor_enabled = true;
    }

    /// Move the blinking hardware cursor to where the next character goes.
    fn update_cursor(&mut self) {
        if !self.cursor_enabled {
            // Scanlines 14-15 (an underline).
            self.set_cursor_visible(true);
        }
        // After a full line the column is 80 until the next character wraps;
        // keep the cursor on the last cell instead of off the screen.
        let column = core::cmp::min(self.column, BUFFER_WIDTH - 1);
        let position = self.row * BUFFER_WIDTH + column;
        crtc_write(CRTC_CURSOR_LOW, (position & 0xFF) as u8);
        crtc_write(CRTC_CURSOR_HIGH, (position >> 8) as u8);
    }
}

/// Shown for characters the VGA font doesn't have: ■.
const UNKNOWN_CHAR: u8 = 0xfe;

/// The CP437 (VGA font) code for a Unicode character, if the font has it.
/// ASCII maps to itself.
pub fn cp437(c: char) -> Option<u8> {
    if (c as u32) < 0x80 {
        return Some(c as u8);
    }
    let code = match c {
        // Box drawing, single and double lines.
        '─' => 0xc4, '│' => 0xb3, '┌' => 0xda, '┐' => 0xbf, '└' => 0xc0, '┘' => 0xd9,
        '├' => 0xc3, '┤' => 0xb4, '┬' => 0xc2, '┴' => 0xc1, '┼' => 0xc5,
        '═' => 0xcd, '║' => 0xba, '╔' => 0xc9, '╗' => 0xbb, '╚' => 0xc8, '╝' => 0xbc,
        '╠' => 0xcc, '╣' => 0xb9, '╦' => 0xcb, '╩' => 0xca, '╬' => 0xce,
        '╒' => 0xd5, '╓' => 0xd6, '╕' => 0xb8, '╖' => 0xb7, '╘' => 0xd4, '╙' => 0xd3,
        '╛' => 0xbe, '╜' => 0xbd, '╞' => 0xc6, '╟' => 0xc7, '╡' => 0xb5, '╢' => 0xb6,
        '╤' => 0xd1, '╥' => 0xd2, '╧' => 0xcf, '╨' => 0xd0, '╪' => 0xd8, '╫' => 0xd7,
        // Blocks and shades.
        '█' => 0xdb, '▄' => 0xdc, '▌' => 0xdd, '▐' => 0xde, '▀' => 0xdf,
        '░' => 0xb0, '▒' => 0xb1, '▓' => 0xb2, '■' => 0xfe,
        // Arrows and shapes.
        '►' | '▶' | '▸' => 0x10, '◄' | '◀' | '◂' => 0x11, '▲' => 0x1e, '▼' => 0x1f,
        '↑' => 0x18, '↓' => 0x19, '→' => 0x1a, '←' => 0x1b, '↕' => 0x12, '↔' => 0x1d,
        '•' => 0x07, '◘' => 0x08, '○' => 0x09, '◙' => 0x0a, '♦' => 0x04, '♥' => 0x03,
        '♣' => 0x05, '♠' => 0x06, '☺' => 0x01, '☻' => 0x02, '☼' => 0x0f, '♪' => 0x0d,
        '♫' => 0x0e, '§' => 0x15, '¶' => 0x14, '▬' => 0x16, '⌂' => 0x7f,
        // Punctuation, math and Greek.
        '·' => 0xfa, '°' => 0xf8, '±' => 0xf1, '²' => 0xfd, '√' | '✓' => 0xfb,
        '≡' => 0xf0, '≈' => 0xf7, '≥' => 0xf2, '≤' => 0xf3, '÷' => 0xf6, '∞' => 0xec,
        '«' => 0xae, '»' => 0xaf, '¬' => 0xaa, '½' => 0xab, '¼' => 0xac, '¡' => 0xad,
        '¿' => 0xa8, '£' => 0x9c, '¥' => 0x9d, '¢' => 0x9b, 'µ' => 0xe6, 'ß' => 0xe1,
        'α' => 0xe0, 'π' => 0xe3, 'Σ' => 0xe4, 'σ' => 0xe5, 'Ω' => 0xea, 'δ' => 0xeb,
        // Accented letters.
        'é' => 0x82, 'è' => 0x8a, 'ê' => 0x88, 'ë' => 0x89, 'à' => 0x85, 'â' => 0x83,
        'ä' => 0x84, 'å' => 0x86, 'ç' => 0x87, 'ï' => 0x8b, 'î' => 0x8c, 'ì' => 0x8d,
        'ô' => 0x93, 'ö' => 0x94, 'ò' => 0x95, 'û' => 0x96, 'ù' => 0x97, 'ü' => 0x81,
        'ÿ' => 0x98, 'á' => 0xa0, 'í' => 0xa1, 'ó' => 0xa2, 'ú' => 0xa3, 'ñ' => 0xa4,
        'Ñ' => 0xa5, 'Ä' => 0x8e, 'Å' => 0x8f, 'É' => 0x90, 'Ö' => 0x99, 'Ü' => 0x9a,
        'Ç' => 0x80, 'æ' => 0x91, 'Æ' => 0x92,
        _ => return None,
    };
    Some(code)
}

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

/// Attribute controller: index/data port (the two alternate on writes) and
/// its read port. Reading `INPUT_STATUS` resets it to "index".
const ATTRIBUTE_PORT: u16 = 0x3C0;
const ATTRIBUTE_READ: u16 = 0x3C1;
const INPUT_STATUS: u16 = 0x3DA;
/// Attribute mode control register; bit 3 makes bright backgrounds blink.
const ATTRIBUTE_MODE: u8 = 0x10;
const BLINK_ENABLE: u8 = 0x08;
/// Set in the index written last, or the screen stays blank.
const PALETTE_ADDRESS_SOURCE: u8 = 0x20;
/// DAC: write the first entry's index, then r, g, b (6 bits each) per entry.
const DAC_WRITE_INDEX: u16 = 0x3C8;
const DAC_READ_INDEX: u16 = 0x3C7;
const DAC_DATA: u16 = 0x3C9;

/// Replace the 16 text colors. Entry `i` is the color of [`Color`] `i`, as
/// 8-bit `(r, g, b)` (the DAC keeps 6 bits per channel). Also turns off
/// blinking, so backgrounds 8–15 are bright colors.
///
/// The attribute controller maps color `i` to DAC entry `i` (normally 6, 8–15
/// map to 20 and 56–63), so the DAC's first 16 entries are the palette.
pub fn set_palette(colors: &[(u8, u8, u8); 16]) {
    use x86_64::instructions::port::Port;
    x86_64::instructions::interrupts::without_interrupts(|| unsafe {
        let mut status = Port::<u8>::new(INPUT_STATUS);
        let mut attribute = Port::<u8>::new(ATTRIBUTE_PORT);
        let mut attribute_read = Port::<u8>::new(ATTRIBUTE_READ);
        status.read();
        for i in 0..16u8 {
            attribute.write(i);
            attribute.write(i);
        }
        attribute.write(ATTRIBUTE_MODE);
        let mode = attribute_read.read();
        attribute.write(mode & !BLINK_ENABLE);
        attribute.write(PALETTE_ADDRESS_SOURCE);

        let mut index = Port::<u8>::new(DAC_WRITE_INDEX);
        let mut data = Port::<u8>::new(DAC_DATA);
        index.write(0);
        for &(r, g, b) in colors {
            data.write(r >> 2);
            data.write(g >> 2);
            data.write(b >> 2);
        }
    });
}

/// DAC entry `index` as 8-bit `(r, g, b)` (the low 2 bits are lost).
pub fn palette_entry(index: u8) -> (u8, u8, u8) {
    use x86_64::instructions::port::Port;
    x86_64::instructions::interrupts::without_interrupts(|| unsafe {
        Port::<u8>::new(DAC_READ_INDEX).write(index);
        let mut data = Port::<u8>::new(DAC_DATA);
        let r = data.read();
        let g = data.read();
        let b = data.read();
        (r << 2, g << 2, b << 2)
    })
}

/// A fixed-size text buffer for formatting without the heap (before it
/// exists, or in an interrupt). Text that doesn't fit is cut off.
pub struct LineBuffer {
    bytes: [u8; BUFFER_WIDTH],
    length: usize,
}

impl LineBuffer {
    pub const fn new() -> Self {
        LineBuffer {
            bytes: [0; BUFFER_WIDTH],
            length: 0,
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.length]
    }

    pub fn make_ascii_uppercase(&mut self) {
        self.bytes[..self.length].make_ascii_uppercase();
    }
}

impl fmt::Write for LineBuffer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &byte in s.as_bytes() {
            if self.length == self.bytes.len() {
                break;
            }
            self.bytes[self.length] = byte;
            self.length += 1;
        }
        Ok(())
    }
}

pub const SCREEN_WIDTH: usize = BUFFER_WIDTH;
pub const SCREEN_HEIGHT: usize = BUFFER_HEIGHT;

use core::fmt;

impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

use lazy_static::lazy_static;
use spin::Mutex;

lazy_static! {
    pub static ref WRITER: Mutex<Writer> = Mutex::new(Writer::new());
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::vga_buffer::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

/// Print a panic report in red, for the panic handler. Unlike `print!`, it
/// doesn't wait for the writer lock: if the panic happened while the
/// writer was locked, nothing is shown (the serial log still has it).
pub fn print_panic(info: &core::panic::PanicInfo) {
    use core::fmt::Write;
    with_kernel_page_tables(|| {
        if let Some(mut writer) = WRITER.try_lock() {
            let _ = write!(
                writer,
                "\n\x1b[1;37;41m KERNEL PANIC \x1b[0m \x1b[91m{}\x1b[0m\n",
                info
            );
        }
    });
}

/// Run `f` with interrupts off and the kernel's page tables active. The
/// VGA buffer (0xb8000) is only mapped there, and `print!` can be called
/// while a user address space is active (a syscall writing to the console).
pub(crate) fn with_kernel_page_tables(f: impl FnOnce()) {
    use core::sync::atomic::Ordering;
    use x86_64::instructions::interrupts;
    use x86_64::registers::control::Cr3;
    use x86_64::structures::paging::{PhysFrame, Size4KiB};
    use x86_64::PhysAddr;

    interrupts::without_interrupts(|| {
        let (current_cr3, flags) = Cr3::read();
        let kernel_cr3_phys = KERNEL_CR3_PHYS.load(Ordering::Relaxed);
        let switched =
            kernel_cr3_phys != 0 && current_cr3.start_address().as_u64() != kernel_cr3_phys;
        if switched {
            unsafe {
                Cr3::write(
                    PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(kernel_cr3_phys)),
                    flags,
                );
            }
        }
        f();
        if switched {
            unsafe {
                Cr3::write(current_cr3, flags);
            }
        }
    });
}

/// Write raw bytes, as a program sent them. A UTF-8 character split across
/// two writes still comes out right: the writer decodes UTF-8 (and escape
/// sequences) with state that lasts between writes.
pub fn write_bytes(bytes: &[u8]) {
    with_kernel_page_tables(|| {
        let mut writer = WRITER.lock();
        for &byte in bytes {
            writer.put_byte(byte);
        }
        writer.update_cursor();
    });
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    with_kernel_page_tables(|| {
        WRITER.lock().write_fmt(args).unwrap();
    });
}
