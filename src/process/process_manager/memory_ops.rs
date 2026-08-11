use alloc::vec::Vec;

use x86_64::structures::paging::{
    FrameAllocator, Mapper, Page, PageTableFlags, PhysFrame, Size4KiB,
};
use x86_64::VirtAddr;

use crate::memory::address_space::map_user_region;
use crate::memory::vma::{VmArea, VmaKind};
use crate::process::PID;

use super::manager::ProcessManager;

impl ProcessManager {
    /// Map a user region into a process and record the VMA.
    pub fn map_user_region_for(
        &mut self,
        pid: PID,
        fa: &mut impl FrameAllocator<Size4KiB>,
        off: VirtAddr,
        start: VirtAddr,
        size: usize,
        leaf_flags: PageTableFlags,
        kind: VmaKind,
    ) -> Vec<PhysFrame<Size4KiB>> {
        let process = self.process_mut(pid);
        let space = process
            .address_space
            .as_mut()
            .expect("map_user_region_for: kernel process has no address space");

        let frames = map_user_region(space, fa, off, start, size, leaf_flags);

        process.vmas.push(VmArea {
            start,
            size: size as u64,
            flags: leaf_flags,
            kind,
        });
        frames
    }

    pub fn register_vma_only(
        &mut self,
        pid: PID,
        start: VirtAddr,
        size: usize,
        flags: PageTableFlags,
        kind: VmaKind,
    ) {
        let process = self.process_mut(pid);
        process.vmas.push(VmArea {
            start,
            size: size as u64,
            flags,
            kind,
        });
    }

    pub fn get_vma_flags(&self, pid: PID, addr: VirtAddr) -> Option<PageTableFlags> {
        let proc = self.process(pid);
        proc.vmas
            .iter()
            .find(|v| {
                let start = v.start.as_u64();
                let end = start + v.size;
                let a = addr.as_u64();
                a >= start && a < end
            })
            .map(|v| v.flags)
    }

    /// Maps a single frame for a faulting process (Demand Paging).
    pub fn demand_map_page(
        &mut self,
        pid: PID,
        page_addr: VirtAddr,
        leaf_flags: PageTableFlags,
        fa: &mut impl FrameAllocator<Size4KiB>,
        off: VirtAddr,
    ) {
        let proc = self.process_mut(pid);
        let space = proc
            .address_space
            .as_mut()
            .expect("demand_map: kernel process has no user space");

        let frame = fa.allocate_frame().expect("demand_map: out of frames");

        let parent_flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::USER_ACCESSIBLE;

        let page = Page::<Size4KiB>::containing_address(page_addr);
        let mut mapper = unsafe { space.mapper(off) };

        unsafe {
            mapper
                .map_to_with_table_flags(
                    page,
                    frame,
                    leaf_flags | PageTableFlags::USER_ACCESSIBLE,
                    parent_flags,
                    fa,
                )
                .expect("demand_map: map_to failed")
                .flush();
        }
    }
}