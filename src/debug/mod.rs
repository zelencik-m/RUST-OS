//! Kernel log: leveled messages on the serial port.
//!
//! ```text
//! [   0.164] INFO  fs::ext2: mounted: 32 MiB, 1024-byte blocks, 4 groups
//! [   1.812] WARN  interrupts: page fault in user mode at 0x0, killing PID(3)
//! ```
//!
//! - Each line has the uptime (timer ticks → seconds), a level, and the
//!   module that logged it (`module_path!()`, without the `rust_os::`
//!   prefix). On a terminal the level is colored (ANSI).
//! - [`set_level`] picks the most verbose level that is printed; `None`
//!   (the default) prints nothing. `src/main.rs` sets it for a normal boot.
//!   Test kernels leave it off to keep their reports clean (`basic_boot`
//!   checks that).
//! - Macros, from least to most verbose: [`log_error!`](crate::log_error),
//!   [`log_warn!`](crate::log_warn), [`log_info!`](crate::log_info),
//!   [`log_debug!`](crate::log_debug), [`log_trace!`](crate::log_trace).
//!   `trace` is for things that happen constantly (every context switch,
//!   every sleep). A disabled level costs one atomic load: the arguments
//!   aren't even formatted.
//!
//! Output goes only to serial, never to the VGA screen, so it doesn't mix
//! with what the user sees. (The serial port also gets a copy of console
//! output, so a verbose level is noisy next to the shell.)

use core::fmt;
use core::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;
use uart_16550::SerialPort;

mod macros;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Level {
    /// Something is broken (the kernel can carry on).
    Error = 1,
    /// Something went wrong for a user program (e.g. it was killed).
    Warn = 2,
    /// Milestones: boot steps, filesystem mounted, shell started.
    Info = 3,
    /// Details useful while debugging one subsystem.
    Debug = 4,
    /// Very frequent events: context switches, sleeps, CR3 switches.
    Trace = 5,
}

impl Level {
    fn tag(self) -> &'static str {
        match self {
            Level::Error => "\x1b[1;31mERROR\x1b[0m",
            Level::Warn => "\x1b[1;33mWARN \x1b[0m",
            Level::Info => "\x1b[1;32mINFO \x1b[0m",
            Level::Debug => "\x1b[1;36mDEBUG\x1b[0m",
            Level::Trace => "\x1b[90mTRACE\x1b[0m",
        }
    }
}

/// The most verbose level that is printed, as a `u8` (0 = off).
static MAX_LEVEL: AtomicU8 = AtomicU8::new(0);

/// Print messages up to `level` (`None`: nothing).
pub fn set_level(level: Option<Level>) {
    MAX_LEVEL.store(level.map_or(0, |level| level as u8), Ordering::Relaxed);
}

pub fn level() -> Option<Level> {
    match MAX_LEVEL.load(Ordering::Relaxed) {
        1 => Some(Level::Error),
        2 => Some(Level::Warn),
        3 => Some(Level::Info),
        4 => Some(Level::Debug),
        5 => Some(Level::Trace),
        _ => None,
    }
}

/// Whether messages at `level` are printed. The macros check this first.
#[inline]
pub fn enabled(level: Level) -> bool {
    level as u8 <= MAX_LEVEL.load(Ordering::Relaxed)
}

/// Whether any logging is on.
pub fn is_enabled() -> bool {
    MAX_LEVEL.load(Ordering::Relaxed) != 0
}

/// Timer ticks since boot, for the timestamps. The timer interrupt bumps
/// it (`Scheduler::tick`); an atomic, so logging never needs the
/// scheduler lock (it's often logged *under* that lock).
static UPTIME_TICKS: AtomicU64 = AtomicU64::new(0);

/// The PIT's default rate is 1193182 / 65536 ≈ 18.2065 Hz: 54925 µs a tick.
const MICROS_PER_TICK: u64 = 54_925;

pub fn record_tick() {
    UPTIME_TICKS.fetch_add(1, Ordering::Relaxed);
}

/// Milliseconds since boot (timer resolution: ~55 ms).
pub fn uptime_millis() -> u64 {
    UPTIME_TICKS.load(Ordering::Relaxed) * MICROS_PER_TICK / 1000
}

lazy_static! {
    static ref SERIAL: Mutex<SerialPort> = {
        let mut serial = unsafe { SerialPort::new(0x3F8) };
        serial.init();
        Mutex::new(serial)
    };
}

/// Write one log line. Use the macros instead: they check the level first.
#[doc(hidden)]
pub fn _log(level: Level, module: &str, args: fmt::Arguments) {
    let module = module.strip_prefix("rust_os::").unwrap_or(module);
    // The binary (src/main.rs) is the crate root of its own crate.
    let module = if module == "rust_os" { "kernel" } else { module };
    let millis = uptime_millis();
    write_serial(format_args!(
        "\x1b[90m[{:4}.{:03}]\x1b[0m {} \x1b[90m{}:\x1b[0m {}\n",
        millis / 1000,
        millis % 1000,
        level.tag(),
        module,
        args
    ));
}

fn write_serial(args: fmt::Arguments) {
    use core::fmt::Write;
    use x86_64::instructions::interrupts;
    use x86_64::registers::control::Cr3;
    use x86_64::structures::paging::{PhysFrame, Size4KiB};
    use x86_64::PhysAddr;

    interrupts::without_interrupts(|| {
        // Logging can happen while a user address space is active. Switch
        // to the kernel's page tables so the lazily initialized port state
        // is reachable, as the VGA writer does.
        let (current_cr3, flags) = Cr3::read();
        let kernel_cr3_phys = crate::vga_buffer::KERNEL_CR3_PHYS.load(Ordering::Relaxed);
        let switched = kernel_cr3_phys != 0 && current_cr3.start_address().as_u64() != kernel_cr3_phys;
        if switched {
            unsafe {
                Cr3::write(
                    PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(kernel_cr3_phys)),
                    flags,
                );
            }
        }

        SERIAL.lock().write_fmt(args).expect("Printing to serial failed");

        if switched {
            unsafe {
                Cr3::write(current_cr3, flags);
            }
        }
    });
}
