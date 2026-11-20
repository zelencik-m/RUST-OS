//! unlink/rmdir against the real ext2 disk (ATA secondary master).
//!
//! The cargo runner gives every test suite a fresh copy of `ext2.img`, but
//! the cases in this suite share that copy. So every case uses its own `p1_*`
//! names and removes everything it creates. Most cases also check that the
//! free block and inode counters, and the group used-dirs counter, return to
//! their starting values.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(support::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use rust_os::fs::ext2::{Ext2Fs, Ext2Usage};
use rust_os::fs::{
    error::FsError, file, AtaPioDisk, FileSystem, FileType, OpenFlags, Path, Vfs, VFS,
};
use rust_os::sync::IrqSafeMutex;

entry_point!(test_kernel_main);

static EXT2: IrqSafeMutex<Option<Arc<Ext2Fs>>> = IrqSafeMutex::new(None);

fn test_kernel_main(boot_info: &'static BootInfo) -> ! {
    rust_os::init(boot_info);

    // `init` already mounted the disk as the VFS root, but only behind
    // `dyn FileSystem`. Mount it again to get a typed handle for the
    // inspection helpers, and install that mount as the VFS root. That
    // drops the first instance, so only one driver ever talks to the disk.
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

/// Current counters. Also checks that the superblock totals agree with the
/// group descriptors (alloc/free must always update both).
fn usage() -> Ext2Usage {
    let usage = fs().usage().expect("reading ext2 counters failed");
    assert_eq!(
        usage.free_blocks, usage.group_free_blocks,
        "superblock free blocks != sum of group free blocks"
    );
    assert_eq!(
        usage.free_inodes, usage.group_free_inodes,
        "superblock free inodes != sum of group free inodes"
    );
    usage
}

fn ino(path: &str) -> u32 {
    fs().resolve_ino(&abs(path))
        .unwrap_or_else(|err| panic!("resolve {}: {:?}", path, err))
}

fn links(path: &str) -> u16 {
    fs().read_disk_inode(ino(path)).unwrap().links_count
}

/// A deleted inode must look deleted on disk, with no links and no data. It
/// also needs a timestamp-like `dtime`: e2fsck reports a `dtime` of 0 as a
/// "deleted inode has zero dtime" error, and reads one below
/// `s_inodes_count` as an ext3 orphan-list link ("corrupted orphan linked
/// list").
fn assert_inode_released(ino: u32) {
    let inode = fs().read_disk_inode(ino).unwrap();
    assert_eq!(inode.links_count, 0, "inode {} still has links", ino);
    assert!(
        inode.dtime >= fs().inodes_count(),
        "inode {} was freed with dtime {}; use a Unix-timestamp-like value",
        ino,
        inode.dtime
    );
    assert_eq!(inode.size, 0, "inode {} still has a size", ino);
    assert!(
        inode.block.iter().all(|&block| block == 0),
        "inode {} still points at blocks: {:?}",
        ino,
        inode.block
    );
}

/// Create `path` and fill it with `len` bytes of a position-dependent pattern.
fn write_file(path: &str, len: usize) {
    let flags = OpenFlags::from_raw(file::O_CREAT | file::O_WRONLY | file::O_TRUNC).unwrap();
    let mut handle = vfs().open(&Path::root(), path, flags).unwrap();
    let mut chunk = [0u8; 4096];
    let mut written = 0;
    while written < len {
        let n = core::cmp::min(chunk.len(), len - written);
        for (i, byte) in chunk[..n].iter_mut().enumerate() {
            *byte = ((written + i) % 251) as u8;
        }
        assert_eq!(handle.write(&chunk[..n]).unwrap(), n);
        written += n;
    }
}

fn write_text(path: &str, text: &str) {
    let flags = OpenFlags::from_raw(file::O_CREAT | file::O_WRONLY | file::O_TRUNC).unwrap();
    let mut handle = vfs().open(&Path::root(), path, flags).unwrap();
    assert_eq!(handle.write(text.as_bytes()).unwrap(), text.len());
}

fn read_text(path: &str) -> String {
    let mut handle = vfs().open(&Path::root(), path, OpenFlags::read_only()).unwrap();
    let mut buf = [0u8; 256];
    let n = handle.read(&mut buf).unwrap();
    String::from(core::str::from_utf8(&buf[..n]).unwrap())
}

/// Sorted names in a directory, without `.` and `..`.
fn names(path: &str) -> Vec<String> {
    let mut names: Vec<String> = vfs()
        .readdir(&Path::root(), path)
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .filter(|name| name != "." && name != "..")
        .collect();
    names.sort();
    names
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test_case]
fn baseline_counters_are_consistent() {
    // Sanity check for the harness itself (passes before phase 1 is done).
    let usage = usage();
    assert!(usage.free_blocks > 0 && usage.free_inodes > 0);
    assert!(usage.used_dirs >= 1, "the root directory is a used dir");
    assert_eq!(fs().root().file_type(), FileType::Directory);
}

#[test_case]
fn unlink_small_file_restores_counters() {
    let before = usage();
    write_file("/p1_small", 100);
    let file_ino = ino("/p1_small");
    let created = usage();
    assert_eq!(created.free_inodes, before.free_inodes - 1);
    assert_eq!(created.free_blocks, before.free_blocks - 1);

    vfs().unlink(&Path::root(), "/p1_small").unwrap();

    assert_eq!(
        vfs().stat(&Path::root(), "/p1_small").err(),
        Some(FsError::NotFound)
    );
    assert_eq!(usage(), before);
    assert_inode_released(file_ino);
}

#[test_case]
fn unlink_large_file_frees_indirect_blocks() {
    // 1 KiB blocks: 12 direct + 256 single-indirect + the rest through the
    // double-indirect tree. 300 data blocks therefore also need 1 single
    // indirect block, 1 double-indirect block and 1 indirect block under it.
    let before = usage();
    write_file("/p1_large", 300 * 1024);
    let file_ino = ino("/p1_large");
    assert!(
        before.free_blocks - usage().free_blocks >= 300,
        "the 300 KiB file should occupy at least 300 blocks"
    );

    vfs().unlink(&Path::root(), "/p1_large").unwrap();

    assert_eq!(usage(), before, "data and indirect blocks must all be freed");
    assert_inode_released(file_ino);
}

#[test_case]
fn rmdir_empty_directory_restores_counters_and_parent_links() {
    let before = usage();
    let root_links = links("/");

    vfs().mkdir(&Path::root(), "/p1_dir").unwrap();
    let dir_ino = ino("/p1_dir");
    assert_eq!(usage().used_dirs, before.used_dirs + 1);
    assert_eq!(links("/"), root_links + 1, "child's `..` links the parent");
    assert_eq!(links("/p1_dir"), 2, "`.` plus the parent's entry");

    vfs().rmdir(&Path::root(), "/p1_dir").unwrap();

    assert_eq!(
        vfs().stat(&Path::root(), "/p1_dir").err(),
        Some(FsError::NotFound)
    );
    assert_eq!(links("/"), root_links, "removing the child drops its `..` link");
    assert_eq!(usage(), before, "used_dirs, inode and block must all come back");
    assert_inode_released(dir_ino);
}

#[test_case]
fn rmdir_nested_directory_updates_its_parent() {
    let before = usage();
    vfs().mkdir(&Path::root(), "/p1_nest").unwrap();
    vfs().mkdir(&Path::root(), "/p1_nest/child").unwrap();
    assert_eq!(links("/p1_nest"), 3);

    vfs().rmdir(&abs("/p1_nest"), "child").unwrap();
    assert_eq!(links("/p1_nest"), 2);
    assert!(names("/p1_nest").is_empty());

    vfs().rmdir(&Path::root(), "/p1_nest").unwrap();
    assert_eq!(usage(), before);
}

#[test_case]
fn directory_records_stay_valid_in_any_removal_order() {
    // Long names (rec_len 164) fit 6 records per 1 KiB block, so 40 files
    // span 7 blocks. Block 0 holds `.`, `..` and files 0-5, so files 6, 12,
    // 18, ... are the *first* record of their block. Phase A removes every
    // third file, which includes all of those, and so takes the
    // `inode = 0` path. The other removals take the `rec_len` merge path,
    // including merges into an `inode = 0` record.
    const COUNT: usize = 40;
    let before = usage();
    vfs().mkdir(&Path::root(), "/p1_many").unwrap();

    let name = |i: usize| format!("{:03}-{}", i, "x".repeat(150));
    for i in 0..COUNT {
        write_text(&format!("/p1_many/{}", name(i)), &format!("file {}", i));
    }
    let dir_size = vfs().stat(&Path::root(), "/p1_many").unwrap().size;
    assert!(dir_size >= 7 * 1024, "directory should span 7+ blocks, is {}", dir_size);

    let mut remaining: Vec<usize> = (0..COUNT).collect();
    let check = |remaining: &[usize]| {
        let expected: Vec<String> = remaining.iter().map(|&i| name(i)).collect();
        assert_eq!(names("/p1_many"), expected);
        // Every surviving record must still point at its own inode.
        for &i in remaining {
            assert_eq!(read_text(&format!("/p1_many/{}", name(i))), format!("file {}", i));
        }
    };

    // Phase A: every third file, front to back.
    for i in (0..COUNT).step_by(3) {
        vfs().unlink(&Path::root(), &format!("/p1_many/{}", name(i))).unwrap();
        remaining.retain(|&r| r != i);
    }
    check(&remaining);

    // A new entry still fits into the directory after removals.
    write_text("/p1_many/new-entry", "new");
    assert_eq!(read_text("/p1_many/new-entry"), "new");
    vfs().unlink(&abs("/p1_many"), "new-entry").unwrap();
    check(&remaining);

    // Phase B: the rest, back to front.
    while let Some(i) = remaining.pop() {
        vfs().unlink(&Path::root(), &format!("/p1_many/{}", name(i))).unwrap();
        if remaining.len() % 5 == 0 {
            check(&remaining);
        }
    }
    assert!(names("/p1_many").is_empty());

    // Some blocks now only contain a deleted (`inode = 0`) record. They
    // still count as empty, and rmdir must free all of the directory's blocks.
    vfs().rmdir(&Path::root(), "/p1_many").unwrap();
    assert_eq!(usage(), before);
}

#[test_case]
fn failed_unlink_and_rmdir_leave_the_disk_untouched() {
    vfs().mkdir(&Path::root(), "/p1_err").unwrap();
    vfs().mkdir(&Path::root(), "/p1_err/sub").unwrap();
    write_text("/p1_err/f", "keep me");
    write_text("/p1_err/sub/g", "keep me too");
    let before = usage();
    let root = Path::root();
    let v = vfs();

    assert_eq!(v.rmdir(&root, "/"), Err(FsError::Busy));
    assert_eq!(v.unlink(&root, "/"), Err(FsError::IsDirectory));
    assert_eq!(v.unlink(&root, "/p1_err/sub"), Err(FsError::IsDirectory));
    assert_eq!(v.rmdir(&root, "/p1_err/f"), Err(FsError::NotDirectory));
    assert_eq!(v.rmdir(&root, "/p1_err"), Err(FsError::NotEmpty));
    assert_eq!(v.rmdir(&root, "/p1_err/sub"), Err(FsError::NotEmpty));
    assert_eq!(v.unlink(&root, "/p1_err/missing"), Err(FsError::NotFound));
    assert_eq!(v.rmdir(&root, "/p1_err/missing"), Err(FsError::NotFound));
    assert_eq!(v.unlink(&root, "/p1_err/f/x"), Err(FsError::NotDirectory));

    // Inode level: `.` and `..` really exist as ext2 records, but they must
    // never be removable by name.
    let dir = fs().root().lookup("p1_err").unwrap();
    for bad in ["", ".", "..", "a/b"] {
        assert_eq!(dir.unlink(bad), Err(FsError::InvalidPath), "unlink {:?}", bad);
        assert_eq!(dir.rmdir(bad), Err(FsError::InvalidPath), "rmdir {:?}", bad);
    }
    let file = dir.lookup("f").unwrap();
    assert_eq!(file.unlink("x"), Err(FsError::NotDirectory));
    assert_eq!(file.rmdir("x"), Err(FsError::NotDirectory));

    assert_eq!(usage(), before);
    assert_eq!(names("/p1_err"), ["f", "sub"]);
    assert_eq!(read_text("/p1_err/f"), "keep me");
    assert_eq!(read_text("/p1_err/sub/g"), "keep me too");
    assert_eq!(links("/p1_err"), 3);

    // Clean up.
    v.unlink(&root, "/p1_err/sub/g").unwrap();
    v.rmdir(&root, "/p1_err/sub").unwrap();
    v.unlink(&root, "/p1_err/f").unwrap();
    v.rmdir(&root, "/p1_err").unwrap();
}

#[test_case]
fn removed_names_can_be_recreated() {
    let before = usage();

    write_text("/p1_again", "first");
    vfs().unlink(&Path::root(), "/p1_again").unwrap();
    write_text("/p1_again", "second");
    assert_eq!(read_text("/p1_again"), "second");
    vfs().unlink(&Path::root(), "/p1_again").unwrap();

    vfs().mkdir(&Path::root(), "/p1_againdir").unwrap();
    write_text("/p1_againdir/inner", "x");
    vfs().unlink(&Path::root(), "/p1_againdir/inner").unwrap();
    vfs().rmdir(&Path::root(), "/p1_againdir").unwrap();
    vfs().mkdir(&Path::root(), "/p1_againdir").unwrap();
    assert!(names("/p1_againdir").is_empty());
    assert_eq!(links("/p1_againdir"), 2);
    vfs().rmdir(&Path::root(), "/p1_againdir").unwrap();

    assert_eq!(usage(), before);
}

mod support;
