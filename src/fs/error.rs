
pub enum FsError {
    NotFound,
    AlreadyExists,
    NotDirectory,
    IsDirectory,
    InvalidOffset,
    ReadOnly,
    Io,
}