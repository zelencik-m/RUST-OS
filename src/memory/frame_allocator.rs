// src/memory/frame_allocator.rs

use bootloader::bootinfo::{MemoryMap, MemoryRegionType};
use x86_64::{
    structures::paging::{FrameAllocator, PhysFrame, Size4KiB},
    PhysAddr,
};
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;

lazy_static! {
    pub static ref FRAME_ALLOCATOR: Mutex<Option<BootInfoFrameAllocator>> = Mutex::new(None);
}

/// Initialize the global frame allocator.
///
/// # Safety
/// Must be called exactly once, early in the kernel, with a valid memory map
/// whose `Usable` frames are truly unused.
pub unsafe fn init_global_allocator(memory_map: &'static MemoryMap) {
    *FRAME_ALLOCATOR.lock() = Some(BootInfoFrameAllocator::init(memory_map));
}

pub struct EmptyFrameAllocator;

unsafe impl FrameAllocator<Size4KiB> for EmptyFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        None
    }
}

pub struct BootInfoFrameAllocator {
    memory_map: &'static MemoryMap,
    next: usize,
    freed: Vec<PhysFrame<Size4KiB>>,
}

impl BootInfoFrameAllocator {
    /// # Safety
    /// The caller must guarantee that all `Usable` frames are truly unused,
    /// and that this function is called only once with a valid memory map.
    pub unsafe fn init(memory_map: &'static MemoryMap) -> Self {
        BootInfoFrameAllocator {
            memory_map,
            next: 0,
            freed: Vec::new(),
        }
    }

    /// Returns an iterator over all usable physical frames in the map.
    fn usable_frames(&self) -> impl Iterator<Item = PhysFrame> {
        self.memory_map
            .iter()
            .filter(|r| r.region_type == MemoryRegionType::Usable)
            .map(|r| r.range.start_addr()..r.range.end_addr())
            .flat_map(|r| r.step_by(4096))
            .map(|addr| PhysFrame::containing_address(PhysAddr::new(addr)))
    }

    /// Release a frame back to the allocator.
    ///
    /// # Safety
    /// The frame must have been previously allocated by this allocator,
    /// must not still be in use, and must not be freed twice.
    pub unsafe fn free_frame(&mut self, frame: PhysFrame<Size4KiB>) {
        self.freed.push(frame);
    }
}

unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        // Give back a freed frame first, if any.
        if let Some(frame) = self.freed.pop() {
            return Some(frame);
        }
        // Otherwise take the next untouched frame from the memory map.
        let frame = self.usable_frames().nth(self.next);
        self.next += 1;
        frame
    }
}