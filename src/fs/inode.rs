use crate::fs::file::FileType;
use crate::fs::error::FsError;
use alloc::sync::Arc;

pub trait Inode {
    fn file_type(&self) -> FileType;

    fn read_at(
        &self,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, FsError>;

    fn write_at(
        &self,
        offset: u64,
        buf: &[u8],
    ) -> Result<usize, FsError>;

    fn lookup(
        &self,
        name: &str,
    ) -> Result<Arc<dyn Inode>, FsError>;
}