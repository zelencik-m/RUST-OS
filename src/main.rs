#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(rust_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use rust_os::process::process_manager;
use rust_os::{debugln, hlt_loop};
#[cfg(not(test))]
use rust_os::serial_println;
use x86_64::instructions::interrupts;

extern crate alloc;

entry_point!(kernel_main);

#[repr(C, align(8))]
struct AlignedBytes<const N: usize>([u8; N]);

macro_rules! embedded_elf {
    ($name:ident, $file:literal) => {
        static $name: AlignedBytes<{ include_bytes!(concat!(env!("OUT_DIR"), "/", $file)).len() }> =
            AlignedBytes(*include_bytes!(concat!(env!("OUT_DIR"), "/", $file)));
    };
}

embedded_elf!(ELF_LOADER_TEST, "elf_loader_test.elf");
embedded_elf!(SYSCALL_TEST, "syscall_test.elf");
embedded_elf!(RING3_EXCEPTION_TEST, "ring3_exception_test.elf");
embedded_elf!(VFS_TEST, "vfs_test.elf");
embedded_elf!(FS_TREE_TEST, "fs_tree_test.elf");
embedded_elf!(THREAD_B, "thread_b.elf");
embedded_elf!(THREAD_C, "thread_c.elf");

fn kernel_main(boot_info: &'static BootInfo) -> ! {
    rust_os::init(boot_info);

    interrupts::without_interrupts(|| {
        launch_user_program("elf_loader_test", &ELF_LOADER_TEST.0);
        launch_user_program("syscall_test", &SYSCALL_TEST.0);
        launch_user_program("vfs_test", &VFS_TEST.0);
        launch_user_program("fs_tree_test", &FS_TREE_TEST.0);
        launch_user_program("ring3_exception_test", &RING3_EXCEPTION_TEST.0);
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

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    interrupts::without_interrupts(|| {
        serial_println!("{}", info);
    });
    hlt_loop();
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    rust_os::test_panic_handler(info)
}
