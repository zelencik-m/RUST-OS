use core::ptr;
use alloc::vec::Vec;
use x86_64::{
    structures::paging::{
        FrameAllocator, OffsetPageTable, PageTable, PageTableFlags, PhysFrame, Size4KiB,
    },
    VirtAddr,
};
use x86_64::PhysAddr;

use crate::memory::paging::active_level_4_table;
use crate::memory::vma::VmArea;

/// Low-half kernel mappings remain shared until the kernel moves higher-half.
pub const USER_P4_START: usize = 255;
pub const USER_P4_END: usize = 256; // exclusive

/// Higher-half kernel mappings.
pub const KERNEL_P4_START: usize = 256;

/// User space lives entirely in P4 index 255.
pub const USER_SPACE_START: u64 = (USER_P4_START as u64) << 39;
pub const USER_SPACE_END: u64 = ((USER_P4_END as u64) << 39) - 0x1000;

pub fn is_user_range(start: VirtAddr, size: u64) -> bool {
    if size == 0 {
        return true;
    }

    let start_u64 = start.as_u64();

    let Some(end_u64) = start_u64.checked_add(size - 1) else {
        return false;
    };

    let start_p4 = (start_u64 >> 39) & 0x1FF;
    let end_p4 = (end_u64 >> 39) & 0x1FF;

    start_p4 >= USER_P4_START as u64 && end_p4 < USER_P4_END as u64
}

pub struct AddressSpace {
    pub p4_frame: PhysFrame<Size4KiB>,
    pub vmas: Vec<VmArea>,
    pub heap_break: VirtAddr,
}

impl AddressSpace {
    pub fn new(fa: &mut impl FrameAllocator<Size4KiB>, off: VirtAddr) -> Self {
        let p4_frame = fa
            .allocate_frame()
            .expect("AddressSpace::new: failed to allocate P4 frame");

        let p4_ptr = (off + p4_frame.start_address().as_u64()).as_mut_ptr::<PageTable>();

        // The new P4 frame may contain garbage.
        unsafe {
            ptr::write_bytes(
                p4_ptr as *mut u8,
                0,
                core::mem::size_of::<PageTable>(),
            );
        }

        let p4: &mut PageTable = unsafe { &mut *p4_ptr };
        let kernel_p4 = unsafe { active_level_4_table(off) };

        // The kernel image and statics still use low-half mappings; keep the
        // private user P4 slot separate.
        for i in 0..KERNEL_P4_START {
            if i >= USER_P4_START && i < USER_P4_END {
                continue;
            }

            p4[i] = kernel_p4[i].clone();
        }

        // Clone higher-half kernel mappings.
        for i in KERNEL_P4_START..512 {
            p4[i] = kernel_p4[i].clone();
        }

        AddressSpace {
            p4_frame,
            vmas: Vec::new(),
            heap_break: VirtAddr::new(USER_SPACE_START),
        }
    }

    /// Build a mapper for this address space, active or not.
    /// The mutable borrow prevents simultaneous mappers for the same space.
    pub unsafe fn mapper(&mut self, off: VirtAddr) -> OffsetPageTable<'_> {
        let p4_ptr = (off + self.p4_frame.start_address().as_u64()).as_mut_ptr();
        let p4: &mut PageTable = &mut *p4_ptr;
        unsafe { OffsetPageTable::new(p4, off) }
    }

    pub fn cr3(&self) -> PhysFrame<Size4KiB> {
        self.p4_frame
    }

    pub fn find_free_region(&self, size: u64) -> Option<VirtAddr> {
        let mut sorted_vmas = self.vmas.clone();
        sorted_vmas.sort_by_key(|v| v.start.as_u64());

        let mut current = USER_SPACE_START;

        for vma in sorted_vmas {
            let vma_start = vma.start.as_u64();

            let aligned_current = (current + 0xFFF) & !0xFFF;

            if aligned_current + size <= vma_start {
                return Some(VirtAddr::new(aligned_current));
            }

            current = vma_start + vma.size;
        }

        let aligned_current = (current + 0xFFF) & !0xFFF;

        if aligned_current + size <= USER_SPACE_END {
            Some(VirtAddr::new(aligned_current))
        } else {
            None
        }
    }

    pub fn get_vma_flags(&self, addr: VirtAddr) -> Option<PageTableFlags> {
        self.vmas
            .iter()
            .find(|v| {
                let start = v.start.as_u64();
                let end = start + v.size;
                let a = addr.as_u64();
                a >= start && a < end
            })
            .map(|v| v.flags)
    }

    pub fn get_vma(&self, addr: VirtAddr) -> Option<&VmArea> {
        self.vmas.iter().find(|v| v.contains(addr))
    }

    /// Find a free memory region starting from the top of user space downwards.
    /// Used primarily for allocating user stacks.
    pub fn find_free_region_top_down(&self, size: u64) -> Option<VirtAddr> {
        let mut sorted_vmas = self.vmas.clone();
        sorted_vmas.sort_by_key(|v| core::cmp::Reverse(v.start.as_u64()));

        let mut current = USER_SPACE_END;

        for vma in sorted_vmas {
            let vma_end = vma.start.as_u64() + vma.size;

            let aligned_end = current & !0xFFF;
            let region_start = aligned_end.checked_sub(size)?;

            if region_start >= vma_end {
                return Some(VirtAddr::new(region_start));
            }

            current = vma.start.as_u64();
        }

        let aligned_end = current & !0xFFF;
        let region_start = aligned_end.checked_sub(size)?;

        if region_start >= USER_SPACE_START {
            Some(VirtAddr::new(region_start))
        } else {
            None
        }
    }

    pub fn translate(
        &self,
        physical_memory_offset: VirtAddr,
        addr: VirtAddr,
    ) -> Option<PhysAddr> {
        use x86_64::structures::paging::page_table::FrameError;

        let mut frame = self.p4_frame;

        for &index in &[
            addr.p4_index(),
            addr.p3_index(),
            addr.p2_index(),
            addr.p1_index(),
        ] {
            let table_virt = physical_memory_offset + frame.start_address().as_u64();
            let table_ptr = table_virt.as_ptr::<PageTable>();
            let table = unsafe { &*table_ptr };

            let entry = &table[index];

            match entry.frame() {
                Ok(next_frame) => frame = next_frame,
                Err(FrameError::FrameNotPresent) => return None,
                Err(FrameError::HugeFrame) => {
                    // Huge pages are not supported by this loader.
                    return None;
                }
            }
        }

        Some(frame.start_address() + u64::from(addr.page_offset()))
    }
}
