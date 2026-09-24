// src/memory/memory_manager/fault.rs

use super::MemoryManager;
use super::vmm::Oom;
use crate::memory::address_space::AddressSpace;
use crate::memory::vma::VmaKind;
use x86_64::structures::paging::{
    FrameAllocator, Mapper, Page, PageTableFlags, Size4KiB,
};
use x86_64::VirtAddr;

/// Result of handling a page fault.
///
/// This is the single classification used by the kernel's page-fault path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultAction {
    /// The fault was resolved by demand-mapping a page.
    Resolved,

    /// The fault hit a guard page, e.g. stack overflow.
    Guard,

    /// The address is not covered by any valid VMA.
    Segfault,

    /// The fault could not be resolved because memory ran out.
    Oom,
}

impl MemoryManager {
    /// Handle a page fault for a given address space.
    ///
    /// This is the single authoritative page-fault handler for user address
    /// spaces. It performs VMA lookup, guard-page detection, and demand
    /// mapping in one place.
    pub fn handle_page_fault(
        &mut self,
        space: &mut AddressSpace,
        fault_addr: VirtAddr,
    ) -> FaultAction {
        let page_addr = Page::<Size4KiB>::containing_address(fault_addr).start_address();

        let (kind, flags) = match space.get_vma(fault_addr) {
            Some(vma) => (vma.kind, vma.flags),
            None => return FaultAction::Segfault,
        };

        // Guard pages are intentionally not present. Touching one is fatal
        // for the faulting thread.
        if kind == VmaKind::Guard {
            return FaultAction::Guard;
        }

        match self.demand_map_page(space, page_addr, flags) {
            Ok(()) => FaultAction::Resolved,
            Err(_) => FaultAction::Oom,
        }
    }

    /// Demand-map one page into an address space.
    ///
    /// This is intentionally not exposed as a general page-fault entry point.
    /// Use `handle_page_fault` instead so guard-page and VMA checks are not
    /// duplicated.
    pub(crate) fn demand_map_page(
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