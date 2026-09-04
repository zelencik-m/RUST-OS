#![allow(dead_code)]

pub mod error;
pub mod file;
pub mod inode;

pub use error::FsError;
pub use file::{File, OpenFlags, SeekWhence};
// pub use filesystem::FileSystem;
pub use inode::{DirEntry, FileStat, FileType, Inode, InodeRef};
// pub use path::Path;
// pub use vfs::{with_vfs, Vfs, VFS};

pub fn init() {
    // vfs::init();
}
