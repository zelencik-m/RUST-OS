use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::error::FsError;

pub type InodeRef = Arc<dyn Inode + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    Regular,
    Directory,
    Device,
}

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub file_type: FileType,
}

#[derive(Debug, Clone, Copy)]
pub struct FileStat {
    pub size: u64,
    pub file_type: FileType,
}

/// Filesystem object interface. Filesystems provide concrete inode
/// implementations behind this trait; the VFS and syscall layers never need
/// to know how the object is stored.
pub trait Inode: Send + Sync {
    fn file_type(&self) -> FileType;

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, FsError>;

    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<usize, FsError>;

    fn lookup(&self, name: &str) -> Result<InodeRef, FsError>;

    fn create_child(&self, name: &str, file_type: FileType) -> Result<InodeRef, FsError>;

    fn readdir(&self) -> Result<Vec<DirEntry>, FsError>;

    fn truncate(&self, size: u64) -> Result<(), FsError>;

    fn len(&self) -> Result<u64, FsError>;

    /// Remove the non-directory entry `name` from this directory.
    ///
    /// Contract shared by every filesystem:
    /// - `self` is not a directory → `NotDirectory`
    /// - `name` is empty, `.`, `..` or contains `/` → `InvalidPath`
    /// - no such entry → `NotFound`
    /// - the entry is a directory → `IsDirectory` (use `rmdir`)
    ///
    /// The default is for inodes that can never be directories (devices).
    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::NotDirectory)
    }

    /// Remove the empty directory `name` from this directory.
    ///
    /// Same contract as [`Inode::unlink`], except:
    /// - the entry is not a directory → `NotDirectory`
    /// - the directory has entries other than `.`/`..` → `NotEmpty`
    fn rmdir(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::NotDirectory)
    }

    fn stat(&self) -> Result<FileStat, FsError> {
        Ok(FileStat {
            size: self.len()?,
            file_type: self.file_type(),
        })
    }
}
