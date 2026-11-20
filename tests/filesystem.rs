#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(support::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::sync::Arc;
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use rust_os::fs::{
    error::FsError, file, path::Path, ramfs::RamFs, FileSystem, FileType, OpenFlags, SeekWhence,
    Vfs,
};

entry_point!(test_kernel_main);

fn test_kernel_main(boot_info: &'static BootInfo) -> ! {
    // Filesystem operations allocate, so use the normal boot path to initialize
    // the kernel heap before running this integration-test harness.
    rust_os::init(boot_info);
    test_main();
    rust_os::hlt_loop()
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    support::test_panic_handler(info)
}

fn ram_vfs() -> Vfs {
    Vfs::new(Arc::new(RamFs::new()))
}

#[test_case]
fn paths_normalize_and_resolve_against_cwd() {
    let cwd = Path::new_absolute("/home/user/./docs").unwrap();
    assert_eq!(cwd.as_str(), "/home/user/docs");

    let resolved = Path::resolve(&cwd, "../notes//today.txt").unwrap();
    assert_eq!(resolved.as_str(), "/home/user/notes/today.txt");
    assert_eq!(resolved.file_name(), Some("today.txt"));
    assert_eq!(resolved.parent().as_str(), "/home/user/notes");
    assert_eq!(
        Path::resolve(&cwd, "/var/log/../tmp").unwrap().as_str(),
        "/var/tmp"
    );
}

#[test_case]
fn paths_reject_relative_absolute_inputs_and_nul_bytes() {
    assert_eq!(Path::new_absolute("relative"), Err(FsError::InvalidPath));
    assert_eq!(Path::resolve(&Path::root(), ""), Err(FsError::InvalidPath));
    assert_eq!(Path::new_absolute("/bad\0name"), Err(FsError::InvalidPath));
    assert!(Path::root().is_root());
    assert_eq!(Path::root().file_name(), None);
}

#[test_case]
fn ramfs_supports_sparse_io_truncate_and_stat() {
    let fs = RamFs::new();
    let root = fs.root();
    let file = root.create_child("data", FileType::Regular).unwrap();

    assert_eq!(file.write_at(3, b"xy").unwrap(), 2);
    let mut bytes = [0xff; 5];
    assert_eq!(file.read_at(0, &mut bytes).unwrap(), 5);
    assert_eq!(&bytes, b"\0\0\0xy");
    assert_eq!(file.stat().unwrap().size, 5);

    file.truncate(2).unwrap();
    assert_eq!(file.len().unwrap(), 2);
    file.truncate(5).unwrap();
    let mut grown = [0xff; 5];
    assert_eq!(file.read_at(0, &mut grown).unwrap(), 5);
    assert_eq!(&grown, b"\0\0\0\0\0");
}

#[test_case]
fn ramfs_validates_children_and_directory_operations() {
    let root = RamFs::new().root();
    root.create_child("child", FileType::Directory).unwrap();
    assert_eq!(
        root.create_child("child", FileType::Regular).err(),
        Some(FsError::AlreadyExists)
    );
    assert_eq!(
        root.create_child("bad/name", FileType::Regular).err(),
        Some(FsError::InvalidPath)
    );
    assert_eq!(root.lookup("").err(), Some(FsError::NotFound));
    assert_eq!(root.lookup("missing").err(), Some(FsError::NotFound));

    let entries = root.readdir().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "child");
    assert_eq!(entries[0].file_type, FileType::Directory);
    assert_eq!(root.read_at(0, &mut [0; 1]), Err(FsError::IsDirectory));
    assert_eq!(root.write_at(0, b"x"), Err(FsError::IsDirectory));
    assert_eq!(root.truncate(0), Err(FsError::IsDirectory));
}

#[test_case]
fn vfs_create_relative_lookup_and_directory_listing() {
    let vfs = ram_vfs();
    let cwd = Path::root();
    vfs.mkdir(&cwd, "home").unwrap();
    vfs.mkdir(&cwd, "/home/user").unwrap();

    let flags = OpenFlags::from_raw(file::O_CREAT | file::O_RDWR).unwrap();
    let mut created = vfs.open(&cwd, "/home/user/note.txt", flags).unwrap();
    assert_eq!(created.write(b"hello").unwrap(), 5);
    assert_eq!(created.stat().unwrap().size, 5);

    let user_cwd = Path::new_absolute("/home/user").unwrap();
    let mut reopened = vfs
        .open(&user_cwd, "../user/./note.txt", OpenFlags::read_only())
        .unwrap();
    let mut data = [0; 8];
    assert_eq!(reopened.read(&mut data).unwrap(), 5);
    assert_eq!(&data[..5], b"hello");
    assert_eq!(reopened.read(&mut data).unwrap(), 0);

    let entries = vfs.readdir(&cwd, "/home/user").unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "note.txt");
    assert_eq!(entries[0].file_type, FileType::Regular);
}

#[test_case]
fn file_enforces_access_append_seek_and_truncate_flags() {
    let vfs = ram_vfs();
    let cwd = Path::root();
    let create = OpenFlags::from_raw(file::O_CREAT | file::O_RDWR).unwrap();
    let mut initial = vfs.open(&cwd, "/log", create).unwrap();
    initial.write(b"abc").unwrap();

    let mut readonly = vfs.open(&cwd, "/log", OpenFlags::read_only()).unwrap();
    assert_eq!(readonly.write(b"x"), Err(FsError::ReadOnly));
    assert_eq!(readonly.truncate(0), Err(FsError::ReadOnly));
    let offset_before_failed_seek = readonly.offset();
    assert_eq!(
        readonly.seek(-1, SeekWhence::Set),
        Err(FsError::InvalidFlags)
    );
    assert_eq!(readonly.offset(), offset_before_failed_seek);
    assert_eq!(readonly.seek(0, SeekWhence::End).unwrap(), 3);
    assert_eq!(readonly.seek(-1, SeekWhence::Cur).unwrap(), 2);

    let append_flags = OpenFlags::from_raw(file::O_WRONLY | file::O_APPEND).unwrap();
    let mut append = vfs.open(&cwd, "/log", append_flags).unwrap();
    assert_eq!(append.offset(), 3);

    // Another open handle can extend the file after the append handle was
    // created. Append writes must consult the current EOF each time.
    let mut concurrent_writer = vfs.open(&cwd, "/log", OpenFlags::read_write()).unwrap();
    concurrent_writer.seek(0, SeekWhence::End).unwrap();
    concurrent_writer.write(b"d").unwrap();

    append.seek(0, SeekWhence::Set).unwrap();
    append.write(b"!").unwrap();
    assert_eq!(append.offset(), 5);

    let mut verify = vfs.open(&cwd, "/log", OpenFlags::read_only()).unwrap();
    let mut contents = [0; 5];
    assert_eq!(verify.read(&mut contents).unwrap(), 5);
    assert_eq!(&contents, b"abcd!");

    let truncate_flags = OpenFlags::from_raw(file::O_WRONLY | file::O_TRUNC).unwrap();
    let truncated = vfs.open(&cwd, "/log", truncate_flags).unwrap();
    assert_eq!(truncated.stat().unwrap().size, 0);
}

#[test_case]
fn vfs_reports_expected_errors_and_open_flag_validation() {
    let vfs = ram_vfs();
    let cwd = Path::root();

    assert_eq!(OpenFlags::from_raw(0x8000), Err(FsError::InvalidFlags));
    assert_eq!(
        OpenFlags::from_raw(file::O_ACCMODE),
        Err(FsError::InvalidFlags)
    );
    assert_eq!(
        OpenFlags::from_raw(file::O_RDONLY | file::O_TRUNC),
        Err(FsError::InvalidFlags)
    );
    assert_eq!(SeekWhence::from_raw(3), Err(FsError::InvalidFlags));
    assert_eq!(
        vfs.open(&cwd, "/absent", OpenFlags::read_only()).err(),
        Some(FsError::NotFound)
    );
    assert_eq!(vfs.mkdir(&cwd, "/",), Err(FsError::AlreadyExists));
    assert_eq!(vfs.mkdir(&cwd, "/missing/child"), Err(FsError::NotFound));

    vfs.mkdir(&cwd, "/dir").unwrap();
    assert_eq!(vfs.mkdir(&cwd, "/dir"), Err(FsError::AlreadyExists));
    assert_eq!(
        vfs.open(&cwd, "/dir", OpenFlags::write_only()).err(),
        Some(FsError::IsDirectory)
    );
    assert_eq!(
        vfs.readdir(&cwd, "/dir/nope").err(),
        Some(FsError::NotFound)
    );
}

#[test_case]
fn ramfs_unlink_and_rmdir_remove_entries() {
    let root = RamFs::new().root();
    root.create_child("file", FileType::Regular).unwrap();
    root.create_child("dir", FileType::Directory).unwrap();

    root.unlink("file").unwrap();
    assert_eq!(root.lookup("file").err(), Some(FsError::NotFound));
    root.rmdir("dir").unwrap();
    assert_eq!(root.lookup("dir").err(), Some(FsError::NotFound));
    assert!(root.readdir().unwrap().is_empty());

    // A removed name can be reused, and the new object starts empty.
    let again = root.create_child("file", FileType::Regular).unwrap();
    assert_eq!(again.len().unwrap(), 0);
}

#[test_case]
fn ramfs_unlink_and_rmdir_enforce_names_and_types() {
    let root = RamFs::new().root();
    let file = root.create_child("file", FileType::Regular).unwrap();
    let dir = root.create_child("dir", FileType::Directory).unwrap();
    dir.create_child("inner", FileType::Regular).unwrap();

    for bad in ["", ".", "..", "a/b"] {
        assert_eq!(root.unlink(bad), Err(FsError::InvalidPath), "unlink {:?}", bad);
        assert_eq!(root.rmdir(bad), Err(FsError::InvalidPath), "rmdir {:?}", bad);
    }
    assert_eq!(root.unlink("missing"), Err(FsError::NotFound));
    assert_eq!(root.rmdir("missing"), Err(FsError::NotFound));
    assert_eq!(root.unlink("dir"), Err(FsError::IsDirectory));
    assert_eq!(root.rmdir("file"), Err(FsError::NotDirectory));
    assert_eq!(root.rmdir("dir"), Err(FsError::NotEmpty));
    // unlink/rmdir *on* a non-directory.
    assert_eq!(file.unlink("x"), Err(FsError::NotDirectory));
    assert_eq!(file.rmdir("x"), Err(FsError::NotDirectory));

    // Failed calls must not have removed anything.
    assert_eq!(root.readdir().unwrap().len(), 2);
    assert_eq!(dir.readdir().unwrap().len(), 1);
}

#[test_case]
fn vfs_unlink_and_rmdir_resolve_paths() {
    let vfs = ram_vfs();
    let root = Path::root();
    vfs.mkdir(&root, "/a").unwrap();
    vfs.mkdir(&root, "/a/b").unwrap();
    let create = OpenFlags::from_raw(file::O_CREAT | file::O_WRONLY).unwrap();
    vfs.open(&root, "/a/f", create).unwrap();

    // Relative to a cwd, with `..` and `.` normalized away.
    let cwd = Path::new_absolute("/a/b").unwrap();
    vfs.unlink(&cwd, "../f").unwrap();
    assert_eq!(vfs.stat(&root, "/a/f").err(), Some(FsError::NotFound));
    vfs.rmdir(&cwd, "../b/.").unwrap();
    assert_eq!(vfs.stat(&root, "/a/b").err(), Some(FsError::NotFound));
    vfs.rmdir(&root, "a").unwrap();
    assert!(vfs.readdir(&root, "/").unwrap().is_empty());
}

#[test_case]
fn vfs_unlink_and_rmdir_report_errors() {
    let vfs = ram_vfs();
    let root = Path::root();
    vfs.mkdir(&root, "/dir").unwrap();
    let create = OpenFlags::from_raw(file::O_CREAT | file::O_WRONLY).unwrap();
    vfs.open(&root, "/dir/file", create).unwrap();

    assert_eq!(vfs.rmdir(&root, "/"), Err(FsError::Busy));
    assert_eq!(vfs.rmdir(&root, "/dir/.."), Err(FsError::Busy));
    assert_eq!(vfs.unlink(&root, "/"), Err(FsError::IsDirectory));
    assert_eq!(vfs.unlink(&root, "/missing"), Err(FsError::NotFound));
    assert_eq!(vfs.unlink(&root, "/missing/x"), Err(FsError::NotFound));
    assert_eq!(vfs.rmdir(&root, "/missing"), Err(FsError::NotFound));
    assert_eq!(vfs.unlink(&root, "/dir/file/x"), Err(FsError::NotDirectory));
    assert_eq!(vfs.unlink(&root, "/dir"), Err(FsError::IsDirectory));
    assert_eq!(vfs.rmdir(&root, "/dir/file"), Err(FsError::NotDirectory));
    assert_eq!(vfs.rmdir(&root, "/dir"), Err(FsError::NotEmpty));
    assert_eq!(vfs.unlink(&root, ""), Err(FsError::InvalidPath));
    assert_eq!(vfs.rmdir(&root, ""), Err(FsError::InvalidPath));

    assert!(vfs.stat(&root, "/dir/file").is_ok());
}

#[test_case]
fn ramfs_open_file_survives_unlink() {
    // POSIX semantics: unlink removes the name, and the data lives on until
    // the last handle is gone. RamFs gets this for free from `Arc`. ext2 does
    // not (no orphan list), which is a documented limitation.
    let vfs = ram_vfs();
    let root = Path::root();
    let create = OpenFlags::from_raw(file::O_CREAT | file::O_RDWR).unwrap();
    let mut handle = vfs.open(&root, "/tmpfile", create).unwrap();
    handle.write(b"still here").unwrap();

    vfs.unlink(&root, "/tmpfile").unwrap();
    assert_eq!(vfs.stat(&root, "/tmpfile").err(), Some(FsError::NotFound));

    handle.seek(0, SeekWhence::Set).unwrap();
    let mut buf = [0; 10];
    assert_eq!(handle.read(&mut buf).unwrap(), 10);
    assert_eq!(&buf, b"still here");
}

mod support;
