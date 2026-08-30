use super::MemoryManager;
use super::vmm::Oom;
use crate::memory::address_space::AddressSpace;
use crate::memory::vma::VmaKind;
use x86_64::structures::paging::{Page, PageTableFlags, Size4KiB, Mapper, FrameAllocator};
use x86_64::VirtAddr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultAction { Resolved, Segfault }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageFaultError;

impl MemoryManager {
    pub fn handle_page_fault(
        &mut self,
        space: &mut AddressSpace,
        fault_addr: VirtAddr,
    ) -> Result<FaultAction, PageFaultError> {
        let page_addr = Page::<Size4KiB>::containing_address(fault_addr).start_address();

        if let Some(vma) = space.get_vma(fault_addr) {
            // Catch stack overflows hitting the guard page
            if vma.kind == VmaKind::Guard {
                return Ok(FaultAction::Segfault);
            }

            // In a VMA, demand map the page.
            let flags = vma.flags;
            self.demand_map_page(space, page_addr, flags)
                .map(|_| FaultAction::Resolved)
                .map_err(|_| PageFaultError)
        } else {
            Ok(FaultAction::Segfault)
        }
    }

    pub fn demand_map_page(
        &mut self,
        space: &mut AddressSpace,
        page_addr: VirtAddr,
        leaf_flags: PageTableFlags,
    ) -> Result<(), Oom> {
        let frame = self.frame_allocator.allocate_frame().ok_or(Oom)?;
        let parent_flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::USER_ACCESSIBLE;

        let page = Page::<Size4KiB>::containing_address(page_addr);
        let mut mapper = unsafe { space.mapper(self.physical_memory_offset) };

        unsafe {
            mapper
                .map_to_with_table_flags(
                    page,
                    frame,
                    leaf_flags | PageTableFlags::USER_ACCESSIBLE,
                    parent_flags,
                    &mut self.frame_allocator,
                )
                .map_err(|_| Oom)?
                .flush();
        }

        // Zero the newly mapped page via the physical-memory direct map,
        // so this works regardless of which CR3 is currently active.
        let kernel_ptr = (self.physical_memory_offset
            + frame.start_address().as_u64())
            .as_mut_ptr::<u8>();
        unsafe {
            core::ptr::write_bytes(kernel_ptr, 0, 4096);
        }

        Ok(())
    }
}