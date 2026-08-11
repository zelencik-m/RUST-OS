use x86_64::VirtAddr;
use x86_64::structures::paging::PageTableFlags;

#[derive(Debug, Clone, Copy)]
pub enum VmaKind {
    Stack,
    Heap,
    Code,
    Data,
    Mapped,     // generic / mmap-style
    User,
}

#[derive(Debug, Clone, Copy)]
pub struct VmArea {
    pub start: VirtAddr,
    pub size: u64,        // bytes; must be page-aligned
    pub flags: PageTableFlags, // leaf flags only
    pub kind: VmaKind,
}

impl VmArea {
    pub fn end(&self) -> VirtAddr { self.start + self.size }
    pub fn contains(&self, v: VirtAddr) -> bool {
        v >= self.start && v < self.end()
    }
}