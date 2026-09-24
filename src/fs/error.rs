// src/fs/error.rs

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
    Io,
    NotInitialized,
    TooManyFiles,
}
