use x86_64::{PhysAddr, VirtAddr};

/// Translates the given virtual address to the mapped physical address,
/// or `None` if the address is not mapped.
///
/// # Safety
///
/// The caller must guarantee that the complete physical memory is mapped
/// to virtual memory at `physical_memory_offset`.
pub unsafe fn translate_addr(
    addr: VirtAddr,
    physical_memory_offset: VirtAddr,
) -> Option<PhysAddr> {
    translate_addr_inner(addr, physical_memory_offset)
}

/// Private helper that performs the actual page-table walk.
///
/// Keeping this as a safe function limits the scope of `unsafe` — Rust
/// treats the entire body of an `unsafe fn` as an unsafe block, so we
/// confine the raw-pointer dereference to the smallest possible scope.
fn translate_addr_inner(
    addr: VirtAddr,
    physical_memory_offset: VirtAddr,
) -> Option<PhysAddr> {
    use x86_64::structures::paging::page_table::FrameError;
    use x86_64::registers::control::Cr3;

    let (level_4_table_frame, _) = Cr3::read();

    let table_indexes = [
        addr.p4_index(),
        addr.p3_index(),
        addr.p2_index(),
        addr.p1_index(),
    ];
    let mut frame = level_4_table_frame;

    for &index in &table_indexes {
        let virt = physical_memory_offset + frame.start_address().as_u64();
        let table_ptr: *const x86_64::structures::paging::PageTable = virt.as_ptr();
        let table = unsafe { &*table_ptr };

        let entry = &table[index];
        frame = match entry.frame() {
            Ok(f) => f,
            Err(FrameError::FrameNotPresent) => return None,
            Err(FrameError::HugeFrame) => panic!("huge pages not supported"),
        };
    }

    Some(frame.start_address() + u64::from(addr.page_offset()))
}