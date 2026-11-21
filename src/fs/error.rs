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
    /// A blocking read was cut short because the process is being killed
    /// (Ctrl-C). The syscall returns `EINTR`, and the process exits on its
    /// way back to user mode.
    Interrupted,
}
