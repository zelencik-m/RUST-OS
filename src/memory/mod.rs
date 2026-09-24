pub mod frame_allocator;
pub mod paging;
pub mod translate;
pub mod allocator;
pub mod memory_manager;
pub mod address_space;
pub mod vma;

pub use frame_allocator::{EmptyFrameAllocator, BootInfoFrameAllocator};
pub use address_space::AddressSpace;
pub use paging::{init, create_example_mapping};
pub use translate::translate_addr;
pub use vma::VmArea;
pub use memory_manager::{MemoryManager, MEMORY_MANAGER, Oom, FaultAction};