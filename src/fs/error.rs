#![allow(dead_code)]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    NotFound,
    AlreadyExists,
    NotDirectory,
    IsDirectory,
    BadFd,
    ReadOnly,
    InvalidFlags,
    InvalidPath,
    NotEmpty,
    /// The target is in use and cannot be removed (e.g. `rmdir("/")`).
    Busy,
    Io,
    NotInitialized,
    TooManyFiles,
}
