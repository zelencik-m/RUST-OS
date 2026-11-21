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


/// Text-mode writer. Output always goes to the bottom row, and the screen
/// scrolls up on a newline. The cursor is therefore always on the bottom row.
///
/// Control bytes (enough for a line editor that redraws its line):
/// - `\n`: new line
/// - `\r`: back to column 0
/// - `\x08` (backspace): one column left, without erasing; no-op at column 0
/// - `\x0c` (form feed): clear the screen, cursor to column 0
///
/// Known limitation: there's no way back to the row above, so text that
/// wraps past column 80 can't be redrawn with `\r`.
pub struct Writer {
    column_position: usize,
    color_code: ColorCode,
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
    /// Write one byte (printable or a supported control byte) and move the
    /// hardware cursor.
    pub fn write_byte(&mut self, byte: u8) {
        self.put_byte(byte);
        self.update_cursor();
    }

    fn put_byte(&mut self, byte: u8) {
        match byte {
            b'\n' => self.new_line(),
            b'\r' => self.column_position = 0,
            0x08 => self.column_position = self.column_position.saturating_sub(1),
            0x0c => self.clear_screen(),
            byte => {
                if self.column_position >= BUFFER_WIDTH {
                    self.new_line();
                }

                let row = BUFFER_HEIGHT - 1;
                let col = self.column_position;

                let color_code = self.color_code;
                self.buffer.chars[row][col].write(ScreenChar {
                    ascii_character: byte,
                    color_code,
                });
                self.column_position += 1;
            }
        }
    }

    fn new_line(&mut self) {
        for row in 1..BUFFER_HEIGHT {
            for col in 0..BUFFER_WIDTH {
                let character = self.buffer.chars[row][col].read();
                self.buffer.chars[row - 1][col].write(character);
            }
        }
        self.clear_row(BUFFER_HEIGHT - 1);
        self.column_position = 0;
    }

    /// Blank every row and put the cursor at column 0 of the bottom row.
    pub fn clear_screen(&mut self) {
        for row in 0..BUFFER_HEIGHT {
            self.clear_row(row);
        }
        self.column_position = 0;
    }

    /// Move the blinking hardware cursor to where the next character goes.
    fn update_cursor(&mut self) {
        if !self.cursor_enabled {
            // Scanlines 14-15 (an underline). Bit 5 of the start register
            // disables the cursor, so it stays clear; keep the reserved bits.
            let start = crtc_read(CRTC_CURSOR_START) & 0xC0;
            crtc_write(CRTC_CURSOR_START, start | 14);
            let end = crtc_read(CRTC_CURSOR_END) & 0xE0;
            crtc_write(CRTC_CURSOR_END, end | 15);
            self.cursor_enabled = true;
        }
        // After a full line the column is 80 until the next character wraps;
        // keep the cursor on the last cell instead of off the screen.
        let column = core::cmp::min(self.column_position, BUFFER_WIDTH - 1);
        let position = (BUFFER_HEIGHT - 1) * BUFFER_WIDTH + column;
        crtc_write(CRTC_CURSOR_LOW, (position & 0xFF) as u8);
        crtc_write(CRTC_CURSOR_HIGH, (position >> 8) as u8);
    }

    fn clear_row(&mut self, row: usize) {
        let blank = ScreenChar {
            ascii_character: b' ',
            color_code: self.color_code,
        };
        for col in 0..BUFFER_WIDTH {
            self.buffer.chars[row][col].write(blank);
        }
    }


    pub fn write_string(&mut self, s: &str) {
        for byte in s.bytes() {
            match byte {
                // printable ASCII, or a control byte the writer understands
                0x20..=0x7e | b'\n' | b'\r' | 0x08 | 0x0c => self.put_byte(byte),
                // anything else shows as a block
                _ => self.put_byte(0xfe),
            }
        }
        self.update_cursor();
    }

}

use core::fmt;

impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

pub fn print_something() {
    use core::fmt::Write;
    let mut writer = Writer {
        column_position: 0,
        color_code: ColorCode::new(Color::Yellow, Color::Black),
        buffer: unsafe { &mut *(0xb8000 as *mut Buffer) },
        cursor_enabled: false,
    };

    writer.write_byte(b'H');
    writer.write_string("ello! ");
    write!(writer, "The numbers are {} and {}", 42, 1.0/3.0).unwrap();
}

use lazy_static::lazy_static;
use spin::Mutex;

lazy_static! {
    pub static ref WRITER: Mutex<Writer> = Mutex::new(Writer {
        column_position: 0,
        color_code: ColorCode::new(Color::Yellow, Color::Black),
        buffer: unsafe { &mut *(0xb8000 as *mut Buffer) },
        cursor_enabled: false,
    });
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

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    use core::sync::atomic::Ordering;
    use x86_64::instructions::interrupts;
    use x86_64::registers::control::Cr3;
    use x86_64::structures::paging::{PhysFrame, Size4KiB};
    use x86_64::PhysAddr;

    interrupts::without_interrupts(|| {
        let (current_cr3, flags) = Cr3::read();
        let kernel_cr3_phys = KERNEL_CR3_PHYS.load(Ordering::Relaxed);

        let switched = if kernel_cr3_phys != 0
            && current_cr3.start_address().as_u64() != kernel_cr3_phys
        {
            unsafe {
                Cr3::write(
                    PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(kernel_cr3_phys)),
                    flags,
                );
            }
            true
        } else {
            false
        };

        WRITER.lock().write_fmt(args).unwrap();

        if switched {
            unsafe {
                Cr3::write(current_cr3, flags);
            }
        }
    });
}
