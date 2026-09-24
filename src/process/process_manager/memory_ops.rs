
use x86_64::structures::paging::PageTableFlags;
use x86_64::VirtAddr;

use crate::process::PID;
use crate::memory::vma::VmArea;

use super::manager::ProcessManager;

impl ProcessManager {
    pub fn get_vma_flags(&self, pid: PID, addr: VirtAddr) -> Option<PageTableFlags> {
        let proc = self.process(pid);
        let space = proc.address_space.as_ref()?;
        space.get_vma_flags(addr)
    }
}
