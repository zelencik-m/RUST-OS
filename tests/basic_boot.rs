#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(support::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;
use rust_os::println;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    test_main();

    loop {}
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    support::test_panic_handler(info)
}

#[test_case]
fn test_println() {
    println!("test_println output");
}

#[test_case]
fn test_vga_buffer_content() {
    const MARKER: &[u8] = b"vga-buffer-test-marker";

    // Reset the column, write the marker, and inspect the scrolled row while
    // holding the shared writer lock so earlier output cannot affect the test.
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut writer = rust_os::vga_buffer::WRITER.lock();
        writer.write_string("\n");
        writer.write_string("vga-buffer-test-marker\n");
        let screen = 0xb8000 as *const u8;
        for (column, expected) in MARKER.iter().enumerate() {
            let actual = unsafe { core::ptr::read_volatile(screen.add((23 * 80 + column) * 2)) };
            assert_eq!(
                actual, *expected,
                "VGA character mismatch at column {}",
                column
            );
        }
    });
}

#[test_case]
fn debug_logging_is_disabled_in_tests() {
    assert!(!rust_os::debug::is_enabled());
}
mod support;
