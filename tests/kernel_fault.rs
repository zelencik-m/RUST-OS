//! A kernel-mode page fault must panic the kernel, never silently kill the
//! current thread. This test deliberately dereferences an address in the
//! user-only P4 slot (empty in the kernel's page tables) and expects the
//! panic handler to run.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(kernel_fault_runner)]
#![reexport_test_harness_main = "test_main"]

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};
use rust_os::serial_println;

static TRIGGERED: AtomicBool = AtomicBool::new(false);

fn kernel_fault_runner(_tests: &[&dyn Fn()]) {}

entry_point!(test_kernel_main);

fn test_kernel_main(boot_info: &'static BootInfo) -> ! {
    rust_os::init(boot_info);

    serial_println!("kernel_fault: triggering a kernel-mode page fault");
    TRIGGERED.store(true, Ordering::SeqCst);

    // The user-only P4 slot is never populated in the kernel's page tables,
    // so this address is guaranteed to be unmapped in kernel mode.
    let unmapped = rust_os::memory::address_space::USER_SPACE_START as *mut u64;
    unsafe {
        core::ptr::write_volatile(unmapped, 42);
    }

    serial_println!("kernel_fault: write unexpectedly succeeded");
    rust_os::exit_qemu(rust_os::QemuExitCode::Failed);
    rust_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    if TRIGGERED.load(Ordering::SeqCst) {
        serial_println!("kernel_fault: panicked as expected: {}", info);
        rust_os::exit_qemu(rust_os::QemuExitCode::Success);
    } else {
        serial_println!("kernel_fault: unexpected panic: {}", info);
        rust_os::exit_qemu(rust_os::QemuExitCode::Failed);
    }
    rust_os::hlt_loop();
}
