use crate::fs::FsError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SysError {
    Efault,
    Einval,
    Ebadf,
    Enomem,
    Enosys,
    Enoent,
    Eexist,
    Enotdir,
    Eisdir,
    Eio,
    Emfile,
    Ebusy,
    Enotty,
    Erange,
    Enotempty,
    Esrch,
    Eintr,
    E2big,
    Enoexec,
    Echild,
    Eacces,
}

impl SysError {
    pub fn to_errno(self) -> u64 {
        match self {
            SysError::Efault => (-14i64) as u64,
            SysError::Einval => (-22i64) as u64,
            SysError::Ebadf => (-9i64) as u64,
            SysError::Enomem => (-12i64) as u64,
            SysError::Enosys => (-38i64) as u64,
            SysError::Enoent => (-2i64) as u64,
            SysError::Eexist => (-17i64) as u64,
            SysError::Enotdir => (-20i64) as u64,
            SysError::Eisdir => (-21i64) as u64,
            SysError::Eio => (-5i64) as u64,
            SysError::Emfile => (-24i64) as u64,
            SysError::Ebusy => (-16i64) as u64,
            SysError::Enotty => (-25i64) as u64,
            SysError::Erange => (-34i64) as u64,
            SysError::Enotempty => (-39i64) as u64,
            SysError::Esrch => (-3i64) as u64,
            SysError::Eintr => (-4i64) as u64,
            SysError::E2big => (-7i64) as u64,
            SysError::Enoexec => (-8i64) as u64,
            SysError::Echild => (-10i64) as u64,
            SysError::Eacces => (-13i64) as u64,
        }
    }

    pub fn from_fs_error(err: FsError) -> Self {
        match err {
            FsError::NotFound => SysError::Enoent,
            FsError::AlreadyExists => SysError::Eexist,
            FsError::NotDirectory => SysError::Enotdir,
            FsError::IsDirectory => SysError::Eisdir,
            FsError::BadFd => SysError::Ebadf,
            FsError::ReadOnly => SysError::Ebadf,
            FsError::InvalidFlags => SysError::Einval,
            FsError::InvalidPath => SysError::Einval,
            FsError::NotEmpty => SysError::Enotempty,
            FsError::Busy => SysError::Ebusy,
            FsError::Io => SysError::Eio,
            FsError::NotInitialized => SysError::Eio,
            FsError::TooManyFiles => SysError::Emfile,
            FsError::Interrupted => SysError::Eintr,
        }
    }
}
