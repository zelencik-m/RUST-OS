#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(support::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    test_main();

    loop {}
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    support::test_panic_handler(info)
}

fn assert_x86_64_elf(name: &str, image: &[u8]) {
    assert!(image.len() >= 20, "{} ELF header is truncated", name);
    assert_eq!(&image[..4], b"\x7fELF", "{} has invalid ELF magic", name);
    assert_eq!(image[4], 2, "{} is not a 64-bit ELF", name);
    assert_eq!(image[5], 1, "{} is not little-endian", name);
    assert_eq!(
        u16::from_le_bytes([image[18], image[19]]),
        0x3e,
        "{} is not an x86_64 ELF",
        name
    );
}

#[test_case]
fn embedded_user_programs_are_x86_64_elfs() {
    for (name, image) in [
        (
            "elf_loader_test",
            include_bytes!(concat!(env!("OUT_DIR"), "/elf_loader_test.elf")) as &[u8],
        ),
        (
            "syscall_test",
            include_bytes!(concat!(env!("OUT_DIR"), "/syscall_test.elf")) as &[u8],
        ),
        (
            "ring3_exception_test",
            include_bytes!(concat!(env!("OUT_DIR"), "/ring3_exception_test.elf")) as &[u8],
        ),
        (
            "vfs_test",
            include_bytes!(concat!(env!("OUT_DIR"), "/vfs_test.elf")) as &[u8],
        ),
        (
            "fs_tree_test",
            include_bytes!(concat!(env!("OUT_DIR"), "/fs_tree_test.elf")) as &[u8],
        ),
        (
            "nx_exec_test",
            include_bytes!(concat!(env!("OUT_DIR"), "/nx_exec_test.elf")) as &[u8],
        ),
        (
            "fs_ops_test",
            include_bytes!(concat!(env!("OUT_DIR"), "/fs_ops_test.elf")) as &[u8],
        ),
        (
            "tty_test",
            include_bytes!(concat!(env!("OUT_DIR"), "/tty_test.elf")) as &[u8],
        ),
        (
            "proc_child",
            include_bytes!(concat!(env!("OUT_DIR"), "/proc_child.elf")) as &[u8],
        ),
        (
            "spawn_test",
            include_bytes!(concat!(env!("OUT_DIR"), "/spawn_test.elf")) as &[u8],
        ),
        (
            "ulib_test",
            include_bytes!(concat!(env!("OUT_DIR"), "/ulib_test.elf")) as &[u8],
        ),
        (
            "hello",
            include_bytes!(concat!(env!("OUT_DIR"), "/hello.elf")) as &[u8],
        ),
        (
            "sh",
            include_bytes!(concat!(env!("OUT_DIR"), "/sh.elf")) as &[u8],
        ),
        (
            "thread_b",
            include_bytes!(concat!(env!("OUT_DIR"), "/thread_b.elf")) as &[u8],
        ),
        (
            "thread_c",
            include_bytes!(concat!(env!("OUT_DIR"), "/thread_c.elf")) as &[u8],
        ),
    ] {
        assert_x86_64_elf(name, image);
    }
}
mod support;
