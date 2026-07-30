// src/memory_manager.rs

use crate::memory;

use memory::BootInfoFrameAllocator;
use bootloader::bootinfo::MemoryMap;
use x86_64::structures::paging::{Size4KiB, mapper::MapToError};

use x86_64::{
    structures::paging::OffsetPageTable,
    VirtAddr,
};

pub struct MemoryManager{
    mapper: OffsetPageTable<'static>,
    frame_allocator: BootInfoFrameAllocator,
}

impl MemoryManager{
    pub unsafe fn new(
        physical_memory_offset: u64,
        memory_map: &'static MemoryMap,
    ) -> MemoryManager {

        let phys_mem_offset = VirtAddr::new(physical_memory_offset);
        let mapper = memory::init(phys_mem_offset);
        let frame_allocator = BootInfoFrameAllocator::init(memory_map);


        MemoryManager { mapper, frame_allocator }
    }


    pub fn init_heap(&mut self) -> Result<(), MapToError<Size4KiB>> {
        memory::allocator::init_heap(&mut self.mapper, &mut self.frame_allocator)
    }


}