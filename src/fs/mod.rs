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
pub use ext2::{Ext2Fs, Ext2Geometry};

use alloc::sync::Arc;

use crate::sync::{SleepLock, SleepLockGuard};

/// Serializes all filesystem access: every VFS operation ([`with_vfs`]) and
/// every regular-file operation ([`File`]'s methods). The ext2 driver has no
/// locks of its own, so this is what keeps two threads from changing it at
/// once (e.g. both taking the same free block).
///
/// It's a [`SleepLock`]: disk I/O runs with interrupts on and can be
/// preempted (the boot screen animates while the disk is being written),
/// and a thread that wants the filesystem while another uses it sleeps. So
/// never use files or the VFS while holding an `IrqSafeMutex` such as
/// PROCESS_MANAGER: syscalls take a shared handle to the open file
/// (`File::share`) and release PROCESS_MANAGER first.
pub static FS_LOCK: SleepLock = SleepLock::new();

/// Take [`FS_LOCK`] (re-entrant).
pub fn lock() -> SleepLockGuard<'static> {
    FS_LOCK.lock()
}

/// Initialize the filesystem stack with the ext2 disk as the VFS root.
///
/// The filesystem image is on the secondary ATA master, separate from the
/// boot medium.
pub fn init() -> Result<Ext2Geometry, FsError> {
    let device = Arc::new(AtaPioDisk::secondary_master());
    let ext2 = Ext2Fs::mount(device)?;
    let geometry = ext2.geometry();
    let vfs = Vfs::new(ext2);
    *VFS.lock() = Some(vfs);
    Ok(geometry)
}
