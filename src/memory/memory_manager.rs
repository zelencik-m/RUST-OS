// src/memory/memory_manager.rs

use crate::memory;
use memory::BootInfoFrameAllocator;
use bootloader::bootinfo::MemoryMap;
use x86_64::structures::paging::{mapper::MapToError, OffsetPageTable, Size4KiB};
use x86_64::VirtAddr;
use spin::Mutex;
use crate::memory::address_space::AddressSpace;

pub struct MemoryManager {
    pub mapper: OffsetPageTable<'static>,
    pub frame_allocator: BootInfoFrameAllocator,
    physical_memory_offset: VirtAddr,
}

pub static MEMORY_MANAGER: Mutex<Option<MemoryManager>> = Mutex::new(None);

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
}