use super::super::error::FsError;

pub const EXT2_SUPER_MAGIC: u16 = 0xEF53;
pub const EXT2_ROOT_INO: u32 = 2;
pub const EXT2_NDIR_BLOCKS: usize = 12;
pub const EXT2_IND_BLOCK: usize = 12;
pub const EXT2_DIND_BLOCK: usize = 13;
pub const EXT2_TIND_BLOCK: usize = 14;
pub const EXT2_N_BLOCKS: usize = 15;

pub const EXT2_FT_UNKNOWN: u8 = 0;
pub const EXT2_FT_REG_FILE: u8 = 1;
pub const EXT2_FT_DIR: u8 = 2;
pub const EXT2_FT_CHRDEV: u8 = 3;
pub const EXT2_FT_BLKDEV: u8 = 4;
pub const EXT2_FT_FIFO: u8 = 5;
pub const EXT2_FT_SOCK: u8 = 6;
pub const EXT2_FT_SYMLINK: u8 = 7;

#[derive(Clone, Copy, Debug)]
pub struct Ext2Superblock {
    pub inodes_count: u32,
    pub blocks_count: u32,
    pub first_data_block: u32,
    pub log_block_size: u32,
    pub blocks_per_group: u32,
    pub inodes_per_group: u32,
    pub rev_level: u32,
    pub first_ino: u32,
    pub inode_size: u16,
    pub feature_compat: u32,
    pub feature_incompat: u32,
    pub feature_ro_compat: u32,
    pub desc_size: u16,
}

impl Ext2Superblock {
pub fn parse(buf: &[u8]) -> Result<Self, FsError> {
    if buf.len() < 1024 {
        return Err(FsError::Io);
    }
    if le16(buf, 56)? != EXT2_SUPER_MAGIC {
        return Err(FsError::Io);
    }

    let log_block_size = le32(buf, 24)?;
    if log_block_size > 2 {
        return Err(FsError::Io);
    }

    let block_size = 1024u32 << log_block_size;
    let rev_level = le32(buf, 76)?;
    let inode_size = if rev_level == 0 { 128 } else { le16(buf, 88)? };
    if inode_size == 0 || inode_size as u32 > block_size || inode_size < 128 {
        return Err(FsError::Io);
    }

    let feature_compat = le32(buf, 92)?;
    let feature_incompat = le32(buf, 96)?;
    let feature_ro_compat = le32(buf, 100)?;

    const INCOMPAT_64BIT: u32 = 0x0080;
    let desc_size = if rev_level != 0 && (feature_incompat & INCOMPAT_64BIT) != 0 {
        let sz = le16(buf, 254)?;
        if sz < 32 { 32 } else { sz }
    } else {
        32
    };

    let blocks_per_group = le32(buf, 32)?;
    let inodes_per_group = le32(buf, 40)?;
    if blocks_per_group == 0 || inodes_per_group == 0 {
        return Err(FsError::Io);
    }

    Ok(Self {
        inodes_count: le32(buf, 0)?,
        blocks_count: le32(buf, 4)?,
        first_data_block: le32(buf, 20)?,
        log_block_size,
        blocks_per_group,
        inodes_per_group,
        rev_level,
        first_ino: le32(buf, 84)?,
        inode_size,
        feature_compat,
        feature_incompat,
        feature_ro_compat,
        desc_size,
    })
}

    pub fn block_size(&self) -> u64 {
        1024u64 << self.log_block_size
    }

    pub fn sectors_per_block(&self) -> u32 {
        (self.block_size() / 512) as u32
    }

    pub fn group_count(&self) -> u32 {
        let blocks = self.blocks_count.saturating_sub(self.first_data_block) as u64;
        let bpg = self.blocks_per_group as u64;
        ((blocks + bpg - 1) / bpg) as u32
    }

    pub fn supports_implemented_format(&self) -> bool {
        const COMPAT_HAS_JOURNAL: u32 = 0x0004;
        const INCOMPAT_FILETYPE: u32 = 0x0002;
        const INCOMPAT_EXTENTS: u32 = 0x0040;
        const INCOMPAT_64BIT: u32 = 0x0080;
        const INCOMPAT_MMP: u32 = 0x0100;
        const INCOMPAT_FLEX_BG: u32 = 0x0200;
        const INCOMPAT_EA_INODE: u32 = 0x0400;
        const INCOMPAT_DIRDATA: u32 = 0x1000;
        const INCOMPAT_CSUM_SEED: u32 = 0x2000;
        const INCOMPAT_LARGEDIR: u32 = 0x4000;
        const INCOMPAT_INLINE_DATA: u32 = 0x8000;
        const INCOMPAT_ENCRYPT: u32 = 0x10000;
        const RO_COMPAT_BIGALLOC: u32 = 0x0200;
        const RO_COMPAT_METADATA_CSUM: u32 = 0x0400;

        self.feature_compat & COMPAT_HAS_JOURNAL == 0
            && (self.feature_incompat & !INCOMPAT_FILETYPE) == 0
            && (self.feature_incompat
                & (INCOMPAT_EXTENTS
                    | INCOMPAT_64BIT
                    | INCOMPAT_MMP
                    | INCOMPAT_FLEX_BG
                    | INCOMPAT_EA_INODE
                    | INCOMPAT_DIRDATA
                    | INCOMPAT_CSUM_SEED
                    | INCOMPAT_LARGEDIR
                    | INCOMPAT_INLINE_DATA
                    | INCOMPAT_ENCRYPT))
                == 0
            && (self.feature_ro_compat & (RO_COMPAT_BIGALLOC | RO_COMPAT_METADATA_CSUM)) == 0
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Ext2GroupDesc {
    pub block_bitmap: u32,
    pub inode_bitmap: u32,
    pub inode_table: u32,
    pub free_blocks_count: u16,
    pub free_inodes_count: u16,
    pub used_dirs_count: u16,
}

impl Ext2GroupDesc {
    pub fn parse(buf: &[u8]) -> Result<Self, FsError> {
        if buf.len() < 32 {
            return Err(FsError::Io);
        }
        Ok(Self {
            block_bitmap: le32(buf, 0)?,
            inode_bitmap: le32(buf, 4)?,
            inode_table: le32(buf, 8)?,
            free_blocks_count: le16(buf, 12)?,
            free_inodes_count: le16(buf, 14)?,
            used_dirs_count: le16(buf, 16)?,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Ext2DiskInode {
    pub mode: u16,
    pub uid: u16,
    pub size: u64,
    pub atime: u32,
    pub ctime: u32,
    pub mtime: u32,
    pub dtime: u32,
    pub gid: u16,
    pub links_count: u16,
    pub blocks: u32,
    pub flags: u32,
    pub block: [u32; EXT2_N_BLOCKS],
}

impl Ext2DiskInode {
    pub fn empty() -> Self {
        Self {
            mode: 0,
            uid: 0,
            size: 0,
            atime: 0,
            ctime: 0,
            mtime: 0,
            dtime: 0,
            gid: 0,
            links_count: 0,
            blocks: 0,
            flags: 0,
            block: [0; EXT2_N_BLOCKS],
        }
    }

    pub fn parse(buf: &[u8], inode_size: usize) -> Result<Self, FsError> {
        if inode_size < 128 || buf.len() < inode_size {
            return Err(FsError::Io);
        }

        let mut block = [0u32; EXT2_N_BLOCKS];
        for (i, slot) in block.iter_mut().enumerate() {
            *slot = le32(buf, 40 + i * 4)?;
        }

        let size_low = le32(buf, 4)? as u64;
        let size_high = le32(buf, 108)? as u64;
        let mode = le16(buf, 0)?;
        let size = if (mode & 0xF000) == 0x8000 {
            size_low | (size_high << 32)
        } else {
            size_low
        };

        Ok(Self {
            mode,
            uid: le16(buf, 2)?,
            size,
            atime: le32(buf, 8)?,
            ctime: le32(buf, 12)?,
            mtime: le32(buf, 16)?,
            dtime: le32(buf, 20)?,
            gid: le16(buf, 24)?,
            links_count: le16(buf, 26)?,
            blocks: le32(buf, 28)?,
            flags: le32(buf, 32)?,
            block,
        })
    }

    pub fn is_directory(&self) -> bool {
        (self.mode & 0xF000) == 0x4000
    }

    pub fn is_regular(&self) -> bool {
        (self.mode & 0xF000) == 0x8000
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Ext2DirEntryHeader {
    pub inode: u32,
    pub rec_len: u16,
    pub name_len: u8,
    pub file_type: u8,
}

impl Ext2DirEntryHeader {
    pub fn parse(buf: &[u8], offset: usize) -> Result<Self, FsError> {
        if offset.checked_add(8).ok_or(FsError::Io)? > buf.len() {
            return Err(FsError::Io);
        }
        Ok(Self {
            inode: le32(buf, offset)?,
            rec_len: le16(buf, offset + 4)?,
            name_len: buf[offset + 6],
            file_type: buf[offset + 7],
        })
    }
}

fn le16(buf: &[u8], offset: usize) -> Result<u16, FsError> {
    let end = offset.checked_add(2).ok_or(FsError::Io)?;
    let bytes = buf.get(offset..end).ok_or(FsError::Io)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn le32(buf: &[u8], offset: usize) -> Result<u32, FsError> {
    let end = offset.checked_add(4).ok_or(FsError::Io)?;
    let bytes = buf.get(offset..end).ok_or(FsError::Io)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

pub fn put_le16(buf: &mut [u8], offset: usize, value: u16) -> Result<(), FsError> {
    let end = offset.checked_add(2).ok_or(FsError::Io)?;
    let dst = buf.get_mut(offset..end).ok_or(FsError::Io)?;
    dst.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

pub fn put_le32(buf: &mut [u8], offset: usize, value: u32) -> Result<(), FsError> {
    let end = offset.checked_add(4).ok_or(FsError::Io)?;
    let dst = buf.get_mut(offset..end).ok_or(FsError::Io)?;
    dst.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

pub fn align4(value: usize) -> usize {
    (value + 3) & !3
}
