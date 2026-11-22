use core::fmt;
use lazy_static::lazy_static;
use spin::Mutex;
use uart_16550::SerialPort;

lazy_static! {
    pub static ref SERIAL1: Mutex<SerialPort> = {
        let mut serial = unsafe { SerialPort::new(0x3F8) };
        serial.init();
        Mutex::new(serial)
    };
}

/// Writes bytes to the port unchanged.
///
/// `SerialPort`'s own `fmt::Write` goes through `send`, which expands every
/// backspace (0x08) and DEL (0x7F) into "back, space, back". That's an erase,
/// not a cursor move. The TTY and the line editor already send their own
/// `\x08 \x08` when they mean "erase", and a bare `\x08` when they mean
/// "move left", so serial output must pass bytes through as-is, just like
/// the VGA writer does.
struct RawSerial<'a>(&'a mut SerialPort);

impl fmt::Write for RawSerial<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            self.0.send_raw(byte);
        }
        Ok(())
    }
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
        let kernel_cr3_phys = crate::vga_buffer::KERNEL_CR3_PHYS.load(Ordering::Relaxed);

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

        RawSerial(&mut SERIAL1.lock())
            .write_fmt(args)
            .expect("serial output failed");

        if switched {
            unsafe {
                Cr3::write(current_cr3, flags);
            }
        }
    });
}

/// Send raw bytes to the host (program output: possibly part of a UTF-8
/// character, which the host terminal puts back together).
pub fn write_bytes(bytes: &[u8]) {
    crate::vga_buffer::with_kernel_page_tables(|| {
        let mut port = SERIAL1.lock();
        for &byte in bytes {
            port.send_raw(byte);
        }
    });
}

/// Prints to the host through the serial interface.
#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => {
        $crate::serial::_print(format_args!($($arg)*));
    };
}

/// Prints to the host through the serial interface, appending a newline.
#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($fmt:expr) => ($crate::serial_print!(concat!($fmt, "\n")));
    ($fmt:expr, $($arg:tt)*) => ($crate::serial_print!(
        concat!($fmt, "\n"), $($arg)*));
}