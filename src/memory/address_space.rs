// src/memory/address_space.rs

use alloc::vec::Vec;
use x86_64::{
    structures::paging::{
        FrameAllocator, PhysFrame, Size4KiB, PageTable, OffsetPageTable, Page, PageTableFlags, Mapper,
    },
    VirtAddr,
};

use crate::memory::paging::active_level_4_table;
use crate::memory::vma::VmArea; // Required for tracking VMAs

pub struct AddressSpace { 
    pub p4_frame: PhysFrame<Size4KiB>,
    pub vmas: Vec<VmArea>,
    pub heap_break: VirtAddr, // Added for Milestone 4.2
}

impl AddressSpace {
    pub fn new(fa: &mut impl FrameAllocator<Size4KiB>, off: VirtAddr) -> Self {
        let p4_frame = fa.allocate_frame().unwrap();
        let p4: &mut PageTable = unsafe { &mut *(off + p4_frame.start_address().as_u64()).as_mut_ptr() };
        let kernel_p4 = unsafe { active_level_4_table(off) };

        // Copy EVERYTHING from the kernel's table: identity-mapped low
        // memory (VGA buffer etc. at index 0), the physical-memory offset
        // window, and the higher-half kernel range. This gives the new
        // space full parity with the kernel until you actually start
        // removing/replacing entries for real user isolation.
        for i in 0..512 {
            p4[i] = kernel_p4[i].clone();
        }

        AddressSpace { 
            p4_frame,
            vmas: Vec::new(), 
            heap_break: VirtAddr::new(0x600000),
        }
    }

    /// Build a mapper for an address space that may or may not be active.
    /// Tied to &mut self to prevent multiple simultaneous mutable borrows.
    pub unsafe fn mapper(&mut self, off: VirtAddr) -> OffsetPageTable<'_> {
        let p4_ptr = (off + self.p4_frame.start_address().as_u64()).as_mut_ptr();
        let p4: &mut PageTable = &mut *p4_ptr;
        unsafe { OffsetPageTable::new(p4, off) }
    }

    pub fn cr3(&self) -> PhysFrame<Size4KiB> {
        self.p4_frame
    }

    pub fn find_free_region(&self, size: u64) -> Option<VirtAddr> {
        // Standard x86_64 user space bounds
        const USER_SPACE_START: u64 = 0x400000;             // 4MB typical start
        const USER_SPACE_END: u64 = 0x0000_7FFF_FFFF_F000;  // Strict canonical limit

        let mut sorted_vmas = self.vmas.clone();
        sorted_vmas.sort_by_key(|v| v.start.as_u64());

        let mut current = USER_SPACE_START;

        for vma in sorted_vmas {
            let vma_start = vma.start.as_u64();
            
            // Align current up to page boundary
            let aligned_current = (current + 0xFFF) & !0xFFF;
            
            // Check if the gap before this VMA is large enough
            if aligned_current + size <= vma_start {
                return Some(VirtAddr::new(aligned_current));
            }
            
            // Move current past this VMA
            current = vma_start + vma.size;
        }

        // Check space after the last VMA
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
        const USER_SPACE_START: u64 = 0x400000;
        const USER_SPACE_END: u64 = 0x0000_7FFF_FFFF_F000;

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

}

/// Build a mapper for a not-yet-active address space.
pub fn mapper_for_address_space(
    p4_frame: PhysFrame<Size4KiB>,
    physical_memory_offset: VirtAddr,
) -> OffsetPageTable<'static> {
    let p4_ptr = (physical_memory_offset + p4_frame.start_address().as_u64())
        .as_mut_ptr();
    let p4: &'static mut PageTable = unsafe { &mut *p4_ptr };
    unsafe { OffsetPageTable::new(p4, physical_memory_offset) }
}

/// Maps a region of memory into an address space, correctly propagating USER_ACCESSIBLE.
pub fn map_user_region(
    space: &mut AddressSpace,
    fa: &mut impl FrameAllocator<Size4KiB>,
    off: VirtAddr,
    start: VirtAddr,
    size: usize,
    leaf_flags: PageTableFlags,
) -> Vec<PhysFrame<Size4KiB>> {
    assert!(size > 0);
    assert!(start.as_u64() % 4096 == 0, "start must be page-aligned");
    assert!(size % 4096 == 0, "size must be page-aligned");

    let parent_flags = PageTableFlags::PRESENT
                     | PageTableFlags::WRITABLE
                     | PageTableFlags::USER_ACCESSIBLE;

    let mut mapper = unsafe { space.mapper(off) };
    let mut frames = Vec::new();

    let start_page = Page::<Size4KiB>::containing_address(start);
    let end_page   = Page::<Size4KiB>::containing_address(start + (size as u64 - 1));
    
    for page in Page::range_inclusive(start_page, end_page) {
        let frame = fa.allocate_frame().expect("map_user_region: out of frames");
        unsafe {
            mapper
                .map_to_with_table_flags(
                    page,
                    frame,
                    leaf_flags | PageTableFlags::USER_ACCESSIBLE,
                    parent_flags,
                    fa,
                )
                .expect("map_to failed")
                .flush();
        }
        frames.push(frame);
    }
    frames
}