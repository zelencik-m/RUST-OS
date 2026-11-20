// Each integration target gets its own support module; custom-runner suites may
// use only the panic handler and leave the shared runner unused.
#![allow(dead_code)]

use core::panic::PanicInfo;
use core::sync::atomic::{AtomicUsize, Ordering};

const NAME_WIDTH: usize = 66;
static TOTAL: AtomicUsize = AtomicUsize::new(0);
static PASSED: AtomicUsize = AtomicUsize::new(0);

pub trait Testable {
    fn name(&self) -> &'static str;
    fn run(&self);
}

impl<T> Testable for T
where
    T: Fn(),
{
    fn name(&self) -> &'static str {
        core::any::type_name::<T>()
    }

    fn run(&self) {
        let name = self.name();
        self();
        report_pass(name);
    }
}

pub fn test_runner(tests: &[&dyn Testable]) {
    let suite = tests
        .first()
        .and_then(|test| test.name().split_once("::").map(|(suite, _)| suite))
        .unwrap_or("kernel");
    begin_suite(suite, tests.len());
    for test in tests {
        test.run();
    }
    finish_suite();
}

pub fn begin_suite(name: &str, total: usize) {
    TOTAL.store(total, Ordering::Relaxed);
    PASSED.store(0, Ordering::Relaxed);
    rust_os::serial_println!();
    rust_os::serial_println!("┌─ Test suite: {}", name);
    rust_os::serial_println!("│  Running {} tests", total);
    rust_os::serial_println!(
        "├──────────────────────────────────────────────────────────────────────────"
    );
}

pub fn report_pass(name: &str) {
    PASSED.fetch_add(1, Ordering::Relaxed);
    rust_os::serial_println!("│  [PASS] {:<width$}", name, width = NAME_WIDTH);
}

pub fn finish_suite() -> ! {
    let total = TOTAL.load(Ordering::Relaxed);
    let passed = PASSED.load(Ordering::Relaxed);
    rust_os::serial_println!(
        "├──────────────────────────────────────────────────────────────────────────"
    );
    rust_os::serial_println!(
        "└─ Result: PASS — {} passed; 0 failed; {} total",
        passed,
        total
    );
    rust_os::serial_println!();
    rust_os::exit_qemu(rust_os::QemuExitCode::Success);
    rust_os::hlt_loop()
}

pub fn test_panic_handler(info: &PanicInfo) -> ! {
    let total = TOTAL.load(Ordering::Relaxed);
    let passed = PASSED.load(Ordering::Relaxed);
    rust_os::serial_println!("│  [FAIL] {}", info);
    rust_os::serial_println!(
        "├──────────────────────────────────────────────────────────────────────────"
    );
    rust_os::serial_println!(
        "└─ Result: FAIL — {} passed; 1 failed; {} not completed; {} total",
        passed,
        total.saturating_sub(passed + 1),
        total
    );
    rust_os::serial_println!();
    rust_os::exit_qemu(rust_os::QemuExitCode::Failed);
    rust_os::hlt_loop()
}
