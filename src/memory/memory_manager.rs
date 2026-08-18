// src/memory/memory_manager.rs

use memory::BootInfoFrameAllocator;
use bootloader::bootinfo::MemoryMap;
use x86_64::structures::paging::{mapper::MapToError, OffsetPageTable, Size4KiB, Page, PageTableFlags};
use x86_64::VirtAddr;
use spin::Mutex;
use x86_64::structures::paging::FrameAllocator;
use x86_64::structures::paging::Mapper;

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

        MemoryManager {
            mapper,
            frame_allocator,
            physical_memory_offset: phys_mem_offset,
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

    /// Map a new contiguous region into the address space.
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
        
        let parent_flags = PageTableFlags::PRESENT
                         | PageTableFlags::WRITABLE
                         | PageTableFlags::USER_ACCESSIBLE;
                         
        let mut mapper = unsafe { space.mapper(self.physical_memory_offset) };
        
        let start_page = Page::<Size4KiB>::containing_address(start);
        let end_page = Page::<Size4KiB>::containing_address(start + (size as u64 - 1));
        
        for page in Page::range_inclusive(start_page, end_page) {
            let frame = self.frame_allocator.allocate_frame().ok_or(Oom)?;
            unsafe {
                mapper.map_to_with_table_flags(
                    page,
                    frame,
                    flags | PageTableFlags::USER_ACCESSIBLE,
                    parent_flags,
                    &mut self.frame_allocator,
                ).map_err(|_| Oom)?.flush();
            }
        }
        
        space.vmas.push(VmArea {
            start,
            size: size as u64,
            flags,
            kind,
        });
        
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

    /// Unmap a region, freeing its frames and removing/splitting VMAs.
    pub fn munmap(
        &mut self,
        space: &mut AddressSpace,
        addr: VirtAddr,
        size: usize,
    ) {
        if size == 0 {
            return;
        }

        let mut mapper = unsafe { space.mapper(self.physical_memory_offset) };
        
        let start_page = Page::<Size4KiB>::containing_address(addr);
        let end_page = Page::<Size4KiB>::containing_address(addr + (size as u64 - 1));
        
        for page in Page::range_inclusive(start_page, end_page) {
            if let Ok((frame, flush)) = mapper.unmap(page) {
                unsafe {
                    self.frame_allocator.free_frame(frame);
                }
                flush.flush();
            }
        }
        
        let unmap_start = addr.as_u64();
        let unmap_end = unmap_start + size as u64;

        // Remove VMAs completely covered by the unmap request
        space.vmas.retain(|vma| {
            let vma_start = vma.start.as_u64();
            let vma_end = vma_start + vma.size;
            !(vma_start >= unmap_start && vma_end <= unmap_end)
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

    /// Handle a page fault for a specific address space.
    /// Returns `FaultAction::Resolved` if the fault was handled (demand paged),
    /// `FaultAction::Segfault` if the address is out of bounds,
    /// or `PageFaultError` if an error occurred (e.g., OOM).
    pub fn handle_page_fault(
        &mut self,
        space: &mut AddressSpace,
        fault_addr: VirtAddr,
    ) -> Result<FaultAction, PageFaultError> {
        let page_addr = Page::<Size4KiB>::containing_address(fault_addr).start_address();
        
        if let Some(leaf_flags) = space.get_vma_flags(fault_addr) {
            // In a VMA, demand map it
            self.demand_map_page(space, page_addr, leaf_flags)
                .map(|_| {
                    // Zero the page to prevent data leaks
                    unsafe { core::ptr::write_bytes(page_addr.as_mut_ptr::<u8>(), 0, 4096); }
                    FaultAction::Resolved
                })
                .map_err(|_| PageFaultError)
        } else {
            // Out of bounds
            Ok(FaultAction::Segfault)
        }
    }
}