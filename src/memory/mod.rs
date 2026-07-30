pub mod frame_allocator;
pub mod paging;
pub mod translate;
pub mod allocator;

// Re-export the primary types so consumers can still write
// `use memory::BootInfoFrameAllocator;` instead of
// `use memory::frame_allocator::BootInfoFrameAllocator;`
pub use frame_allocator::{EmptyFrameAllocator, BootInfoFrameAllocator};
pub use paging::{init, create_example_mapping};
pub use translate::translate_addr;