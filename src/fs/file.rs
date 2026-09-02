
use crate::fs::inode::Inode;
use crate::fs::error::FsError;
use alloc::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileType {
    Regular,
    Directory,
    Device,
}

pub struct File {
    pub inode: Arc<dyn Inode>,
    pub offset: u64,
    // pub flags: OpenFlags,
}

impl File {
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, FsError> {
        let n = self.inode.read_at(self.offset, buf)?;
        self.offset += n as u64;
        Ok(n)
    }

    pub fn write(&mut self, buf: &[u8]) -> Result<usize, FsError> {
        let n = self.inode.write_at(self.offset, buf)?;
        self.offset += n as u64;
        Ok(n)
    }
}