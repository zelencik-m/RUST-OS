#![no_std]
#![no_main]

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use rust_os::process::process_manager;
use rust_os::serial_println;
use rust_os::{debugln, hlt_loop};
use x86_64::instructions::interrupts;

entry_point!(kernel_main);

#[repr(C, align(8))]
struct AlignedBytes<const N: usize>([u8; N]);

macro_rules! embedded_elf {
    ($name:ident, $file:literal) => {
        static $name: AlignedBytes<{ include_bytes!(concat!(env!("OUT_DIR"), "/", $file)).len() }> =
            AlignedBytes(*include_bytes!(concat!(env!("OUT_DIR"), "/", $file)));
    };
}

embedded_elf!(THREAD_B, "thread_b.elf");
embedded_elf!(THREAD_C, "thread_c.elf");
embedded_elf!(HELLO, "hello.elf");

/// Installed to `/bin/<name>` at every boot (see `rust_os::programs`).
fn installed_programs() -> [(&'static str, &'static [u8]); 3] {
    [
        ("hello", &HELLO.0),
        ("thread_b", &THREAD_B.0),
        ("thread_c", &THREAD_C.0),
    ]
}

fn kernel_main(boot_info: &'static BootInfo) -> ! {
    rust_os::debug::set_enabled(true);
    rust_os::init(boot_info);

    // Rewritten on every boot, so the copies on the persistent disk image
    // always match this build.
    rust_os::programs::install(&installed_programs()).expect("failed to install programs in /bin");
    debugln!(">>> installed {} programs in {}", installed_programs().len(), rust_os::programs::BIN_DIR);

    interrupts::without_interrupts(|| {
        launch_user_program("thread_b", &THREAD_B.0);
        launch_user_program("thread_c", &THREAD_C.0);
    });

    interrupts::enable();
    hlt_loop();
}

fn launch_user_program(name: &str, elf: &[u8]) {
    let (pid, tid) = process_manager::spawn_elf(name.into(), elf)
        .unwrap_or_else(|_| panic!("failed to load ELF program {name}"));
    debugln!(
        ">>> launched {} from ELF as PID({:?}), TID({:?})",
        name,
        pid.0,
        tid.0
    );
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    interrupts::without_interrupts(|| {
        serial_println!("{}", info);
    });
    hlt_loop();
}
