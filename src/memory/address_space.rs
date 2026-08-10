// src/memory/address_space.rs

use x86_64::{
    structures::paging::{FrameAllocator, PhysFrame, Size4KiB, PageTable, OffsetPageTable},
    VirtAddr,
};

use crate::memory::paging::active_level_4_table;


pub struct AddressSpace { pub p4_frame: PhysFrame<Size4KiB> }

impl AddressSpace {
    pub fn new(fa: &mut impl FrameAllocator<Size4KiB>, off: VirtAddr) -> Self {
        let p4_frame = fa.allocate_frame().unwrap();
        let p4: &mut PageTable = unsafe { &mut *(off + p4_frame.start_address().as_u64()).as_mut_ptr() };
        p4.zero();
        let kernel_p4 = unsafe { active_level_4_table(off) };
        for i in 256..512 { p4[i] = kernel_p4[i].clone(); }
        AddressSpace { p4_frame }
    }
}

/// Build a mapper for a not‑yet‑active address space.
pub fn mapper_for_address_space(
    p4_frame: PhysFrame<Size4KiB>,
    physical_memory_offset: VirtAddr,
) -> OffsetPageTable<'static> {
    let p4_ptr = (physical_memory_offset + p4_frame.start_address().as_u64())
        .as_mut_ptr();
    let p4: &'static mut PageTable = unsafe { &mut *p4_ptr };
    unsafe { OffsetPageTable::new(p4, physical_memory_offset) }
}