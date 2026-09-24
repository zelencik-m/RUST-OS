// src/fs/vfs.rs

use alloc::sync::Arc;

use crate::sync::IrqSafeMutex;

use super::error::FsError;
use super::file::{File, OpenFlags};
use super::filesystem::FileSystem;
use super::inode::{DirEntry, FileStat, FileType, InodeRef};
use super::path::Path;
use super::ramfs::RamFs;

pub struct Vfs {
    root_fs: Arc<dyn FileSystem>,
}

impl Vfs {
    pub fn new(root_fs: Arc<dyn FileSystem>) -> Self {
        Self { root_fs }
    }

    fn resolve_inode(&self, path: &Path) -> Result<InodeRef, FsError> {
        let mut current = self.root_fs.root();

        for component in path.components() {
            current = current.lookup(component)?;
        }

        Ok(current)
    }

    fn resolve_parent<'a>(&self, path: &'a Path) -> Result<(InodeRef, &'a str), FsError>  {
        let name = path.file_name().ok_or(FsError::InvalidPath)?;
        let parent_path = path.parent();
        let parent = self.resolve_inode(&parent_path)?;
        if parent.file_type() != FileType::Directory {
            return Err(FsError::NotDirectory);
        }
        Ok((parent, name))
    }

    pub fn open(
        &self,
        cwd: &Path,
        raw_path: &str,
        flags: OpenFlags,
    ) -> Result<File, FsError> {
        let path = Path::resolve(cwd, raw_path)?;

        let inode = match self.resolve_inode(&path) {
            Ok(inode) => {
                if flags.create() && flags.truncate() && inode.file_type() == FileType::Directory {
                    return Err(FsError::IsDirectory);
                }
                inode
            }
            Err(FsError::NotFound) if flags.create() => {
                let (parent, name) = self.resolve_parent(&path)?;
                parent.create_child(name, FileType::Regular)?
            }
            Err(err) => return Err(err),
        };

        let mut file = File::new(inode, flags)?;
        if flags.truncate() {
            file.truncate(0)?;
        }
        Ok(file)
    }

    pub fn mkdir(&self, cwd: &Path, raw_path: &str) -> Result<(), FsError> {
        let path = Path::resolve(cwd, raw_path)?;
        if path.is_root() {
            return Err(FsError::AlreadyExists);
        }

        let (parent, name) = self.resolve_parent(&path)?;
        parent.create_child(name, FileType::Directory)?;
        Ok(())
    }

    pub fn stat(&self, cwd: &Path, raw_path: &str) -> Result<FileStat, FsError> {
        let path = Path::resolve(cwd, raw_path)?;
        self.resolve_inode(&path)?.stat()
    }

    pub fn readdir(&self, cwd: &Path, raw_path: &str) -> Result<alloc::vec::Vec<DirEntry>, FsError> {
        let path = Path::resolve(cwd, raw_path)?;
        self.resolve_inode(&path)?.readdir()
    }
}

pub static VFS: IrqSafeMutex<Option<Vfs>> = IrqSafeMutex::new(None);

pub fn init() {
    let ramfs = Arc::new(RamFs::new());
    let vfs = Vfs::new(ramfs);
    *VFS.lock() = Some(vfs);
}

pub fn with_vfs<R>(f: impl FnOnce(&Vfs) -> Result<R, FsError>) -> Result<R, FsError> {
    let guard = VFS.lock();
    let vfs = guard.as_ref().ok_or(FsError::NotInitialized)?;
    f(vfs)
}
