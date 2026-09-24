#![allow(dead_code)]

pub mod error;
pub mod file;
pub mod inode;
pub mod ramfs;
pub mod path;
pub mod filesystem;
pub mod vfs;
pub mod devices;
pub mod block_device;
pub mod ata_pio;
pub mod ext2;

pub use error::FsError;
pub use file::{File, OpenFlags, SeekWhence};
pub use filesystem::FileSystem;
pub use inode::{DirEntry, FileStat, FileType, Inode, InodeRef};
pub use path::Path;
pub use vfs::{with_vfs, Vfs, VFS};
pub use block_device::BlockDevice;
pub use ata_pio::AtaPioDisk;
pub use ext2::Ext2Fs;

use alloc::sync::Arc;

/// Initialize the filesystem stack with the ext2 disk as the VFS root.
///
/// The filesystem image is on the secondary ATA master, separate from the
/// boot medium.
pub fn init() -> Result<(), FsError> {
    let device = Arc::new(AtaPioDisk::secondary_master());
    let ext2 = Ext2Fs::mount(device)?;
    let vfs = Vfs::new(ext2);
    *VFS.lock() = Some(vfs);
    Ok(())
}
