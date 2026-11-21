use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use super::error::FsError;
use super::inode::{FileStat, FileType, InodeRef};

// Linux-compatible values for the subset we support.
pub const O_RDONLY: u64 = 0x0000;
pub const O_WRONLY: u64 = 0x0001;
pub const O_RDWR: u64 = 0x0002;
pub const O_ACCMODE: u64 = 0x0003;
pub const O_CREAT: u64 = 0x0040;
pub const O_TRUNC: u64 = 0x0200;
pub const O_APPEND: u64 = 0x0400;

const SUPPORTED_FLAGS: u64 = O_ACCMODE | O_CREAT | O_TRUNC | O_APPEND;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenFlags(u64);

impl OpenFlags {
    pub fn from_raw(raw: u64) -> Result<Self, FsError> {
        if raw & !SUPPORTED_FLAGS != 0 {
            return Err(FsError::InvalidFlags);
        }

        let mode = raw & O_ACCMODE;
        if mode != O_RDONLY && mode != O_WRONLY && mode != O_RDWR {
            return Err(FsError::InvalidFlags);
        }

        if (raw & O_TRUNC) != 0 && !matches!(mode, O_WRONLY | O_RDWR) {
            return Err(FsError::InvalidFlags);
        }

        Ok(Self(raw))
    }

    pub const fn read_only() -> Self {
        Self(O_RDONLY)
    }

    pub const fn write_only() -> Self {
        Self(O_WRONLY)
    }

    pub const fn read_write() -> Self {
        Self(O_RDWR)
    }

    pub fn can_read(self) -> bool {
        matches!(self.0 & O_ACCMODE, O_RDONLY | O_RDWR)
    }

    pub fn can_write(self) -> bool {
        matches!(self.0 & O_ACCMODE, O_WRONLY | O_RDWR)
    }

    pub fn create(self) -> bool {
        self.0 & O_CREAT != 0
    }

    pub fn truncate(self) -> bool {
        self.0 & O_TRUNC != 0
    }

    pub fn append(self) -> bool {
        self.0 & O_APPEND != 0
    }

    pub fn raw(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekWhence {
    Set,
    Cur,
    End,
}

impl SeekWhence {
    pub fn from_raw(raw: u64) -> Result<Self, FsError> {
        match raw {
            0 => Ok(Self::Set),
            1 => Ok(Self::Cur),
            2 => Ok(Self::End),
            _ => Err(FsError::InvalidFlags),
        }
    }
}

/// An open file: an inode, access flags and a file offset.
///
/// The offset lives behind an `Arc`, so [`File::share`] can give a child
/// process the *same* open file (Linux's "open file description"), not a
/// copy. A parent and the child it spawned then write into a shared stdout
/// one after the other, and neither one overwrites what the other wrote.
pub struct File {
    inode: InodeRef,
    offset: Arc<AtomicU64>,
    flags: OpenFlags,
}

impl File {
    pub fn new(inode: InodeRef, flags: OpenFlags) -> Result<Self, FsError> {
        if inode.file_type() == FileType::Directory && flags.can_write() {
            return Err(FsError::IsDirectory);
        }

        let offset = if flags.append() { inode.len()? } else { 0 };

        Ok(Self {
            inode,
            offset: Arc::new(AtomicU64::new(offset)),
            flags,
        })
    }

    /// Another handle to this open file, sharing its offset (like `dup`).
    /// Spawn uses it for the stdio a child inherits.
    ///
    /// Each read or write loads the offset, does the I/O, then stores the
    /// new offset. Two processes using one shared file at the same moment
    /// could interleave those steps. That's fine for how it's used: a parent
    /// shell waits while its child runs.
    pub fn share(&self) -> File {
        File {
            inode: self.inode.clone(),
            offset: self.offset.clone(),
            flags: self.flags,
        }
    }

    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, FsError> {
        if !self.flags.can_read() {
            return Err(FsError::ReadOnly);
        }
        if self.inode.file_type() == FileType::Directory {
            return Err(FsError::IsDirectory);
        }

        let offset = self.offset();
        let n = self.inode.read_at(offset, buf)?;
        self.set_offset(offset.saturating_add(n as u64));
        Ok(n)
    }

    pub fn write(&mut self, buf: &[u8]) -> Result<usize, FsError> {
        if !self.flags.can_write() {
            return Err(FsError::ReadOnly);
        }
        if self.inode.file_type() == FileType::Directory {
            return Err(FsError::IsDirectory);
        }

        let offset = if self.flags.append() {
            self.inode.len()?
        } else {
            self.offset()
        };

        let n = self.inode.write_at(offset, buf)?;
        self.set_offset(offset.saturating_add(n as u64));
        Ok(n)
    }

    pub fn seek(&mut self, offset: i64, whence: SeekWhence) -> Result<u64, FsError> {
        let base = match whence {
            SeekWhence::Set => 0i128,
            SeekWhence::Cur => self.offset() as i128,
            SeekWhence::End => self.inode.len()? as i128,
        };

        let new_offset = base.checked_add(offset as i128).ok_or(FsError::InvalidFlags)?;
        if new_offset < 0 || new_offset > u64::MAX as i128 {
            return Err(FsError::InvalidFlags);
        }

        self.set_offset(new_offset as u64);
        Ok(new_offset as u64)
    }

    pub fn truncate(&mut self, size: u64) -> Result<(), FsError> {
        if !self.flags.can_write() {
            return Err(FsError::ReadOnly);
        }
        self.inode.truncate(size)
    }

    pub fn stat(&self) -> Result<FileStat, FsError> {
        self.inode.stat()
    }

    pub fn readdir(&self) -> Result<alloc::vec::Vec<super::inode::DirEntry>, FsError> {
        self.inode.readdir()
    }

    pub fn flags(&self) -> OpenFlags {
        self.flags
    }

    pub fn offset(&self) -> u64 {
        self.offset.load(Ordering::SeqCst)
    }

    fn set_offset(&self, offset: u64) {
        self.offset.store(offset, Ordering::SeqCst);
    }

    pub fn inode(&self) -> InodeRef {
        self.inode.clone()
    }
}
