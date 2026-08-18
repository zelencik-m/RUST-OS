// src/memory/memory_manager/lifecycle.rs

use super::MemoryManager;
use crate::memory::address_space::AddressSpace;
use x86_64::structures::paging::{PageTable, page_table::FrameError};

impl MemoryManager {
   
    pub fn create_address_space(&mut self) -> AddressSpace {
        AddressSpace::new(
            &mut self.frame_allocator,
            self.physical_memory_offset,
        )
    }

    pub fn destroy_address_space(&mut self, space: &mut AddressSpace) {
        let p4_frame = space.p4_frame;
        let p4_ptr = (self.physical_memory_offset + p4_frame.start_address().as_u64()).as_mut_ptr();
        let p4: &mut PageTable = unsafe { &mut *p4_ptr };

        // 1. Walk and free all user-space mappings (indices 0..256)
        // `.take(256)` is much cleaner than `enumerate()` and `break`!
        for p4_entry in p4.iter_mut().take(256) {
            if let Ok(p3_frame) = p4_entry.frame() {
                let p3_ptr = (self.physical_memory_offset + p3_frame.start_address().as_u64()).as_mut_ptr();
                let p3: &mut PageTable = unsafe { &mut *p3_ptr };
                
                // Recursively free P3, P2, P1, and the mapped data frames
                self.free_page_tables_recursive(p3, 3);
                
                // Free the P3 table frame itself
                unsafe { self.frame_allocator.free_frame(p3_frame); }
                
                // Good hygiene: clear the entry so it doesn't point to freed memory
                p4_entry.set_unused();
            }
        }

        // 2. Free the P4 frame itself
        unsafe { self.frame_allocator.free_frame(p4_frame); }
        
        // 3. Clear VMA tracking
        space.vmas.clear();
    }

    /// Recursively walks the page table hierarchy and frees all frames.
    /// `level` should be 3 for P3, 2 for P2, 1 for P1.
    fn free_page_tables_recursive(&mut self, table: &mut PageTable, level: u8) {
        for entry in table.iter_mut() {
            match entry.frame() {
                Ok(frame) => {
                    if level > 1 {
                        // Intermediate page table (P3 -> P2, or P2 -> P1)
                        let next_table_ptr = (self.physical_memory_offset + frame.start_address().as_u64()).as_mut_ptr();
                        let next_table: &mut PageTable = unsafe { &mut *next_table_ptr };
                        
                        // Recurse down the tree
                        self.free_page_tables_recursive(next_table, level - 1);
                        
                        // Free the intermediate table frame after its children are freed
                        unsafe { self.frame_allocator.free_frame(frame); }
                    } else {
                        // Level 1: `frame` is the actual mapped physical memory page
                        unsafe { self.frame_allocator.free_frame(frame); }
                    }
                }
                Err(FrameError::FrameNotPresent) => {
                    // Nothing mapped here, skip.
                }
                Err(FrameError::HugeFrame) => {
                    // If you implement 2MB/1GB pages later, you would free 
                    // the large frame here. For now, we just skip it.
                }
            }
        }
    }
}