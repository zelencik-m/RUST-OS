// src/process/process_manager/memory_ops.rs

use x86_64::structures::paging::PageTableFlags;
use x86_64::VirtAddr;

use crate::process::PID;

use super::manager::ProcessManager;

impl ProcessManager {
    // MILESTONE 1.2: Kept as a proxy for backwards compatibility.
    // The actual logic now lives natively in AddressSpace.
    pub fn get_vma_flags(&self, pid: PID, addr: VirtAddr) -> Option<PageTableFlags> {
        let proc = self.process(pid);
        let space = proc.address_space.as_ref()?;
        space.get_vma_flags(addr)
    }
}