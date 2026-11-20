use super::MemoryManager;
use crate::memory::address_space::AddressSpace;
use crate::memory::vma::{VmArea, VmaKind};
use x86_64::{
    structures::paging::{Page, PageTableFlags, Size4KiB, Mapper, FrameAllocator},
    VirtAddr,
};
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Oom;

impl MemoryManager {
    pub fn mmap(
        &mut self,
        space: &mut AddressSpace,
        size: usize,
        flags: PageTableFlags,
        kind: VmaKind,
    ) -> Result<VirtAddr, Oom> {
        if size == 0 {
            return Err(Oom);
        }
        let start = space.find_free_region(size as u64).ok_or(Oom)?;
        if !crate::memory::address_space::is_user_range(start, size as u64) {
            return Err(Oom);
        }
        let start_page = Page::<Size4KiB>::containing_address(start);
        let end_page = Page::<Size4KiB>::containing_address(start + (size as u64 - 1));

        let mut frames = Vec::new();
        for _ in Page::range_inclusive(start_page, end_page) {
            match self.frame_allocator.allocate_frame() {
                Some(f) => frames.push(f),
                None => {
                    // Rollback: free any frames we already allocated
                    for f in frames {
                        unsafe { self.frame_allocator.free_frame(f); }
                    }
                    return Err(Oom);
                }
            }
        }

        let parent_flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;
        let mut mapper = unsafe { space.mapper(self.physical_memory_offset) };
        let mut frames_iter = frames.into_iter();
        let mut mapped_pages = Vec::new();
        
        for page in Page::range_inclusive(start_page, end_page) {
            let frame = frames_iter.next().unwrap();
            unsafe {
                match mapper.map_to_with_table_flags(
                    page, frame, flags | PageTableFlags::USER_ACCESSIBLE,
                    parent_flags, &mut self.frame_allocator,
                ) {
                    Ok(flush) => {
                        flush.flush();
                        mapped_pages.push((page, frame));
                    }
                    Err(_) => {
                        // Rollback successfully mapped pages
                        for (mapped_page, _) in mapped_pages {
                            if let Ok((unmapped_frame, flush)) = mapper.unmap(mapped_page) {
                                flush.flush();
                                self.frame_allocator.free_frame(unmapped_frame);
                            }
                        }
                        // Free the frame that failed to map
                        self.frame_allocator.free_frame(frame);
                        // Free any remaining unallocated frames
                        for remaining_frame in frames_iter {
                            self.frame_allocator.free_frame(remaining_frame);
                        }
                        return Err(Oom);
                    }
                }
            }
        }

        space.vmas.push(VmArea { start, size: size as u64, flags, kind });
        Ok(start)
    }

    pub fn mmap_at(
        &mut self,
        space: &mut AddressSpace,
        start: VirtAddr,
        size: usize,
        flags: PageTableFlags,
        kind: VmaKind,
    ) -> Result<(), Oom> {
        if size == 0 {
            return Ok(());
        }
        let end_addr = start + size as u64;
        for vma in &space.vmas {
            let vma_end = vma.start + vma.size;
            if start < vma_end && end_addr > vma.start {
                return Err(Oom);
            }
        }
        if !crate::memory::address_space::is_user_range(start, size as u64) {
            return Err(Oom);
        }
        
        let parent_flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;
        let mut mapper = unsafe { space.mapper(self.physical_memory_offset) };
        let start_page = Page::<Size4KiB>::containing_address(start);
        let end_page = Page::<Size4KiB>::containing_address(start + (size as u64 - 1));
        
        let mut mapped_pages = Vec::new();
        for page in Page::range_inclusive(start_page, end_page) {
            let frame = match self.frame_allocator.allocate_frame() {
                Some(f) => f,
                None => {
                    // Rollback previously mapped pages if allocation fails
                    for (mapped_page, _) in mapped_pages {
                        if let Ok((unmapped_frame, flush)) = mapper.unmap(mapped_page) {
                            flush.flush();
                            unsafe {
                                self.frame_allocator.free_frame(unmapped_frame);
                            };  
                        }
                    }
                    return Err(Oom);
                }
            };

            unsafe {
                match mapper.map_to_with_table_flags(
                    page, frame, flags | PageTableFlags::USER_ACCESSIBLE,
                    parent_flags, &mut self.frame_allocator,
                ) {
                    Ok(flush) => {
                        flush.flush();
                        mapped_pages.push((page, frame));
                    }
                    Err(_) => {
                        // Rollback successfully mapped pages
                        for (mapped_page, _) in mapped_pages {
                            if let Ok((unmapped_frame, flush)) = mapper.unmap(mapped_page) {
                                flush.flush();
                                self.frame_allocator.free_frame(unmapped_frame);
                            }
                        }
                        // Free the frame that failed to map
                        self.frame_allocator.free_frame(frame);
                        return Err(Oom);
                    }
                }
            }
        }
        
        space.vmas.push(VmArea { start, size: size as u64, flags, kind });
        Ok(())
    }

    /// Unmap pages and shrink or split overlapping VMAs.
    pub fn munmap(&mut self, space: &mut AddressSpace, addr: VirtAddr, size: usize) {
        if size == 0 { return; }

        let mut mapper = unsafe { space.mapper(self.physical_memory_offset) };
        let start_page = Page::<Size4KiB>::containing_address(addr);
        let end_page = Page::<Size4KiB>::containing_address(addr + (size as u64 - 1));
        
        for page in Page::range_inclusive(start_page, end_page) {
            if let Ok((frame, flush)) = mapper.unmap(page) {
                unsafe { self.frame_allocator.free_frame(frame); }
                flush.flush();
            }
        }
        
        let unmap_start = addr.as_u64();
        let unmap_end = unmap_start + size as u64;

        let mut new_vmas = Vec::new();
        for mut vma in space.vmas.drain(..) {
            let vma_start = vma.start.as_u64();
            let vma_end = vma_start + vma.size;
            
            if vma_start >= unmap_start && vma_end <= unmap_end {
                continue; // Completely covered, drop it
            } else if vma_start < unmap_start && vma_end > unmap_start && vma_end <= unmap_end {
                vma.size = unmap_start - vma_start; // Shrink from end
                new_vmas.push(vma);
            } else if vma_start >= unmap_start && vma_start < unmap_end && vma_end > unmap_end {
                vma.size = vma_end - unmap_end; // Shrink from start
                vma.start = VirtAddr::new(unmap_end);
                new_vmas.push(vma);
            } else if vma_start < unmap_start && vma_end > unmap_end {
                // Split in the middle
                let left_size = unmap_start - vma_start;
                let right_start = unmap_end;
                let right_size = vma_end - unmap_end;
                
                new_vmas.push(VmArea { start: vma.start, size: left_size, flags: vma.flags, kind: vma.kind });
                new_vmas.push(VmArea { start: VirtAddr::new(right_start), size: right_size, flags: vma.flags, kind: vma.kind });
            } else {
                new_vmas.push(vma); // No overlap
            }
        }
        space.vmas = new_vmas;
    }

    /// Register a VMA without mapping physical frames (used for setting up demand paging regions).
    pub fn register_vma(
        &mut self,
        space: &mut AddressSpace,
        start: VirtAddr,
        size: usize,
        flags: PageTableFlags,
        kind: VmaKind,
    ) {
        assert!(
            crate::memory::address_space::is_user_range(start, size as u64),
            "register_vma: address {:#x} size {:#x} is outside the private user P4 range",
            start.as_u64(),
            size
        );

        space.vmas.push(VmArea {
            start,
            size: size as u64,
            flags,
            kind,
        });
    }
}