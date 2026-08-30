// src/syscalls/errors.rs

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SysError {
    Efault,
    Einval,
    Ebadf,
    Enomem,
    Enosys,
}

impl SysError {
    pub fn to_errno(self) -> u64 {
        match self {
            SysError::Efault => (-14i64) as u64,
            SysError::Einval => (-22i64) as u64,
            SysError::Ebadf => (-9i64) as u64,
            SysError::Enomem => (-12i64) as u64,
            SysError::Enosys => (-38i64) as u64,
        }
    }
}