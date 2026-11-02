use crate::sync::IrqSafeMutex;
use x86_64::instructions::port::Port;

use super::block_device::{BlockDevice, SECTOR_SIZE};
use super::error::FsError;

const ATA_STATUS_ERR: u8 = 0x01;
const ATA_STATUS_DRQ: u8 = 0x08;
const ATA_STATUS_SRV: u8 = 0x10;
const ATA_STATUS_DF: u8 = 0x20;
const ATA_STATUS_RDY: u8 = 0x40;
const ATA_STATUS_BSY: u8 = 0x80;

const ATA_CMD_READ_SECTORS: u8 = 0x20;
const ATA_CMD_WRITE_SECTORS: u8 = 0x30;
const ATA_CMD_CACHE_FLUSH: u8 = 0xE7;

/// Legacy ATA PIO disk using 28-bit LBA.
///
/// Uses polling without DMA or interrupts, suitable for QEMU's legacy IDE
/// controller.
pub struct AtaPioDisk {
    io_base: u16,
    control_base: u16,
    drive_select: u8,
    lock: IrqSafeMutex<()>,
}

impl AtaPioDisk {
    /// Primary-master ATA disk (0x1F0 / 0x3F6).
    pub const fn primary_master() -> Self {
        Self {
            io_base: 0x1F0,
            control_base: 0x3F6,
            drive_select: 0xE0,
            lock: IrqSafeMutex::new(()),
        }
    }

    /// Secondary-master ATA disk (0x170 / 0x376).
    ///
    /// Attach the ext2 image here to keep it separate from the boot device.
    pub const fn secondary_master() -> Self {
        Self {
            io_base: 0x170,
            control_base: 0x376,
            drive_select: 0xE0,
            lock: IrqSafeMutex::new(()),
        }
    }

    fn read_status(&self) -> u8 {
        let mut port = Port::<u8>::new(self.io_base + 7);
        unsafe { port.read() }
    }

    fn read_alt_status(&self) -> u8 {
        let mut port = Port::<u8>::new(self.control_base);
        unsafe { port.read() }
    }

    fn write_device(&self, value: u8) {
        let mut port = Port::<u8>::new(self.io_base + 6);
        unsafe { port.write(value) }
    }

    fn write_sector_count(&self, value: u8) {
        let mut port = Port::<u8>::new(self.io_base + 2);
        unsafe { port.write(value) }
    }

    fn write_lba_low(&self, value: u8) {
        let mut port = Port::<u8>::new(self.io_base + 3);
        unsafe { port.write(value) }
    }

    fn write_lba_mid(&self, value: u8) {
        let mut port = Port::<u8>::new(self.io_base + 4);
        unsafe { port.write(value) }
    }

    fn write_lba_high(&self, value: u8) {
        let mut port = Port::<u8>::new(self.io_base + 5);
        unsafe { port.write(value) }
    }

    fn write_command(&self, value: u8) {
        let mut port = Port::<u8>::new(self.io_base + 7);
        unsafe { port.write(value) }
    }

    fn write_control(&self, value: u8) {
        let mut port = Port::<u8>::new(self.control_base);
        unsafe { port.write(value) }
    }

    fn delay_400ns(&self) {
        for _ in 0..4 {
            let _ = self.read_alt_status();
        }
    }

    fn wait_not_busy(&self) -> Result<u8, FsError> {
        for _ in 0..10_000_000usize {
            let status = self.read_status();
            if (status & ATA_STATUS_BSY) == 0 {
                return Ok(status);
            }
            core::hint::spin_loop();
        }
        Err(FsError::Io)
    }

    fn wait_drq(&self) -> Result<(), FsError> {
        for _ in 0..10_000_000usize {
            let status = self.read_status();
            if (status & ATA_STATUS_ERR) != 0 || (status & ATA_STATUS_DF) != 0 {
                return Err(FsError::Io);
            }
            if (status & ATA_STATUS_BSY) == 0 && (status & ATA_STATUS_DRQ) != 0 {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(FsError::Io)
    }

    fn wait_flush(&self) -> Result<(), FsError> {
        for _ in 0..10_000_000usize {
            let status = self.read_status();
            if (status & ATA_STATUS_ERR) != 0 || (status & ATA_STATUS_DF) != 0 {
                return Err(FsError::Io);
            }
            if (status & ATA_STATUS_BSY) == 0 {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(FsError::Io)
    }

    fn validate_request(&self, lba: u64, sectors: usize, buf_len: usize) -> Result<(), FsError> {
        if sectors == 0 || buf_len != sectors.checked_mul(SECTOR_SIZE).ok_or(FsError::Io)? {
            return Err(FsError::Io);
        }
        // READ/WRITE SECTORS uses 28-bit LBA.
        let last_lba = lba
            .checked_add(sectors as u64 - 1)
            .ok_or(FsError::Io)?;
        if last_lba > 0x0FFF_FFFF {
            return Err(FsError::Io);
        }
        Ok(())
    }

    /// Select the drive, program a 28-bit LBA transfer, and send `command`.
    ///
    /// The drive ignores register writes while BSY is set, so wait for it to
    /// go idle both before and after selecting the drive.
    fn issue_command(&self, command: u8, lba: u64, sectors: usize) -> Result<(), FsError> {
        self.wait_not_busy()?;
        self.write_device(self.drive_select | ((lba >> 24) as u8 & 0x0F));
        self.delay_400ns();
        self.wait_not_busy()?;

        self.write_sector_count(sectors as u8);
        self.write_lba_low((lba & 0xFF) as u8);
        self.write_lba_mid(((lba >> 8) & 0xFF) as u8);
        self.write_lba_high(((lba >> 16) & 0xFF) as u8);
        self.write_command(command);
        Ok(())
    }

    fn read_chunk(&self, lba: u64, sectors: usize, buf: &mut [u8]) -> Result<(), FsError> {
        self.issue_command(ATA_CMD_READ_SECTORS, lba, sectors)?;

        let mut data = Port::<u16>::new(self.io_base);
        for sector in 0..sectors {
            self.wait_drq()?;
            let sector_buf = &mut buf[sector * SECTOR_SIZE..(sector + 1) * SECTOR_SIZE];
            for word in sector_buf.chunks_exact_mut(2) {
                let value = unsafe { data.read() };
                word.copy_from_slice(&value.to_le_bytes());
            }
        }

        let status = self.wait_not_busy()?;
        if (status & (ATA_STATUS_ERR | ATA_STATUS_DF)) != 0 {
            return Err(FsError::Io);
        }
        Ok(())
    }

    fn write_chunk(&self, lba: u64, sectors: usize, buf: &[u8]) -> Result<(), FsError> {
        self.issue_command(ATA_CMD_WRITE_SECTORS, lba, sectors)?;

        let mut data = Port::<u16>::new(self.io_base);
        for sector in 0..sectors {
            self.wait_drq()?;
            let sector_buf = &buf[sector * SECTOR_SIZE..(sector + 1) * SECTOR_SIZE];
            for word in sector_buf.chunks_exact(2) {
                let value = u16::from_le_bytes([word[0], word[1]]);
                unsafe { data.write(value) };
            }
        }

        self.write_command(ATA_CMD_CACHE_FLUSH);
        self.wait_flush()
    }
}

impl BlockDevice for AtaPioDisk {
    fn read_sectors(
        &self,
        lba: u64,
        sectors: usize,
        buf: &mut [u8],
    ) -> Result<(), FsError> {
        self.validate_request(lba, sectors, buf.len())?;
        let _guard = self.lock.lock();

        // Keep requests below the 256-sector ATA count encoding edge case.
        let mut done = 0usize;
        while done < sectors {
            let count = core::cmp::min(128, sectors - done);
            let byte_start = done * SECTOR_SIZE;
            let byte_end = (done + count) * SECTOR_SIZE;
            self.read_chunk(lba + done as u64, count, &mut buf[byte_start..byte_end])?;
            done += count;
        }
        Ok(())
    }

    fn write_sectors(
        &self,
        lba: u64,
        sectors: usize,
        buf: &[u8],
    ) -> Result<(), FsError> {
        self.validate_request(lba, sectors, buf.len())?;
        let _guard = self.lock.lock();

        let mut done = 0usize;
        while done < sectors {
            let count = core::cmp::min(128, sectors - done);
            let byte_start = done * SECTOR_SIZE;
            let byte_end = (done + count) * SECTOR_SIZE;
            self.write_chunk(lba + done as u64, count, &buf[byte_start..byte_end])?;
            done += count;
        }
        Ok(())
    }
}

#[allow(dead_code)]
const _ATA_STATUS_UNUSED: u8 = ATA_STATUS_SRV | ATA_STATUS_RDY;
