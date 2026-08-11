pub mod frame_allocator;
pub mod paging;
pub mod translate;
pub mod allocator;
pub mod memory_manager;
pub mod address_space;
pub mod vma;

// Re-export the primary types so consumers can still write
// `use memory::BootInfoFrameAllocator;` instead of
// `use memory::frame_allocator::BootInfoFrameAllocator;`
pub use frame_allocator::{EmptyFrameAllocator, BootInfoFrameAllocator};
pub use address_space::AddressSpace;
pub use paging::{init, create_example_mapping};
pub use translate::translate_addr;
pub use vma::VmArea;