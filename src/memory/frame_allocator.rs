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

enum FrameTracker {
    /// Tracking metadata is not available yet.
    ///
    /// This state exists because the frame allocator is needed to build the
    /// kernel heap, and the tracking bitmap itself needs the kernel heap.
    Bootstrap,

    /// Full allocation-state tracking is enabled.
    ///
    /// Each bit represents one 4 KiB physical frame.
    ///
    /// - bit set: frame is currently allocated
    /// - bit clear: frame is currently free
    Enabled { bitmap: Vec<u64> },
}

pub struct BootInfoFrameAllocator {
    memory_map: &'static MemoryMap,
    /// How many fresh frames (never handed out before) have been allocated.
    next: usize,
    /// Where the next fresh frame comes from: an index into the memory map
    /// and an address. Fresh frames are handed out in memory-map order, the
    /// same order as `usable_frames`, but continuing from here instead of
    /// walking the map from the start each time. That walk (`nth(next)`)
    /// made every allocation O(frames allocated so far): mapping the 10 MiB
    /// heap at boot took ~3 s under emulation, with interrupts off.
    cursor_region: usize,
    cursor_address: u64,
    freed: Vec<PhysFrame<Size4KiB>>,

    /// Highest physical frame number seen in any usable memory-map region.
    max_frame_number: u64,

    tracker: FrameTracker,
}

impl BootInfoFrameAllocator {
    /// # Safety
    /// The caller must guarantee that all `Usable` frames are truly unused,
    /// and that this function is called only once with a valid memory map.
    pub unsafe fn init(memory_map: &'static MemoryMap) -> Self {
        let max_frame_number = memory_map
            .iter()
            .filter(|r| r.region_type == MemoryRegionType::Usable)
            .filter_map(|r| {
                let start = r.range.start_addr();
                let end = r.range.end_addr();

                if end > start {
                    // Last byte in the region.
                    Some((end - 1) >> 12)
                } else {
                    None
                }
            })
            .max()
            .unwrap_or(0);

        BootInfoFrameAllocator {
            memory_map,
            next: 0,
            cursor_region: 0,
            cursor_address: 0,
            freed: Vec::new(),
            max_frame_number,
            tracker: FrameTracker::Bootstrap,
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

    /// The next fresh frame in memory-map order, advancing the cursor.
    fn next_fresh_frame(&mut self) -> Option<PhysFrame> {
        while let Some(region) = self.memory_map.get(self.cursor_region) {
            if region.region_type == MemoryRegionType::Usable {
                let start = region.range.start_addr();
                let end = region.range.end_addr();
                if self.cursor_address < start {
                    self.cursor_address = start;
                }
                if self.cursor_address < end {
                    let frame = PhysFrame::containing_address(PhysAddr::new(self.cursor_address));
                    self.cursor_address += 4096;
                    return Some(frame);
                }
            }
            self.cursor_region += 1;
            self.cursor_address = 0;
        }
        None
    }

    fn frame_number(frame: PhysFrame<Size4KiB>) -> u64 {
        frame.start_address().as_u64() >> 12
    }

    fn bit_index(frame_number: u64) -> (usize, u64) {
        let word_index = (frame_number >> 6) as usize;
        let bit_mask = 1u64 << (frame_number & 63);
        (word_index, bit_mask)
    }

    /// Sets the allocation bit for `frame_number`.
    ///
    /// Returns `true` if the bit was already set.
    fn set_bit(bitmap: &mut [u64], frame_number: u64) -> bool {
        let (word_index, bit_mask) = Self::bit_index(frame_number);
        let old = bitmap[word_index];
        let was_set = old & bit_mask != 0;
        bitmap[word_index] = old | bit_mask;
        was_set
    }

    /// Clears the allocation bit for `frame_number`.
    ///
    /// Returns `true` if the bit was set before clearing.
    fn clear_bit(bitmap: &mut [u64], frame_number: u64) -> bool {
        let (word_index, bit_mask) = Self::bit_index(frame_number);
        let old = bitmap[word_index];
        let was_set = old & bit_mask != 0;
        bitmap[word_index] = old & !bit_mask;
        was_set
    }

    /// Turn the bootstrap allocator into a fully checked allocator.
    ///
    /// This must be called after the kernel heap is initialized, because the
    /// tracking bitmap itself is allocated from the kernel heap.
    pub fn enable_tracking(&mut self) {
        if matches!(self.tracker, FrameTracker::Enabled { .. }) {
            return;
        }

        let words = (self.max_frame_number / 64 + 1) as usize;

        let mut bitmap: Vec<u64> = Vec::new();
        bitmap.resize(words, 0);

        // Mark every fresh frame handed out so far as allocated.
        for frame in self.usable_frames().take(self.next) {
            let n = Self::frame_number(frame);

            if n > self.max_frame_number {
                panic!(
                    "BootInfoFrameAllocator: allocated frame {:#x} is outside tracked range",
                    n << 12
                );
            }

            if Self::set_bit(&mut bitmap, n) {
                panic!(
                    "BootInfoFrameAllocator: duplicate usable frame {:#x} in memory map",
                    n << 12
                );
            }
        }

        // Validate any frames freed before tracking was enabled. This should
        // be empty because `free_frame` panics during bootstrap.
        let mut preserved_freed = Vec::new();

        for frame in self.freed.drain(..) {
            let n = Self::frame_number(frame);

            if n > self.max_frame_number {
                panic!(
                    "BootInfoFrameAllocator::enable_tracking: freed frame {:#x} is outside tracked range",
                    n << 12
                );
            }

            if !Self::clear_bit(&mut bitmap, n) {
                panic!(
                    "BootInfoFrameAllocator::enable_tracking: free list contains unallocated or double-freed frame {:#x}",
                    n << 12
                );
            }

            preserved_freed.push(frame);
        }

        self.freed = preserved_freed;
        self.tracker = FrameTracker::Enabled { bitmap };
    }

    fn track_allocation(&mut self, frame: PhysFrame<Size4KiB>) {
        let n = Self::frame_number(frame);
        let max = self.max_frame_number;

        if let FrameTracker::Enabled { bitmap } = &mut self.tracker {
            if n > max {
                panic!(
                    "BootInfoFrameAllocator: allocating frame {:#x} outside tracked range",
                    n << 12
                );
            }

            if Self::set_bit(bitmap, n) {
                panic!(
                    "BootInfoFrameAllocator: allocating already-allocated frame {:#x}",
                    n << 12
                );
            }
        }
    }

    /// Release a frame back to the allocator.
    ///
    /// # Safety
    /// The frame must have been previously allocated by this allocator,
    /// must not still be in use, and must not be freed twice.
    ///
    /// The “previously allocated” and “not freed twice” conditions are now
    /// checked at runtime and panic on violation.
    pub unsafe fn free_frame(&mut self, frame: PhysFrame<Size4KiB>) {
        let n = Self::frame_number(frame);
        let max = self.max_frame_number;

        if n > max {
            panic!(
                "BootInfoFrameAllocator: free of frame {:#x} outside tracked range",
                n << 12
            );
        }

        match &mut self.tracker {
            FrameTracker::Bootstrap => {
                panic!(
                    "BootInfoFrameAllocator::free_frame called before enable_tracking() (frame {:#x})",
                    n << 12
                );
            }
            FrameTracker::Enabled { bitmap } => {
                if !Self::clear_bit(bitmap, n) {
                    panic!(
                        "BootInfoFrameAllocator: double free or free of unallocated frame {:#x}",
                        n << 12
                    );
                }

                self.freed.push(frame);
            }
        }
    }
}

unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        // Give back a freed frame first, if any.
        if let Some(frame) = self.freed.pop() {
            self.track_allocation(frame);
            return Some(frame);
        }

        // Otherwise take the next untouched frame from the memory map.
        let frame = self.next_fresh_frame();

        if let Some(frame) = frame {
            self.next += 1;
            self.track_allocation(frame);
        }

        frame
    }
}