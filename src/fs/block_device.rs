// src/fs/block_device.rs

use alloc::vec::Vec;

use super::error::FsError;

/// Physical sector size used by the ATA PIO implementation.
pub const SECTOR_SIZE: usize = 512;

/// Minimal block-device API used by filesystems.
///
/// The filesystem never talks directly to ATA ports. It asks for sectors,
/// while the default `read_at` helper handles unaligned byte ranges.
pub trait BlockDevice: Send + Sync {
    fn read_sectors(
        &self,
        lba: u64,
        sectors: usize,
        buf: &mut [u8],
    ) -> Result<(), FsError>;

    fn write_sectors(
        &self,
        lba: u64,
        sectors: usize,
        buf: &[u8],
    ) -> Result<(), FsError>;

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), FsError> {
        if buf.is_empty() {
            return Ok(());
        }

        let start_sector = offset / SECTOR_SIZE as u64;
        let end_offset = offset
            .checked_add(buf.len() as u64)
            .ok_or(FsError::Io)?;
        let end_sector = (end_offset - 1) / SECTOR_SIZE as u64;
        let sector_count = usize::try_from(end_sector - start_sector + 1)
            .map_err(|_| FsError::Io)?;

        let mut sectors = Vec::new();
        sectors.resize(
            sector_count
                .checked_mul(SECTOR_SIZE)
                .ok_or(FsError::Io)?,
            0,
        );

        self.read_sectors(start_sector, sector_count, &mut sectors)?;

        let start_in_sector = (offset % SECTOR_SIZE as u64) as usize;
        let end = start_in_sector
            .checked_add(buf.len())
            .ok_or(FsError::Io)?;
        buf.copy_from_slice(&sectors[start_in_sector..end]);
        Ok(())
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<(), FsError> {
        if buf.is_empty() {
            return Ok(());
        }

        let start_sector = offset / SECTOR_SIZE as u64;
        let end_offset = offset
            .checked_add(buf.len() as u64)
            .ok_or(FsError::Io)?;
        let end_sector = (end_offset - 1) / SECTOR_SIZE as u64;
        let sector_count = usize::try_from(end_sector - start_sector + 1)
            .map_err(|_| FsError::Io)?;

        let mut sectors = Vec::new();
        sectors.resize(
            sector_count
                .checked_mul(SECTOR_SIZE)
                .ok_or(FsError::Io)?,
            0,
        );

        // Read-modify-write if the request does not cover whole sectors.
        if (offset % SECTOR_SIZE as u64) != 0 || (buf.len() % SECTOR_SIZE) != 0 {
            self.read_sectors(start_sector, sector_count, &mut sectors)?;
        }

        let start_in_sector = (offset % SECTOR_SIZE as u64) as usize;
        let end = start_in_sector
            .checked_add(buf.len())
            .ok_or(FsError::Io)?;
        sectors[start_in_sector..end].copy_from_slice(buf);
        self.write_sectors(start_sector, sector_count, &sectors)
    }
}
