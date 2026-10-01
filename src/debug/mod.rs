use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;
use uart_16550::SerialPort;

mod macros;

// Debug logging is opt-in. Normal kernel boot enables it explicitly, while
// integration-test kernels leave it disabled to keep their reports focused.
static DEBUG_ENABLED: AtomicBool = AtomicBool::new(false);

pub fn set_enabled(enabled: bool) {
    DEBUG_ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn is_enabled() -> bool {
    DEBUG_ENABLED.load(Ordering::Relaxed)
}

lazy_static! {
    static ref SERIAL: Mutex<SerialPort> = {
        let mut serial = unsafe { SerialPort::new(0x3F8) };
        serial.init();
        Mutex::new(serial)
    };
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    if !is_enabled() {
        return;
    }

    use core::fmt::Write;
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

        SERIAL
            .lock()
            .write_fmt(args)
            .expect("Printing to serial failed");

        if switched {
            unsafe {
                Cr3::write(current_cr3, flags);
            }
        }
    });
}
