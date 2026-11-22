//! Program files: installing the embedded user programs into `/bin`, and
//! loading a program file for `spawn`.
//!
//! The kernel binary embeds the compiled programs (`include_bytes!` of
//! `$OUT_DIR/<name>.elf`). At boot `src/main.rs` calls [`install`], which
//! writes each one to `/bin/<name>` on the root filesystem. Programs then
//! start each other by path (`spawn("/bin/hello", argv)`), which reads the
//! file back with [`load`].

use alloc::format;
use alloc::vec::Vec;

use crate::fs::file::{O_CREAT, O_TRUNC, O_WRONLY};
use crate::fs::{with_vfs, FileType, FsError, OpenFlags, Path, Vfs};

/// Where [`install`] puts programs.
pub const BIN_DIR: &str = "/bin";

/// Largest program file [`load`] reads. Programs are read whole into the
/// kernel heap (10 MiB) before they are loaded.
pub const MAX_PROGRAM_SIZE: u64 = 4 * 1024 * 1024;

/// Write every `(name, bytes)` to `/bin/<name>`, creating `/bin` if needed.
///
/// See [`install_in`].
pub fn install(programs: &[(&str, &[u8])]) -> Result<(), FsError> {
    install_in(BIN_DIR, programs)
}

/// Write every `(name, bytes)` to `<dir>/<name>` (`dir` is absolute).
///
/// Idempotent, because the ext2 disk persists between boots:
/// - `dir` is created if it's missing. If it already exists it must be a
///   directory (`NotDirectory` otherwise).
/// - Each file is opened with `O_TRUNC` and rewritten in full, so it never
///   keeps bytes from an older, longer version. Other files in `dir` are
///   left alone.
pub fn install_in(dir: &str, programs: &[(&str, &[u8])]) -> Result<(), FsError> {
    let root = Path::root();
    with_vfs(|vfs| {
        match vfs.mkdir(&root, dir) {
            Ok(()) => {}
            Err(FsError::AlreadyExists) => {
                if vfs.stat(&root, dir)?.file_type != FileType::Directory {
                    return Err(FsError::NotDirectory);
                }
            }
            Err(error) => return Err(error),
        }
        for &(name, bytes) in programs {
            write_file(vfs, &format!("{dir}/{name}"), bytes)?;
        }
        Ok(())
    })
}

fn write_file(vfs: &Vfs, path: &str, bytes: &[u8]) -> Result<(), FsError> {
    let flags = OpenFlags::from_raw(O_WRONLY | O_CREAT | O_TRUNC)?;
    let mut file = vfs.open(&Path::root(), path, flags)?;
    let mut written = 0;
    while written < bytes.len() {
        let n = file.write(&bytes[written..])?;
        if n == 0 {
            return Err(FsError::Io);
        }
        written += n;
    }
    Ok(())
}

/// Why [`load`] failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadError {
    /// The path doesn't resolve, or reading failed.
    Fs(FsError),
    /// Not a regular file (a directory or a device). `spawn`: `EACCES`.
    NotRegular,
    /// Larger than [`MAX_PROGRAM_SIZE`]. `spawn`: `ENOMEM`.
    TooLarge,
    /// Not a 64-bit little-endian x86_64 ELF. `spawn`: `ENOEXEC`.
    NotElf,
}

impl From<FsError> for LoadError {
    fn from(error: FsError) -> Self {
        LoadError::Fs(error)
    }
}

/// Read the whole program file at the absolute `path`, checking that it is
/// a regular file and an x86_64 ELF. The VFS lock is held while reading.
pub fn load(path: &Path) -> Result<Vec<u8>, LoadError> {
    let root = Path::root();
    // The outer Result is the VFS's, the inner one is load's own checks.
    let bytes = with_vfs(|vfs| -> Result<Result<Vec<u8>, LoadError>, FsError> {
        let stat = vfs.stat(&root, path.as_str())?;
        if stat.file_type != FileType::Regular {
            return Ok(Err(LoadError::NotRegular));
        }
        if stat.size > MAX_PROGRAM_SIZE {
            return Ok(Err(LoadError::TooLarge));
        }
        let mut file = vfs.open(&root, path.as_str(), OpenFlags::read_only())?;
        let mut bytes = alloc::vec![0u8; stat.size as usize];
        let mut filled = 0;
        while filled < bytes.len() {
            let n = file.read(&mut bytes[filled..])?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        bytes.truncate(filled);
        Ok(Ok(bytes))
    })??;

    if !is_x86_64_elf(&bytes) {
        return Err(LoadError::NotElf);
    }
    Ok(bytes)
}

/// e_ident: magic, ELFCLASS64, little-endian; e_machine: x86_64.
pub fn is_x86_64_elf(bytes: &[u8]) -> bool {
    bytes.len() >= 20
        && bytes[..4] == *b"\x7fELF"
        && bytes[4] == 2
        && bytes[5] == 1
        && u16::from_le_bytes([bytes[18], bytes[19]]) == 0x3e
}
