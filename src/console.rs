use core::fmt;
use spin::Mutex;

const MAX_CONSOLES: usize = 4;

pub trait Console: Send + Sync{
    fn name(&self) -> &'static str;
    fn write(&self, args: fmt::Arguments);
}

struct Registry{
    consoles: [Option<&'static dyn Console>; MAX_CONSOLES],
    count: usize,
}

static REGISTRY: Mutex<Registry> = Mutex::new(Registry { 
    consoles: [None; MAX_CONSOLES],
    count: 0,
});

pub fn register_console(console: &'static dyn Console) {
    let mut reg = REGISTRY.lock();
    if reg.count < MAX_CONSOLES {
        reg.consoles[reg.count] = Some(console);
        reg.count += 1;
    } else {
        // In a production kernel you might want to warn here.
        // For now we silently drop extra consoles.
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use x86_64::instructions::interrupts;

    interrupts::without_interrupts(|| {
        let reg = REGISTRY.lock();
        for i in 0..reg.count {
            if let Some(console) = reg.consoles[i] {
                console.write(args);
            }
        }
    });
}

/// Prints to all registered kernel consoles.
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::console::_print(format_args!($($arg)*))
    };
}

/// Prints to all registered kernel consoles, appending a newline.
#[macro_export]
macro_rules! println {
    () => {
        $crate::print!("\n")
    };
    ($fmt:expr) => {
        $crate::print!(concat!($fmt, "\n"))
    };
    ($fmt:expr, $($arg:tt)*) => {
        $crate::print!(concat!($fmt, "\n"), $($arg)*)
    };
}