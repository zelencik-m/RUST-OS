// src/fs/filesystem.rs
use super::inode::InodeRef;

/// A mounted filesystem implementation.
///
/// The VFS only needs a root inode from a filesystem. All path traversal and
/// file operations happen through the inode interface.
pub trait FileSystem: Send + Sync {
    fn root(&self) -> InodeRef;
}
