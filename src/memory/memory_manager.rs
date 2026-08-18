// src/memory/memory_manager.rs

use memory::BootInfoFrameAllocator;
use bootloader::bootinfo::MemoryMap;
use x86_64::structures::paging::{mapper::MapToError, OffsetPageTable, Size4KiB, Page, PageTableFlags, PageTable, PhysFrame, page_table::FrameError};
use x86_64::VirtAddr;
use x86_64::structures::paging::FrameAllocator;
use x86_64::structures::paging::Mapper;
use alloc::vec::Vec;

use crate::memory;
use crate::memory::address_space::AddressSpace;
use crate::memory::vma::{VmArea, VmaKind};
use crate::sync::{IrqSafeMutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Oom;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultAction {
    Resolved,
    Segfault,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageFaultError;

pub struct MemoryManager {
    pub mapper: OffsetPageTable<'static>,
    pub frame_allocator: BootInfoFrameAllocator,
    pub physical_memory_offset: VirtAddr,
    pub kernel_p4_frame: PhysFrame<Size4KiB>, // Added for Milestone 5.2
}

pub static MEMORY_MANAGER: IrqSafeMutex<Option<MemoryManager>> = IrqSafeMutex::new(None);

impl MemoryManager {
    pub unsafe fn new(
        physical_memory_offset: u64,
        memory_map: &'static MemoryMap,
    ) -> Self {
        let phys_mem_offset = VirtAddr::new(physical_memory_offset);
        let mapper = memory::paging::init(phys_mem_offset);
        let frame_allocator = BootInfoFrameAllocator::init(memory_map);
        let (kernel_p4_frame, _) = x86_64::registers::control::Cr3::read(); // Save boot CR3

        MemoryManager {
            mapper,
            frame_allocator,
            physical_memory_offset: phys_mem_offset,
            kernel_p4_frame,
        }
    }

    pub fn init_heap(&mut self) -> Result<(), MapToError<Size4KiB>> {
        memory::allocator::init_heap(&mut self.mapper, &mut self.frame_allocator)
    }

    pub fn create_address_space(&mut self) -> AddressSpace {
        AddressSpace::new(
            &mut self.frame_allocator,
            self.physical_memory_offset,
        )
    }

    pub fn mmap(
        &mut self,
        space: &mut AddressSpace,
        size: usize,
        flags: PageTableFlags,
        kind: VmaKind,
    ) -> Result<VirtAddr, Oom> {
        if size == 0 { return Err(Oom); }
        
        let start = space.find_free_region(size as u64).ok_or(Oom)?;
        let start_page = Page::<Size4KiB>::containing_address(start);
        let end_page = Page::<Size4KiB>::containing_address(start + (size as u64 - 1));
        
        // 1. Allocate all frames first (Transactional)
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

        // 2. Now map them (Mapping rarely fails unless there's a bug in our logic)
        let parent_flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;
        let mut mapper = unsafe { space.mapper(self.physical_memory_offset) };
        
        for (page, frame) in Page::range_inclusive(start_page, end_page).zip(frames.into_iter()) {
            unsafe {
                mapper.map_to_with_table_flags(
                    page, frame, flags | PageTableFlags::USER_ACCESSIBLE,
                    parent_flags, &mut self.frame_allocator,
                ).map_err(|_| Oom)?.flush();
            }
        }
        
        // 3. Commit the VMA
        space.vmas.push(VmArea { start, size: size as u64, flags, kind });
        Ok(start)
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
        space.vmas.push(VmArea {
            start,
            size: size as u64,
            flags,
            kind,
        });
    }

    /// Map a single page on demand (used by the page fault handler).
    pub fn demand_map_page(
        &mut self,
        space: &mut AddressSpace,
        page_addr: VirtAddr,
        leaf_flags: PageTableFlags,
    ) -> Result<(), Oom> {
        let frame = self.frame_allocator.allocate_frame().ok_or(Oom)?;

        let parent_flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::USER_ACCESSIBLE;

        let page = Page::<Size4KiB>::containing_address(page_addr);
        let mut mapper = unsafe { space.mapper(self.physical_memory_offset) };

        unsafe {
            mapper
                .map_to_with_table_flags(
                    page,
                    frame,
                    leaf_flags | PageTableFlags::USER_ACCESSIBLE,
                    parent_flags,
                    &mut self.frame_allocator,
                )
                .map_err(|_| Oom)?
                .flush();
        }
        
        Ok(())
    }

    pub fn mmap_at(
        &mut self,
        space: &mut AddressSpace,
        start: VirtAddr,
        size: usize,
        flags: PageTableFlags,
        kind: VmaKind,
    ) -> Result<(), Oom> {
        if size == 0 { return Ok(()); }
        
        let end_addr = start + size as u64;
        for vma in &space.vmas {
            let vma_end = vma.start + vma.size;
            if start < vma_end && end_addr > vma.start {
                // Overlap detected! In a real OS, you might unmap the old one first,
                // but for now, returning an error is safer.
                return Err(Oom); // Or create a specific MmapError::Overlap
            }
        }

        let parent_flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;
        let mut mapper = unsafe { space.mapper(self.physical_memory_offset) };
        
        let start_page = Page::<Size4KiB>::containing_address(start);
        let end_page = Page::<Size4KiB>::containing_address(start + (size as u64 - 1));
        
        for page in Page::range_inclusive(start_page, end_page) {
            let frame = self.frame_allocator.allocate_frame().ok_or(Oom)?;
            unsafe {
                mapper.map_to_with_table_flags(
                    page, frame, flags | PageTableFlags::USER_ACCESSIBLE,
                    parent_flags, &mut self.frame_allocator,
                ).map_err(|_| Oom)?.flush();
            }
        }
        
        space.vmas.push(VmArea { start, size: size as u64, flags, kind });
        Ok(())
    }

    /// Upgraded munmap that correctly shrinks and splits VMAs.
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

        // Smart VMA shrink/split logic
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

    pub fn allocate_user_stack(&mut self, space: &mut AddressSpace) -> Result<VirtAddr, Oom> {
        const STACK_PAGES: usize = 16; // 64KB stack
        const GUARD_PAGES: usize = 1;
        let total_pages = STACK_PAGES + GUARD_PAGES;
        let total_size = total_pages * 4096;

        let start = space.find_free_region_top_down(total_size as u64).ok_or(Oom)?;

        // 1. Register Guard Page (Bottom-most page)
        space.vmas.push(VmArea {
            start,
            size: 4096,
            flags: PageTableFlags::USER_ACCESSIBLE, // NOT Present
            kind: VmaKind::Guard,
        });

        // 2. Map Stack Pages
        let stack_start = start + 4096u64;
        let stack_size = STACK_PAGES * 4096;
        
        self.mmap_at(
            space, stack_start, stack_size, 
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE, 
            VmaKind::Stack
        )?;

        // 3. Return initial RSP (Top of the stack)
        Ok(stack_start + stack_size as u64)
    }

    /// Handle a page fault for a specific address space.
    pub fn handle_page_fault(
        &mut self,
        space: &mut AddressSpace,
        fault_addr: VirtAddr,
    ) -> Result<FaultAction, PageFaultError> {
        let page_addr = Page::<Size4KiB>::containing_address(fault_addr).start_address();
        
        if let Some(vma) = space.get_vma(fault_addr) {
            // Catch stack overflows hitting the guard page
            if vma.kind == VmaKind::Guard {
                return Ok(FaultAction::Segfault);
            }
            
            // In a VMA, demand map it
            self.demand_map_page(space, page_addr, vma.flags)
                .map(|_| {
                    unsafe { core::ptr::write_bytes(page_addr.as_mut_ptr::<u8>(), 0, 4096); }
                    FaultAction::Resolved
                })
                .map_err(|_| PageFaultError)
        } else {
            Ok(FaultAction::Segfault)
        }
    }

    pub fn destroy_address_space(&mut self, space: &mut AddressSpace) {
        let p4_frame = space.p4_frame;
        let p4_ptr = (self.physical_memory_offset + p4_frame.start_address().as_u64()).as_mut_ptr();
        let p4: &mut PageTable = unsafe { &mut *p4_ptr };

        // Iterate only over the lower half (user space: indices 0..256)
        // The upper half contains cloned kernel mappings which we MUST NOT free.
        for (i, p4_entry) in p4.iter_mut().enumerate() {
            if i >= 256 { break; }
            
            match p4_entry.frame() {
                Ok(p3_frame) => {
                    let p3_ptr = (self.physical_memory_offset + p3_frame.start_address().as_u64()).as_mut_ptr();
                    let p3: &mut PageTable = unsafe { &mut *p3_ptr };
                    
                    for p3_entry in p3.iter_mut() {
                        match p3_entry.frame() {
                            Ok(p2_frame) => {
                                let p2_ptr = (self.physical_memory_offset + p2_frame.start_address().as_u64()).as_mut_ptr();
                                let p2: &mut PageTable = unsafe { &mut *p2_ptr };
                                
                                for p2_entry in p2.iter_mut() {
                                    match p2_entry.frame() {
                                        Ok(p1_frame) => {
                                            let p1_ptr = (self.physical_memory_offset + p1_frame.start_address().as_u64()).as_mut_ptr();
                                            let p1: &mut PageTable = unsafe { &mut *p1_ptr };
                                            
                                            for p1_entry in p1.iter_mut() {
                                                if let Ok(frame) = p1_entry.frame() {
                                                    unsafe { self.frame_allocator.free_frame(frame); }
                                                }
                                            }
                                            unsafe { self.frame_allocator.free_frame(p1_frame); }
                                        }
                                        Err(FrameError::HugeFrame) => {} // 2MB pages not currently used
                                        Err(FrameError::FrameNotPresent) => {}
                                    }
                                }
                                unsafe { self.frame_allocator.free_frame(p2_frame); }
                            }
                            Err(FrameError::HugeFrame) => {} // 1GB pages not currently used
                            Err(FrameError::FrameNotPresent) => {}
                        }
                    }
                    unsafe { self.frame_allocator.free_frame(p3_frame); }
                }
                Err(FrameError::HugeFrame) => {}
                Err(FrameError::FrameNotPresent) => {}
            }
        }
        
        // Finally, free the P4 frame itself
        unsafe { self.frame_allocator.free_frame(p4_frame); }
        space.vmas.clear();
    }

    pub fn sys_brk(
        &mut self,
        space: &mut AddressSpace,
        new_break: VirtAddr,
    ) -> Result<VirtAddr, Oom> {
        let current_break = space.heap_break;
        if new_break == current_break { return Ok(current_break); }
        
        if new_break > current_break {
            let map_start = VirtAddr::new((current_break.as_u64() + 0xFFF) & !0xFFF);
            let map_end = VirtAddr::new((new_break.as_u64() + 0xFFF) & !0xFFF);
            
            if map_start < map_end {
                let size = (map_end.as_u64() - map_start.as_u64()) as usize;
                self.mmap_at(
                    space, map_start, size,
                    PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE,
                    VmaKind::Heap,
                )?;
            }
            space.heap_break = new_break;
            Ok(new_break)
        } else {
            let unmap_start = VirtAddr::new((new_break.as_u64() + 0xFFF) & !0xFFF);
            let unmap_end = VirtAddr::new((current_break.as_u64() + 0xFFF) & !0xFFF);
            
            if unmap_start < unmap_end {
                let size = (unmap_end.as_u64() - unmap_start.as_u64()) as usize;
                self.munmap(space, unmap_start, size);
            }
            space.heap_break = new_break;
            Ok(new_break)
        }
    }
}