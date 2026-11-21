use alloc::sync::Arc;

use super::error::FsError;
use super::file::{File, OpenFlags};
use super::inode::{DirEntry, FileStat, FileType, Inode, InodeRef};

#[derive(Debug, Clone, Copy)]
enum ConsoleKind {
    Stdin,
    Stdout,
    Stderr,
}

struct ConsoleInode {
    kind: ConsoleKind,
}

impl Inode for ConsoleInode {
    fn file_type(&self) -> FileType {
        FileType::Device
    }

    fn read_at(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        match self.kind {
            // Blocks until the keyboard has produced input, or the reading
            // process is killed.
            ConsoleKind::Stdin => crate::tty::read(buf).map_err(|_| FsError::Interrupted),
            ConsoleKind::Stdout | ConsoleKind::Stderr => Err(FsError::ReadOnly),
        }
    }

    fn write_at(&self, _offset: u64, buf: &[u8]) -> Result<usize, FsError> {
        match self.kind {
            ConsoleKind::Stdin => Err(FsError::ReadOnly),
            ConsoleKind::Stdout | ConsoleKind::Stderr => {
                // Preserve sys_write's serial and VGA output behavior.
                if let Ok(s) = core::str::from_utf8(buf) {
                    crate::serial_print!("{}", s);
                    crate::print!("{}", s);
                } else {
                    for &byte in buf {
                        crate::serial_print!("{}", byte as char);
                        crate::print!("{}", byte as char);
                    }
                }
                Ok(buf.len())
            }
        }
    }

    fn lookup(&self, _name: &str) -> Result<InodeRef, FsError> {
        Err(FsError::NotDirectory)
    }

    fn create_child(&self, _name: &str, _file_type: FileType) -> Result<InodeRef, FsError> {
        Err(FsError::NotDirectory)
    }

    fn readdir(&self) -> Result<alloc::vec::Vec<DirEntry>, FsError> {
        Err(FsError::NotDirectory)
    }

    fn truncate(&self, _size: u64) -> Result<(), FsError> {
        Err(FsError::ReadOnly)
    }

    fn len(&self) -> Result<u64, FsError> {
        Ok(0)
    }

    fn is_terminal(&self) -> bool {
        true
    }

    fn stat(&self) -> Result<FileStat, FsError> {
        Ok(FileStat {
            size: 0,
            file_type: FileType::Device,
        })
    }
}

fn console_inode(kind: ConsoleKind) -> InodeRef {
    Arc::new(ConsoleInode { kind })
}

pub fn stdin_file() -> File {
    File::new(console_inode(ConsoleKind::Stdin), OpenFlags::read_only())
        .expect("stdin file construction failed")
}

pub fn stdout_file() -> File {
    File::new(console_inode(ConsoleKind::Stdout), OpenFlags::write_only())
        .expect("stdout file construction failed")
}

pub fn stderr_file() -> File {
    File::new(console_inode(ConsoleKind::Stderr), OpenFlags::write_only())
        .expect("stderr file construction failed")
}
