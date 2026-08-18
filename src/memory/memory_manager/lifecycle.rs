// src/memory/memory_manager/lifecycle.rs

use super::MemoryManager;
use crate::memory::address_space::AddressSpace;
use x86_64::structures::paging::{PageTable, page_table::FrameError, FrameAllocator};

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
}