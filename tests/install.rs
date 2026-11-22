//! Installing programs into `/bin` (`rust_os::programs`) and loading them
//! back, against the real ext2 disk.
//!
//! The cases share this suite's copy of the disk, so each one uses its own
//! `p4_*` names (except the `/bin` case) and removes what it creates.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(support::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;
mod support;

use alloc::sync::Arc;
use alloc::vec::Vec;
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use rust_os::fs::ext2::{Ext2Fs, Ext2Usage};
use rust_os::fs::{error::FsError, file, AtaPioDisk, FileType, OpenFlags, Path, Vfs, VFS};
use rust_os::programs::{self, LoadError};
use rust_os::sync::IrqSafeMutex;

entry_point!(test_kernel_main);

static EXT2: IrqSafeMutex<Option<Arc<Ext2Fs>>> = IrqSafeMutex::new(None);

static HELLO: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/hello.elf"));
static THREAD_B: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/thread_b.elf"));

fn test_kernel_main(boot_info: &'static BootInfo) -> ! {
    rust_os::init(boot_info);

    // As in tests/ext2.rs: remount to get a typed handle for the counters,
    // and make that mount the VFS root, so one driver talks to the disk.
    let fs = Ext2Fs::mount(Arc::new(AtaPioDisk::secondary_master()))
        .expect("failed to mount the ext2 test disk");
    *VFS.lock() = Some(Vfs::new(fs.clone()));
    *EXT2.lock() = Some(fs);

    test_main();
    rust_os::hlt_loop()
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    support::test_panic_handler(info)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn fs() -> Arc<Ext2Fs> {
    EXT2.lock().clone().expect("ext2 test mount missing")
}

fn vfs() -> Vfs {
    Vfs::new(fs())
}

fn abs(path: &str) -> Path {
    Path::new_absolute(path).unwrap()
}

fn usage() -> Ext2Usage {
    fs().usage().expect("reading ext2 counters failed")
}

fn read_all(path: &str) -> Vec<u8> {
    let mut handle = vfs()
        .open(&Path::root(), path, OpenFlags::read_only())
        .unwrap_or_else(|err| panic!("open {}: {:?}", path, err));
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = handle.read(&mut chunk).unwrap();
        if n == 0 {
            return bytes;
        }
        bytes.extend_from_slice(&chunk[..n]);
    }
}

fn size(path: &str) -> u64 {
    vfs().stat(&Path::root(), path).unwrap().size
}

fn write_text(path: &str, text: &str) {
    let flags = OpenFlags::from_raw(file::O_CREAT | file::O_WRONLY | file::O_TRUNC).unwrap();
    let mut handle = vfs().open(&Path::root(), path, flags).unwrap();
    assert_eq!(handle.write(text.as_bytes()).unwrap(), text.len());
}

/// `len` bytes of a position-dependent pattern.
fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8 ^ seed).collect()
}

fn remove(path: &str) {
    vfs().unlink(&Path::root(), path).unwrap();
}

fn remove_dir(path: &str) {
    vfs().rmdir(&Path::root(), path).unwrap();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test_case]
fn install_creates_bin_and_writes_every_program() {
    programs::install(&[("hello", HELLO), ("thread_b", THREAD_B)]).expect("install failed");

    let bin = vfs().stat(&Path::root(), programs::BIN_DIR).unwrap();
    assert_eq!(bin.file_type, FileType::Directory);
    // hello is larger than the 12 direct blocks (1 KiB each) can hold, so
    // this also goes through a single-indirect block.
    assert!(HELLO.len() > 12 * 1024, "hello.elf is only {} bytes", HELLO.len());
    for (name, image) in [("/bin/hello", HELLO), ("/bin/thread_b", THREAD_B)] {
        assert_eq!(size(name), image.len() as u64, "{} has the wrong size", name);
        assert!(read_all(name) == image, "{} differs from the embedded ELF", name);
    }
}

#[test_case]
fn load_reads_back_an_installed_program() {
    programs::install(&[("hello", HELLO)]).unwrap();
    let loaded = programs::load(&abs("/bin/hello")).expect("load failed");
    assert!(loaded == HELLO, "load returned different bytes");
}

#[test_case]
fn reinstalling_rewrites_files_in_full() {
    programs::install_in("/p4_bin", &[]).unwrap();
    assert_eq!(
        vfs().stat(&Path::root(), "/p4_bin").unwrap().file_type,
        FileType::Directory,
        "install_in creates the directory even with no programs"
    );
    write_text("/p4_bin/keep", "not a program");
    let before = usage();

    // 40 KiB: data blocks plus a single-indirect block.
    let big = pattern(40 * 1024, 0x5a);
    programs::install_in("/p4_bin", &[("prog", &big)]).unwrap();
    assert!(read_all("/p4_bin/prog") == big);
    assert!(before.free_blocks - usage().free_blocks >= 41);

    // Shorter: no stale tail, and the blocks the old version used are free
    // again (one data block left).
    let small = pattern(100, 0xa5);
    programs::install_in("/p4_bin", &[("prog", &small)]).unwrap();
    assert_eq!(size("/p4_bin/prog"), 100);
    assert!(read_all("/p4_bin/prog") == small);
    let after = usage();
    assert_eq!(after.free_blocks, before.free_blocks - 1);
    assert_eq!(after.free_inodes, before.free_inodes - 1);

    // The same content again changes nothing.
    programs::install_in("/p4_bin", &[("prog", &small)]).unwrap();
    assert_eq!(usage(), after);
    assert!(read_all("/p4_bin/prog") == small);

    // Files that aren't being installed are left alone.
    assert!(read_all("/p4_bin/keep") == b"not a program");

    remove("/p4_bin/prog");
    remove("/p4_bin/keep");
    remove_dir("/p4_bin");
}

#[test_case]
fn installing_into_a_regular_file_fails() {
    write_text("/p4_file", "in the way");
    assert_eq!(
        programs::install_in("/p4_file", &[("x", HELLO)]),
        Err(FsError::NotDirectory)
    );
    assert!(read_all("/p4_file") == b"in the way", "the file was changed");
    remove("/p4_file");
}

#[test_case]
fn load_rejects_what_is_not_a_program() {
    vfs().mkdir(&Path::root(), "/p4_dir").unwrap();
    write_text("/p4_dir/text", "#!/bin/sh\n");

    assert_eq!(programs::load(&abs("/p4_dir")), Err(LoadError::NotRegular));
    assert_eq!(programs::load(&abs("/p4_dir/text")), Err(LoadError::NotElf));
    assert_eq!(
        programs::load(&abs("/p4_dir/missing")),
        Err(LoadError::Fs(FsError::NotFound))
    );

    remove("/p4_dir/text");
    remove_dir("/p4_dir");
}
