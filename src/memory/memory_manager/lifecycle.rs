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
    use crate::memory::address_space::{USER_P4_START, USER_P4_END};

    let p4_frame = space.p4_frame;

    let p4_ptr =
        (self.physical_memory_offset + p4_frame.start_address().as_u64()).as_mut_ptr();
    let p4: &mut PageTable = unsafe { &mut *p4_ptr };

    // Kernel mappings are shared, so only release the private user P4 slot.
    for idx in USER_P4_START..USER_P4_END {
        let p4_entry = &mut p4[idx];

        if let Ok(p3_frame) = p4_entry.frame() {
            let p3_ptr = (self.physical_memory_offset + p3_frame.start_address().as_u64())
                .as_mut_ptr();
            let p3: &mut PageTable = unsafe { &mut *p3_ptr };

            self.free_page_tables_recursive(p3, 3);

            unsafe {
                self.frame_allocator.free_frame(p3_frame);
            }

            p4_entry.set_unused();
        }
    }

    unsafe {
        self.frame_allocator.free_frame(p4_frame);
    }

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
                        
                        self.free_page_tables_recursive(next_table, level - 1);
                        
                        unsafe { self.frame_allocator.free_frame(frame); }
                    } else {
                        unsafe { self.frame_allocator.free_frame(frame); }
                    }
                }
                Err(FrameError::FrameNotPresent) => {
                    // The entry does not reference a frame.
                }
                Err(FrameError::HugeFrame) => {
                    // Huge pages are not supported here.
                }
            }
        }
    }
}