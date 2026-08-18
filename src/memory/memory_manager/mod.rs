// src/memory/memory_manager/mod.rs

use bootloader::bootinfo::MemoryMap;
use x86_64::structures::paging::{OffsetPageTable, Size4KiB, PhysFrame, mapper::MapToError};
use x86_64::VirtAddr;
use crate::sync::IrqSafeMutex;
use crate::memory::BootInfoFrameAllocator;

// Import submodules so their `impl` blocks are compiled
mod lifecycle;
mod vmm;
mod fault;
mod allocators;

// Re-export public types/enums so `use crate::memory::memory_manager::Oom` still works
pub use vmm::Oom;
pub use fault::{FaultAction, PageFaultError};

pub struct MemoryManager {
    pub mapper: OffsetPageTable<'static>,
    pub frame_allocator: BootInfoFrameAllocator,
    pub physical_memory_offset: VirtAddr,
    pub kernel_p4_frame: PhysFrame<Size4KiB>,
}

pub static MEMORY_MANAGER: IrqSafeMutex<Option<MemoryManager>> = IrqSafeMutex::new(None);

impl MemoryManager {
    pub unsafe fn new(
        physical_memory_offset: u64,
        memory_map: &'static MemoryMap,
    ) -> Self {
        let phys_mem_offset = VirtAddr::new(physical_memory_offset);
        let mapper = crate::memory::paging::init(phys_mem_offset);
        let frame_allocator = BootInfoFrameAllocator::init(memory_map);
        let (kernel_p4_frame, _) = x86_64::registers::control::Cr3::read();

        MemoryManager {
            mapper,
            frame_allocator,
            physical_memory_offset: phys_mem_offset,
            kernel_p4_frame,
        }
    }

    pub fn init_heap(&mut self) -> Result<(), MapToError<Size4KiB>> {
        crate::memory::allocator::init_heap(&mut self.mapper, &mut self.frame_allocator)
    }
}