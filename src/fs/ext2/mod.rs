use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::cmp;
use core::str;

use super::block_device::BlockDevice;
use super::error::FsError;
use super::filesystem::FileSystem;
use super::inode::{DirEntry, FileStat, FileType, Inode, InodeRef};

mod ondisk;

use ondisk::{
    align4, put_le16, put_le32, Ext2DirEntryHeader, Ext2DiskInode, Ext2GroupDesc,
    Ext2Superblock, EXT2_DIND_BLOCK, EXT2_FT_DIR, EXT2_FT_REG_FILE, EXT2_NDIR_BLOCKS,
    EXT2_ROOT_INO, EXT2_SUPER_MAGIC, EXT2_TIND_BLOCK,
};

pub use ondisk::{Ext2DiskInode as DiskInode, Ext2Superblock as Superblock};

const S_IFREG: u16 = 0x8000;
const S_IFDIR: u16 = 0x4000;
const DEFAULT_FILE_MODE: u16 = S_IFREG | 0o644;
const DEFAULT_DIR_MODE: u16 = S_IFDIR | 0o755;

struct Ext2State {
    device: Arc<dyn BlockDevice>,
    superblock: Ext2Superblock,
    groups: Vec<Ext2GroupDesc>,
    group_desc_offset: u64,
}

pub struct Ext2Fs {
    state: Arc<Ext2State>,
}

impl Ext2Fs {
    pub fn mount(device: Arc<dyn BlockDevice>) -> Result<Arc<Self>, FsError> {
        let mut sb_buf = vec![0u8; 1024];

        device.read_at(1024, &mut sb_buf)?;

        let superblock = Ext2Superblock::parse(&sb_buf)?;

        if superblock.blocks_count == 0
            || superblock.inodes_count == 0
            || superblock.group_count() == 0
            || superblock.first_data_block > superblock.blocks_count
            || !superblock.supports_implemented_format()
        {
            return Err(FsError::Io);
        }

        let block_size = superblock.block_size();
        let group_count = superblock.group_count() as usize;
        let desc_size = superblock.desc_size as usize;
        let group_desc_block = if block_size == 1024 { 2u32 } else { 1u32 };
        let group_desc_offset = (group_desc_block as u64)
            .checked_mul(block_size)
            .ok_or(FsError::Io)?;
        let table_bytes = group_count.checked_mul(desc_size).ok_or(FsError::Io)?;
        let mut desc_buf = vec![0u8; table_bytes];
        device.read_at(group_desc_offset, &mut desc_buf)?;

        let mut groups = Vec::with_capacity(group_count);
        for i in 0..group_count {
            let start = i.checked_mul(desc_size).ok_or(FsError::Io)?;
            groups.push(Ext2GroupDesc::parse(&desc_buf[start..start + 32])?);
        }

        let state = Arc::new(Self::state(device, superblock, groups, group_desc_offset));
        let root = state.read_inode(EXT2_ROOT_INO)?;
        if !root.is_directory() {
            return Err(FsError::Io);
        }

        crate::debugln!(
            "[ext2] mounted: block_size={}, blocks={}, inodes={}, groups={}",
            state.block_size(),
            state.superblock.blocks_count,
            state.superblock.inodes_count,
            state.groups.len()
        );

        Ok(Arc::new(Self { state }))
    }

    fn state(
        device: Arc<dyn BlockDevice>,
        superblock: Ext2Superblock,
        groups: Vec<Ext2GroupDesc>,
        group_desc_offset: u64,
    ) -> Ext2State {
        Ext2State {
            device,
            superblock,
            groups,
            group_desc_offset,
        }
    }
}

impl FileSystem for Ext2Fs {
    fn root(&self) -> InodeRef {
        Arc::new(Ext2Inode {
            state: Arc::clone(&self.state),
            ino: EXT2_ROOT_INO,
        })
    }
}

impl Ext2State {
    fn block_size(&self) -> usize {
        self.superblock.block_size() as usize
    }

    fn sectors_per_block(&self) -> u32 {
        self.superblock.sectors_per_block()
    }

    fn read_block(&self, block: u32, buf: &mut [u8]) -> Result<(), FsError> {
        if block == 0 || block >= self.superblock.blocks_count {
            return Err(FsError::Io);
        }
        if buf.len() != self.block_size() {
            return Err(FsError::Io);
        }
        let offset = (block as u64)
            .checked_mul(self.superblock.block_size())
            .ok_or(FsError::Io)?;
        self.device.read_at(offset, buf)
    }

    fn write_block(&self, block: u32, buf: &[u8]) -> Result<(), FsError> {
        if block == 0 || block >= self.superblock.blocks_count {
            return Err(FsError::Io);
        }
        if buf.len() != self.block_size() {
            return Err(FsError::Io);
        }
        let offset = (block as u64)
            .checked_mul(self.superblock.block_size())
            .ok_or(FsError::Io)?;
        self.device.write_at(offset, buf)
    }

    fn read_block_vec(&self, block: u32) -> Result<Vec<u8>, FsError> {
        let mut buf = vec![0u8; self.block_size()];
        self.read_block(block, &mut buf)?;
        Ok(buf)
    }

    fn inode_disk_offset(&self, ino: u32) -> Result<u64, FsError> {
        if ino == 0 || ino > self.superblock.inodes_count {
            return Err(FsError::NotFound);
        }
        let zero_based = ino - 1;
        let group = zero_based / self.superblock.inodes_per_group;
        let index = zero_based % self.superblock.inodes_per_group;
        let desc = self.groups.get(group as usize).ok_or(FsError::Io)?;
        (desc.inode_table as u64)
            .checked_mul(self.superblock.block_size())
            .and_then(|v| {
                v.checked_add(
                    (index as u64).checked_mul(self.superblock.inode_size as u64)?,
                )
            })
            .ok_or(FsError::Io)
    }

    fn read_inode_raw(&self, ino: u32) -> Result<Vec<u8>, FsError> {
        let mut raw = vec![0u8; self.superblock.inode_size as usize];
        self.device.read_at(self.inode_disk_offset(ino)?, &mut raw)?;
        Ok(raw)
    }

    fn read_inode(&self, ino: u32) -> Result<Ext2DiskInode, FsError> {
        let raw = self.read_inode_raw(ino)?;
        Ext2DiskInode::parse(&raw, raw.len())
    }

    fn write_inode(&self, ino: u32, inode: &Ext2DiskInode) -> Result<(), FsError> {
        let mut raw = self.read_inode_raw(ino)?;
        put_le16(&mut raw, 0, inode.mode)?;
        put_le16(&mut raw, 2, inode.uid)?;
        put_le32(&mut raw, 4, inode.size as u32)?;
        put_le32(&mut raw, 8, inode.atime)?;
        put_le32(&mut raw, 12, inode.ctime)?;
        put_le32(&mut raw, 16, inode.mtime)?;
        put_le32(&mut raw, 20, inode.dtime)?;
        put_le16(&mut raw, 24, inode.gid)?;
        put_le16(&mut raw, 26, inode.links_count)?;
        put_le32(&mut raw, 28, inode.blocks)?;
        put_le32(&mut raw, 32, inode.flags)?;
        for (i, block) in inode.block.iter().enumerate() {
            put_le32(&mut raw, 40 + i * 4, *block)?;
        }
        if inode.is_regular() {
            put_le32(&mut raw, 108, (inode.size >> 32) as u32)?;
        } else {
            put_le32(&mut raw, 108, 0)?;
        }
        self.device.write_at(self.inode_disk_offset(ino)?, &raw)
    }

    fn group_blocks(&self, group: u32) -> Result<u32, FsError> {
        if group >= self.superblock.group_count() {
            return Err(FsError::Io);
        }
        let first = self.superblock.first_data_block as u64
            + group as u64 * self.superblock.blocks_per_group as u64;
        if first >= self.superblock.blocks_count as u64 {
            return Ok(0);
        }
        Ok(cmp::min(
            self.superblock.blocks_per_group,
            self.superblock.blocks_count - first as u32,
        ))
    }

    fn bitmap_bit(&self, bitmap_block: u32, bit: u32) -> Result<bool, FsError> {
        let bitmap = self.read_block_vec(bitmap_block)?;
        let byte = (bit / 8) as usize;
        let mask = 1u8 << (bit & 7);
        Ok(bitmap.get(byte).ok_or(FsError::Io)? & mask != 0)
    }

    fn set_bitmap_bit(&self, bitmap_block: u32, bit: u32, set: bool) -> Result<(), FsError> {
        let mut bitmap = self.read_block_vec(bitmap_block)?;
        let byte = (bit / 8) as usize;
        let mask = 1u8 << (bit & 7);
        let value = bitmap.get_mut(byte).ok_or(FsError::Io)?;
        if set {
            *value |= mask;
        } else {
            *value &= !mask;
        }
        self.write_block(bitmap_block, &bitmap)
    }

    fn update_group_counts(
        &self,
        group: u32,
        free_blocks_delta: i32,
        free_inodes_delta: i32,
        dirs_delta: i32,
    ) -> Result<(), FsError> {
        let base = self
            .group_desc_offset
            .checked_add(group as u64 * self.superblock.desc_size as u64)
            .ok_or(FsError::Io)?;
        if free_blocks_delta != 0 {
            let mut b = [0u8; 2];
            self.device.read_at(base + 12, &mut b)?;
            let old = u16::from_le_bytes(b) as i32;
            let new = old + free_blocks_delta;
            if !(0..=u16::MAX as i32).contains(&new) {
                return Err(FsError::Io);
            }
            self.device.write_at(base + 12, &(new as u16).to_le_bytes())?;
        }
        if free_inodes_delta != 0 {
            let mut b = [0u8; 2];
            self.device.read_at(base + 14, &mut b)?;
            let old = u16::from_le_bytes(b) as i32;
            let new = old + free_inodes_delta;
            if !(0..=u16::MAX as i32).contains(&new) {
                return Err(FsError::Io);
            }
            self.device.write_at(base + 14, &(new as u16).to_le_bytes())?;
        }
        if dirs_delta != 0 {
            let mut b = [0u8; 2];
            self.device.read_at(base + 16, &mut b)?;
            let old = u16::from_le_bytes(b) as i32;
            let new = old + dirs_delta;
            if !(0..=u16::MAX as i32).contains(&new) {
                return Err(FsError::Io);
            }
            self.device.write_at(base + 16, &(new as u16).to_le_bytes())?;
        }
        Ok(())
    }

    fn update_super_counts(&self, free_blocks_delta: i32, free_inodes_delta: i32) -> Result<(), FsError> {
        let mut sb = vec![0u8; 1024];
        self.device.read_at(1024, &mut sb)?;

        if free_blocks_delta != 0 {
            let old = u32::from_le_bytes([sb[12], sb[13], sb[14], sb[15]]) as i64;
            let new = old + free_blocks_delta as i64;
            if !(0..=u32::MAX as i64).contains(&new) {
                return Err(FsError::Io);
            }
            put_le32(&mut sb, 12, new as u32)?;
        }
        if free_inodes_delta != 0 {
            let old = u32::from_le_bytes([sb[16], sb[17], sb[18], sb[19]]) as i64;
            let new = old + free_inodes_delta as i64;
            if !(0..=u32::MAX as i64).contains(&new) {
                return Err(FsError::Io);
            }
            put_le32(&mut sb, 16, new as u32)?;
        }
        self.device.write_at(1024, &sb)
    }

    fn allocate_block(&self) -> Result<u32, FsError> {
        for group in 0..self.superblock.group_count() {
            let desc = self.groups[group as usize];
            let group_blocks = self.group_blocks(group)?;
            if group_blocks == 0 {
                continue;
            }

            let mut bitmap = self.read_block_vec(desc.block_bitmap)?;
            for bit in 0..group_blocks {
                let byte = (bit / 8) as usize;
                let mask = 1u8 << (bit & 7);
                if bitmap[byte] & mask != 0 {
                    continue;
                }

                bitmap[byte] |= mask;
                self.write_block(desc.block_bitmap, &bitmap)?;

                self.update_group_counts(group, -1, 0, 0)?;
                self.update_super_counts(-1, 0)?;

                let block = self.superblock.first_data_block
                    + group * self.superblock.blocks_per_group
                    + bit;
                if block == 0 || block >= self.superblock.blocks_count {
                    return Err(FsError::Io);
                }

                let zeroes = vec![0u8; self.block_size()];
                self.write_block(block, &zeroes)?;
                return Ok(block);
            }
        }
        Err(FsError::Io)
    }

    fn free_block(&self, block: u32) -> Result<(), FsError> {
        if block == 0 || block >= self.superblock.blocks_count {
            return Err(FsError::Io);
        }
        if block < self.superblock.first_data_block {
            return Err(FsError::Io);
        }

        let relative = block - self.superblock.first_data_block;
        let group = relative / self.superblock.blocks_per_group;
        let bit = relative % self.superblock.blocks_per_group;
        let desc = self.groups.get(group as usize).ok_or(FsError::Io)?;

        if !self.bitmap_bit(desc.block_bitmap, bit)? {
            return Err(FsError::Io);
        }
        self.set_bitmap_bit(desc.block_bitmap, bit, false)?;
        self.update_group_counts(group, 1, 0, 0)?;
        self.update_super_counts(1, 0)
    }

    fn allocate_inode(&self, is_dir: bool) -> Result<u32, FsError> {
        for group in 0..self.superblock.group_count() {
            let desc = self.groups[group as usize];
            let max = cmp::min(
                self.superblock.inodes_per_group,
                self.superblock.inodes_count
                    .saturating_sub(group * self.superblock.inodes_per_group),
            );
            if max == 0 {
                continue;
            }

            let mut bitmap = self.read_block_vec(desc.inode_bitmap)?;
            for bit in 0..max {
                let byte = (bit / 8) as usize;
                let mask = 1u8 << (bit & 7);
                if bitmap[byte] & mask != 0 {
                    continue;
                }

                bitmap[byte] |= mask;
                self.write_block(desc.inode_bitmap, &bitmap)?;
                self.update_group_counts(group, 0, -1, if is_dir { 1 } else { 0 })?;
                self.update_super_counts(0, -1)?;

                let ino = group * self.superblock.inodes_per_group + bit + 1;
                if ino < EXT2_ROOT_INO {
                    continue;
                }
                return Ok(ino);
            }
        }
        Err(FsError::Io)
    }

    fn free_inode(&self, ino: u32, was_dir: bool) -> Result<(), FsError> {
        if ino == 0 || ino > self.superblock.inodes_count {
            return Err(FsError::Io);
        }
        let relative = ino - 1;
        let group = relative / self.superblock.inodes_per_group;
        let bit = relative % self.superblock.inodes_per_group;
        let desc = self.groups.get(group as usize).ok_or(FsError::Io)?;
        if !self.bitmap_bit(desc.inode_bitmap, bit)? {
            return Err(FsError::Io);
        }
        self.set_bitmap_bit(desc.inode_bitmap, bit, false)?;
        self.update_group_counts(group, 0, 1, if was_dir { -1 } else { 0 })?;
        self.update_super_counts(0, 1)
    }

    fn read_indirect_pointer(&self, block: u32, index: u32) -> Result<u32, FsError> {
        if block == 0 {
            return Ok(0);
        }
        let ptrs = self.block_size() / 4;
        if index as usize >= ptrs {
            return Err(FsError::Io);
        }
        let data = self.read_block_vec(block)?;
        let off = index as usize * 4;
        Ok(u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]))
    }

    fn write_indirect_pointer(&self, block: u32, index: u32, value: u32) -> Result<(), FsError> {
        if block == 0 {
            return Err(FsError::Io);
        }
        let ptrs = self.block_size() / 4;
        if index as usize >= ptrs {
            return Err(FsError::Io);
        }
        let mut data = self.read_block_vec(block)?;
        let off = index as usize * 4;
        data[off..off + 4].copy_from_slice(&value.to_le_bytes());
        self.write_block(block, &data)
    }

    fn alloc_accounting(&self, inode: &mut Ext2DiskInode, block: u32) {
        inode.blocks = inode.blocks.saturating_add(self.sectors_per_block());
        let _ = block;
    }

    fn ensure_block(
        &self,
        inode: &mut Ext2DiskInode,
        logical: u32,
    ) -> Result<u32, FsError> {
        let ptrs = (self.block_size() / 4) as u32;
        if ptrs == 0 {
            return Err(FsError::Io);
        }

        if logical < EXT2_NDIR_BLOCKS as u32 {
            let idx = logical as usize;
            if inode.block[idx] == 0 {
                inode.block[idx] = self.allocate_block()?;
                self.alloc_accounting(inode, inode.block[idx]);
            }
            return Ok(inode.block[idx]);
        }

        let single_capacity = ptrs;
        let double_capacity = ptrs.checked_mul(ptrs).ok_or(FsError::Io)?;
        let triple_capacity = double_capacity.checked_mul(ptrs).ok_or(FsError::Io)?;

        let after_direct = logical - EXT2_NDIR_BLOCKS as u32;
        if after_direct < single_capacity {
            if inode.block[12] == 0 {
                inode.block[12] = self.allocate_block()?;
                self.alloc_accounting(inode, inode.block[12]);
            }
            let data = self.read_indirect_pointer(inode.block[12], after_direct)?;
            if data != 0 {
                return Ok(data);
            }
            let data = self.allocate_block()?;
            self.alloc_accounting(inode, data);
            self.write_indirect_pointer(inode.block[12], after_direct, data)?;
            return Ok(data);
        }

        let after_single = after_direct - single_capacity;
        if after_single < double_capacity {
            let first = after_single / single_capacity;
            let second = after_single % single_capacity;
            if inode.block[13] == 0 {
                inode.block[13] = self.allocate_block()?;
                self.alloc_accounting(inode, inode.block[13]);
            }
            let mut level1 = self.read_indirect_pointer(inode.block[13], first)?;
            if level1 == 0 {
                level1 = self.allocate_block()?;
                self.alloc_accounting(inode, level1);
                self.write_indirect_pointer(inode.block[13], first, level1)?;
            }
            let data = self.read_indirect_pointer(level1, second)?;
            if data != 0 {
                return Ok(data);
            }
            let data = self.allocate_block()?;
            self.alloc_accounting(inode, data);
            self.write_indirect_pointer(level1, second, data)?;
            return Ok(data);
        }

        let after_double = after_single - double_capacity;
        if after_double < triple_capacity {
            let first = after_double / double_capacity;
            let remainder = after_double % double_capacity;
            let second = remainder / single_capacity;
            let third = remainder % single_capacity;
            if inode.block[14] == 0 {
                inode.block[14] = self.allocate_block()?;
                self.alloc_accounting(inode, inode.block[14]);
            }
            let mut level2 = self.read_indirect_pointer(inode.block[14], first)?;
            if level2 == 0 {
                level2 = self.allocate_block()?;
                self.alloc_accounting(inode, level2);
                self.write_indirect_pointer(inode.block[14], first, level2)?;
            }
            let mut level1 = self.read_indirect_pointer(level2, second)?;
            if level1 == 0 {
                level1 = self.allocate_block()?;
                self.alloc_accounting(inode, level1);
                self.write_indirect_pointer(level2, second, level1)?;
            }
            let data = self.read_indirect_pointer(level1, third)?;
            if data != 0 {
                return Ok(data);
            }
            let data = self.allocate_block()?;
            self.alloc_accounting(inode, data);
            self.write_indirect_pointer(level1, third, data)?;
            return Ok(data);
        }

        Err(FsError::Io)
    }

    fn logical_to_physical_block(
        &self,
        inode: &Ext2DiskInode,
        logical: u32,
    ) -> Result<u32, FsError> {
        if (logical as u64) < EXT2_NDIR_BLOCKS as u64 {
            return Ok(inode.block[logical as usize]);
        }
        let ptrs = (self.block_size() / 4) as u32;
        let single_capacity = ptrs as u64;
        let double_capacity = single_capacity * single_capacity;
        let triple_capacity = double_capacity * single_capacity;
        let logical = logical as u64;
        let after_direct = logical - EXT2_NDIR_BLOCKS as u64;

        if after_direct < single_capacity {
            return self.read_indirect_pointer(inode.block[12], after_direct as u32);
        }
        let after_single = after_direct - single_capacity;
        if after_single < double_capacity {
            let first = (after_single / single_capacity) as u32;
            let second = (after_single % single_capacity) as u32;
            let level1 = self.read_indirect_pointer(inode.block[13], first)?;
            return self.read_indirect_pointer(level1, second);
        }
        let after_double = after_single - double_capacity;
        if after_double < triple_capacity {
            let first = (after_double / double_capacity) as u32;
            let rem = after_double % double_capacity;
            let second = (rem / single_capacity) as u32;
            let third = (rem % single_capacity) as u32;
            let level2 = self.read_indirect_pointer(inode.block[14], first)?;
            let level1 = self.read_indirect_pointer(level2, second)?;
            return self.read_indirect_pointer(level1, third);
        }
        Err(FsError::Io)
    }

    fn read_inode_at(
        &self,
        inode: &Ext2DiskInode,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, FsError> {
        if buf.is_empty() || offset >= inode.size {
            return Ok(0);
        }
        let count = cmp::min(buf.len() as u64, inode.size - offset) as usize;
        let block_size = self.block_size() as u64;
        let mut copied = 0usize;
        let mut current = offset;

        while copied < count {
            let logical = current / block_size;
            if logical > u32::MAX as u64 {
                return Err(FsError::Io);
            }
            let in_block = (current % block_size) as usize;
            let chunk = cmp::min(self.block_size() - in_block, count - copied);
            let physical = self.logical_to_physical_block(inode, logical as u32)?;
            if physical == 0 {
                buf[copied..copied + chunk].fill(0);
            } else {
                let data = self.read_block_vec(physical)?;
                buf[copied..copied + chunk]
                    .copy_from_slice(&data[in_block..in_block + chunk]);
            }
            copied += chunk;
            current += chunk as u64;
        }
        Ok(copied)
    }

    fn directory_entries(&self, inode: &Ext2DiskInode) -> Result<Vec<DirEntryRecord>, FsError> {
        if !inode.is_directory() {
            return Err(FsError::NotDirectory);
        }
        let block_size = self.block_size();
        let block_count = ((inode.size as usize) + block_size - 1) / block_size;
        let mut entries = Vec::new();

        for logical in 0..block_count {
            let physical = self.logical_to_physical_block(inode, logical as u32)?;
            if physical == 0 {
                continue;
            }
            let data = self.read_block_vec(physical)?;
            let valid = cmp::min(block_size, inode.size as usize - logical * block_size);
            let mut off = 0usize;
            while off < valid {
                let header = Ext2DirEntryHeader::parse(&data[..valid], off)?;
                let rec_len = header.rec_len as usize;
                let name_len = header.name_len as usize;
                if rec_len < 8 || (rec_len & 3) != 0 || off + rec_len > valid {
                    return Err(FsError::Io);
                }
                if name_len > rec_len - 8 {
                    return Err(FsError::Io);
                }
                if header.inode != 0 {
                    let name_start = off + 8;
                    let name_end = name_start + name_len;
                    let name = str::from_utf8(&data[name_start..name_end])
                        .map_err(|_| FsError::Io)?;
                    entries.push(DirEntryRecord {
                        ino: header.inode,
                        name: String::from(name),
                        file_type: header.file_type,
                        physical_block: physical,
                        offset_in_block: off,
                        rec_len,
                        name_len,
                    });
                }
                off += rec_len;
            }
        }
        Ok(entries)
    }

    fn add_directory_entry(
        &self,
        parent_ino: u32,
        child_ino: u32,
        name: &str,
        file_type: u8,
    ) -> Result<(), FsError> {
        if name.is_empty() || name.len() > 255 || name.contains('/') {
            return Err(FsError::InvalidPath);
        }

        let mut parent = self.read_inode(parent_ino)?;
        if !parent.is_directory() {
            return Err(FsError::NotDirectory);
        }
        let needed = align4(8 + name.len());

        for entry in self.directory_entries(&parent)? {
            let ideal = align4(8 + entry.name_len);
            if entry.rec_len < ideal || entry.rec_len - ideal < needed {
                continue;
            }
            let mut block = self.read_block_vec(entry.physical_block)?;
            let new_offset = entry.offset_in_block + ideal;
            let new_len = entry.rec_len - ideal;
            put_le16(&mut block, entry.offset_in_block + 4, ideal as u16)?;
            put_le32(&mut block, new_offset, child_ino)?;
            put_le16(&mut block, new_offset + 4, new_len as u16)?;
            block[new_offset + 6] = name.len() as u8;
            block[new_offset + 7] = file_type;
            block[new_offset + 8..new_offset + 8 + name.len()].copy_from_slice(name.as_bytes());
            self.write_block(entry.physical_block, &block)?;
            return Ok(());
        }

        let logical = (parent.size / self.superblock.block_size()) as u32;
        let new_block = self.ensure_block(&mut parent, logical)?;
        let mut block = vec![0u8; self.block_size()];
        put_le32(&mut block, 0, child_ino)?;
        put_le16(&mut block, 4, self.block_size() as u16)?;
        block[6] = name.len() as u8;
        block[7] = file_type;
        block[8..8 + name.len()].copy_from_slice(name.as_bytes());
        self.write_block(new_block, &block)?;
        parent.size = parent
            .size
            .checked_add(self.superblock.block_size())
            .ok_or(FsError::Io)?;
        self.write_inode(parent_ino, &parent)
    }

    fn initialize_directory(&self, ino: u32, parent_ino: u32) -> Result<(), FsError> {
        let block = self.allocate_block()?;
        let mut inode = Ext2DiskInode::empty();
        inode.mode = DEFAULT_DIR_MODE;
        inode.links_count = 2;
        inode.size = self.superblock.block_size();
        inode.blocks = self.sectors_per_block();
        inode.block[0] = block;
        self.write_inode(ino, &inode)?;

        let mut data = vec![0u8; self.block_size()];
        put_le32(&mut data, 0, ino)?;
        put_le16(&mut data, 4, 12)?;
        data[6] = 1;
        data[7] = EXT2_FT_DIR;
        data[8] = b'.';

        let second = 12;
        put_le32(&mut data, second, parent_ino)?;
        put_le16(&mut data, second + 4, (self.block_size() - second) as u16)?;
        data[second + 6] = 2;
        data[second + 7] = EXT2_FT_DIR;
        data[second + 8..second + 10].copy_from_slice(b"..");
        self.write_block(block, &data)
    }

    fn create_inode(&self, parent_ino: u32, name: &str, file_type: FileType) -> Result<u32, FsError> {
        let parent = self.read_inode(parent_ino)?;
        if !parent.is_directory() {
            return Err(FsError::NotDirectory);
        }
        if name.is_empty() || name.len() > 255 || name.contains('/') {
            return Err(FsError::InvalidPath);
        }

        let child_is_dir = file_type == FileType::Directory;
        let ino = self.allocate_inode(child_is_dir)?;

        let result = (|| {
            if child_is_dir {
                self.initialize_directory(ino, parent_ino)?;
            } else if file_type == FileType::Regular {
                let mut inode = Ext2DiskInode::empty();
                inode.mode = DEFAULT_FILE_MODE;
                inode.links_count = 1;
                self.write_inode(ino, &inode)?;
            } else {
                return Err(FsError::Io);
            }

            self.add_directory_entry(
                parent_ino,
                ino,
                name,
                if child_is_dir { EXT2_FT_DIR } else { EXT2_FT_REG_FILE },
            )?;

            if child_is_dir {
                let mut updated_parent = self.read_inode(parent_ino)?;
                updated_parent.links_count = updated_parent.links_count.saturating_add(1);
                self.write_inode(parent_ino, &updated_parent)?;
            }
            Ok(ino)
        })();

        if result.is_err() {
            let _ = self.free_inode(ino, child_is_dir);
        }
        result.map(|_| ino)
    }

    fn free_indirect_tree(&self, block: u32, depth: usize, freed: &mut u32) -> Result<(), FsError> {
        if block == 0 {
            return Ok(());
        }
        let data = self.read_block_vec(block)?;
        let ptrs = self.block_size() / 4;
        for i in 0..ptrs {
            let off = i * 4;
            let child = u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
            if child == 0 {
                continue;
            }
            if depth == 1 {
                self.free_block(child)?;
                *freed = freed.saturating_add(1);
            } else {
                self.free_indirect_tree(child, depth - 1, freed)?;
            }
        }
        self.free_block(block)?;
        *freed = freed.saturating_add(1);
        Ok(())
    }

    fn truncate_inode_to_zero(&self, ino: u32) -> Result<(), FsError> {
        let mut inode = self.read_inode(ino)?;
        if inode.is_directory() {
            return Err(FsError::IsDirectory);
        }

        for idx in 0..EXT2_NDIR_BLOCKS {
            if inode.block[idx] != 0 {
                self.free_block(inode.block[idx])?;
                inode.block[idx] = 0;
            }
        }
        let mut freed_metadata_and_data = 0u32;
        if inode.block[12] != 0 {
            let block = inode.block[12];
            self.free_indirect_tree(block, 1, &mut freed_metadata_and_data)?;
            inode.block[12] = 0;
        }
        if inode.block[13] != 0 {
            let block = inode.block[13];
            self.free_indirect_tree(block, 2, &mut freed_metadata_and_data)?;
            inode.block[13] = 0;
        }
        if inode.block[14] != 0 {
            let block = inode.block[14];
            self.free_indirect_tree(block, 3, &mut freed_metadata_and_data)?;
            inode.block[14] = 0;
        }
        inode.size = 0;
        inode.blocks = 0;
        self.write_inode(ino, &inode)
    }
}

struct DirEntryRecord {
    ino: u32,
    name: String,
    file_type: u8,
    physical_block: u32,
    offset_in_block: usize,
    rec_len: usize,
    name_len: usize,
}

pub struct Ext2Inode {
    state: Arc<Ext2State>,
    ino: u32,
}

impl Ext2Inode {
    pub fn inode_number(&self) -> u32 {
        self.ino
    }

    fn child(&self, ino: u32) -> InodeRef {
        Arc::new(Self {
            state: Arc::clone(&self.state),
            ino,
        })
    }
}

impl Inode for Ext2Inode {
    fn file_type(&self) -> FileType {
        self.state
            .inode_file_type(self.ino)
            .unwrap_or(FileType::Device)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        let inode = self.state.read_inode(self.ino)?;
        if inode.is_directory() {
            return Err(FsError::IsDirectory);
        }
        if !inode.is_regular() {
            return Err(FsError::Io);
        }
        self.state.read_inode_at(&inode, offset, buf)
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<usize, FsError> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut inode = self.state.read_inode(self.ino)?;
        if inode.is_directory() {
            return Err(FsError::IsDirectory);
        }
        if !inode.is_regular() {
            return Err(FsError::Io);
        }

        let end = offset.checked_add(buf.len() as u64).ok_or(FsError::Io)?;
        let block_size = self.state.block_size() as u64;
        let mut written = 0usize;
        while written < buf.len() {
            let current = offset + written as u64;
            let logical = current / block_size;
            if logical > u32::MAX as u64 {
                return Err(FsError::Io);
            }
            let in_block = (current % block_size) as usize;
            let chunk = cmp::min(self.state.block_size() - in_block, buf.len() - written);
            let physical = self.state.ensure_block(&mut inode, logical as u32)?;
            let disk_offset = (physical as u64)
                .checked_mul(self.state.superblock.block_size())
                .and_then(|v| v.checked_add(in_block as u64))
                .ok_or(FsError::Io)?;
            self.state
                .device
                .write_at(disk_offset, &buf[written..written + chunk])?;
            written += chunk;
        }

        if end > inode.size {
            inode.size = end;
        }
        self.state.write_inode(self.ino, &inode)?;
        Ok(written)
    }

    fn lookup(&self, name: &str) -> Result<InodeRef, FsError> {
        if name.is_empty() || name.contains('/') {
            return Err(FsError::InvalidPath);
        }
        let inode = self.state.read_inode(self.ino)?;
        if !inode.is_directory() {
            return Err(FsError::NotDirectory);
        }
        for entry in self.state.directory_entries(&inode)? {
            if entry.name == name {
                return Ok(self.child(entry.ino));
            }
        }
        Err(FsError::NotFound)
    }

    fn create_child(&self, name: &str, file_type: FileType) -> Result<InodeRef, FsError> {
        if self.lookup(name).is_ok() {
            return Err(FsError::AlreadyExists);
        }
        let ino = self.state.create_inode(self.ino, name, file_type)?;
        Ok(self.child(ino))
    }

    fn readdir(&self) -> Result<Vec<DirEntry>, FsError> {
        let inode = self.state.read_inode(self.ino)?;
        let records = self.state.directory_entries(&inode)?;
        let mut result = Vec::with_capacity(records.len());
        for entry in records {
            let file_type = self.state.inode_file_type(entry.ino).unwrap_or_else(|_| match entry.file_type {
                EXT2_FT_DIR => FileType::Directory,
                EXT2_FT_REG_FILE => FileType::Regular,
                _ => FileType::Device,
            });
            result.push(DirEntry {
                name: entry.name,
                file_type,
            });
        }
        Ok(result)
    }

    fn truncate(&self, size: u64) -> Result<(), FsError> {
        if size != 0 {
            return Err(FsError::Io);
        }
        self.state.truncate_inode_to_zero(self.ino)
    }

    fn len(&self) -> Result<u64, FsError> {
        Ok(self.state.read_inode(self.ino)?.size)
    }

    fn stat(&self) -> Result<FileStat, FsError> {
        let inode = self.state.read_inode(self.ino)?;
        let file_type = if inode.is_directory() {
            FileType::Directory
        } else if inode.is_regular() {
            FileType::Regular
        } else {
            FileType::Device
        };
        Ok(FileStat {
            size: inode.size,
            file_type,
        })
    }
}

impl Ext2State {
    fn inode_file_type(&self, ino: u32) -> Result<FileType, FsError> {
        let inode = self.read_inode(ino)?;
        Ok(if inode.is_directory() {
            FileType::Directory
        } else if inode.is_regular() {
            FileType::Regular
        } else {
            FileType::Device
        })
    }
}

#[allow(dead_code)]
const _EXT2_MAGIC: u16 = EXT2_SUPER_MAGIC;
#[allow(dead_code)]
const _EXT2_DIND_INDEX: usize = EXT2_DIND_BLOCK;
#[allow(dead_code)]
const _EXT2_TIND_INDEX: usize = EXT2_TIND_BLOCK;
