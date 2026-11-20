use super::MemoryManager;
use super::vmm::Oom;
use crate::memory::address_space::AddressSpace;
use crate::memory::vma::VmaKind;
use crate::memory::VmArea;
use x86_64::{structures::paging::PageTableFlags, VirtAddr};

/// Flags for user stack and heap pages: writable data, never executable (W^X).
const USER_DATA_FLAGS: PageTableFlags = PageTableFlags::PRESENT
    .union(PageTableFlags::WRITABLE)
    .union(PageTableFlags::USER_ACCESSIBLE)
    .union(PageTableFlags::NO_EXECUTE);

impl MemoryManager {
    pub fn allocate_user_stack(&mut self, space: &mut AddressSpace) -> Result<VirtAddr, Oom> {
        const STACK_PAGES: usize = 16; // 64KB stack
        const GUARD_PAGES: usize = 1;
        let total_pages = STACK_PAGES + GUARD_PAGES;
        let total_size = total_pages * 4096;

        let start = space.find_free_region_top_down(total_size as u64).ok_or(Oom)?;

        // Reserve the bottom page as a guard.
        space.vmas.push(VmArea {
            start,
            size: 4096,
            flags: PageTableFlags::USER_ACCESSIBLE, // NOT Present
            kind: VmaKind::Guard,
        });

        let stack_start = start + 4096u64;
        let stack_size = STACK_PAGES * 4096;
        
        self.mmap_at(
            space, stack_start, stack_size, 
            USER_DATA_FLAGS, 
            VmaKind::Stack
        )?;

        // The initial stack pointer is at the top of the stack.
        Ok(stack_start + stack_size as u64)
    }

    pub fn sys_brk(
        &mut self,
        space: &mut AddressSpace,
        new_break: VirtAddr,
    ) -> Result<VirtAddr, Oom> {
        let current_break = space.heap_break;
        if new_break == current_break { return Ok(current_break); }
        
        if new_break > current_break {
            let map_start = VirtAddr::new((current_break.as_u64() + 0xFFF) & !0xFFF);
            let map_end = VirtAddr::new((new_break.as_u64() + 0xFFF) & !0xFFF);
            
            if map_start < map_end {
                let size = (map_end.as_u64() - map_start.as_u64()) as usize;
                self.mmap_at(
                    space, map_start, size,
                    USER_DATA_FLAGS,
                    VmaKind::Heap,
                )?;
            }
            space.heap_break = new_break;
            Ok(new_break)
        } else {
            let unmap_start = VirtAddr::new((new_break.as_u64() + 0xFFF) & !0xFFF);
            let unmap_end = VirtAddr::new((current_break.as_u64() + 0xFFF) & !0xFFF);
            
            if unmap_start < unmap_end {
                let size = (unmap_end.as_u64() - unmap_start.as_u64()) as usize;
                self.munmap(space, unmap_start, size);
            }
            space.heap_break = new_break;
            Ok(new_break)
        }
    }
}