use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use spin::Mutex;

use super::error::FsError;
use super::filesystem::FileSystem;
use super::inode::{DirEntry, FileStat, FileType, Inode, InodeRef};

pub struct RamFs {
    root: Arc<RamInode>,
}

enum RamNode {
    File(Vec<u8>),
    Directory(BTreeMap<String, Arc<RamInode>>),
}

pub struct RamInode {
    node: Mutex<RamNode>,
    file_type: FileType,
}

impl RamInode {
    fn new(file_type: FileType) -> Self {
        let node = match file_type {
            FileType::Regular => RamNode::File(Vec::new()),
            FileType::Directory => RamNode::Directory(BTreeMap::new()),
            FileType::Device => RamNode::File(Vec::new()),
        };

        Self {
            node: Mutex::new(node),
            file_type,
        }
    }

    fn regular() -> Arc<Self> {
        Arc::new(Self::new(FileType::Regular))
    }

    fn directory() -> Arc<Self> {
        Arc::new(Self::new(FileType::Directory))
    }
}

impl RamFs {
    pub fn new() -> Self {
        Self {
            root: RamInode::directory(),
        }
    }

    pub fn root_inode(&self) -> Arc<RamInode> {
        self.root.clone()
    }
}

impl FileSystem for RamFs {
    fn root(&self) -> InodeRef {
        self.root.clone()
    }
}

impl Inode for RamInode {
    fn file_type(&self) -> FileType {
        self.file_type
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        let node = self.node.lock();
        match &*node {
            RamNode::File(data) => {
                let start = core::cmp::min(offset as usize, data.len());
                let available = data.len() - start;
                let count = core::cmp::min(available, buf.len());
                buf[..count].copy_from_slice(&data[start..start + count]);
                Ok(count)
            }
            RamNode::Directory(_) => Err(FsError::IsDirectory),
        }
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<usize, FsError> {
        let mut node = self.node.lock();
        match &mut *node {
            RamNode::File(data) => {
                let start = offset as usize;
                let end = start.checked_add(buf.len()).ok_or(FsError::Io)?;
                if data.len() < end {
                    data.resize(end, 0);
                }
                data[start..end].copy_from_slice(buf);
                Ok(buf.len())
            }
            RamNode::Directory(_) => Err(FsError::IsDirectory),
        }
    }

    fn lookup(&self, name: &str) -> Result<InodeRef, FsError> {
        if name.is_empty() || name == "." || name == ".." {
            return Err(FsError::NotFound);
        }

        let node = self.node.lock();
        match &*node {
            RamNode::Directory(children) => children
                .get(name)
                .cloned()
                .map(|inode| inode as InodeRef)
                .ok_or(FsError::NotFound),
            RamNode::File(_) => Err(FsError::NotDirectory),
        }
    }

    fn create_child(&self, name: &str, file_type: FileType) -> Result<InodeRef, FsError> {
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            return Err(FsError::InvalidPath);
        }

        let mut node = self.node.lock();
        match &mut *node {
            RamNode::Directory(children) => {
                if children.contains_key(name) {
                    return Err(FsError::AlreadyExists);
                }

                let child = Arc::new(RamInode::new(file_type));
                children.insert(String::from(name), child.clone());
                Ok(child as InodeRef)
            }
            RamNode::File(_) => Err(FsError::NotDirectory),
        }
    }

    fn readdir(&self) -> Result<Vec<DirEntry>, FsError> {
        let node = self.node.lock();
        match &*node {
            RamNode::Directory(children) => {
                let mut entries = Vec::with_capacity(children.len());
                for (name, inode) in children {
                    entries.push(DirEntry {
                        name: name.clone(),
                        file_type: inode.file_type(),
                    });
                }
                Ok(entries)
            }
            RamNode::File(_) => Err(FsError::NotDirectory),
        }
    }

    fn truncate(&self, size: u64) -> Result<(), FsError> {
        let mut node = self.node.lock();
        match &mut *node {
            RamNode::File(data) => {
                let size = usize::try_from(size).map_err(|_| FsError::Io)?;
                data.resize(size, 0);
                Ok(())
            }
            RamNode::Directory(_) => Err(FsError::IsDirectory),
        }
    }

    fn len(&self) -> Result<u64, FsError> {
        let node = self.node.lock();
        match &*node {
            RamNode::File(data) => Ok(data.len() as u64),
            RamNode::Directory(children) => Ok(children.len() as u64),
        }
    }

    fn stat(&self) -> Result<FileStat, FsError> {
        Ok(FileStat {
            size: self.len()?,
            file_type: self.file_type,
        })
    }
}
