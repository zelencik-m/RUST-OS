#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::alloc::{GlobalAlloc, Layout};
use core::panic::PanicInfo;
use core::ptr::addr_of_mut;

use rust_os::{serial_print, serial_println};
use rust_os::memory::allocator::buddy_allocator::{BuddyAllocator};
use rust_os::memory::allocator::{Locked};

#[no_mangle]
pub extern "C" fn _start() -> ! {
    test_main();
    loop {}
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    rust_os::test_panic_handler(info)
}

// Width of the padded test-name column, so every "[ok]" lands in the same
// spot regardless of test name length.
const NAME_COL: usize = 52;

/// Runs the allocator tests and reports the result to QEMU.
pub fn test_runner(tests: &[&dyn Fn()]) {
    serial_println!();
    serial_println!("┌────────────────────────────────────────────────────┐");
    serial_println!("│  Buddy Allocator Test Suite  ({:>2} tests)            │", tests.len());
    serial_println!("└────────────────────────────────────────────────────┘");

    for test in tests {
        test();
    }

    serial_println!("┌────────────────────────────────────────────────────┐");
    serial_println!("│  All {:>2} tests passed                              │", tests.len());
    serial_println!("└────────────────────────────────────────────────────┘");
    serial_println!();

    rust_os::exit_qemu(rust_os::QemuExitCode::Success);
}

/// Prints a section divider before a group of related tests.
fn section(name: &str) {
    serial_println!();
    serial_println!("── {} ──", name);
}

/// Prints the padded test name, runs `body`, then prints "[ok]".
macro_rules! run_test {
    ($name:expr, $body:block) => {{
        serial_print!("{:<width$}", concat!($name, "..."), width = NAME_COL);
        $body
        serial_println!("[ok]");
    }};
}

// 128 minimum-size blocks keep the exhaustion tests' pointer arrays small.
const HEAP_SIZE: usize = 2048;

#[repr(align(4096))]
struct AlignedHeap([u8; HEAP_SIZE]);

static mut TEST_HEAP: AlignedHeap = AlignedHeap([0; HEAP_SIZE]);

/// Build a fresh allocator over the shared static heap. Called at the start
/// of every test so tests don't see stale state from previous tests.
fn new_allocator() -> Locked<BuddyAllocator> {
    let allocator = Locked::new(BuddyAllocator::new());
    unsafe {
        let heap_start = addr_of_mut!(TEST_HEAP) as usize;
        allocator.lock().init(heap_start, HEAP_SIZE);
    }
    allocator
}

#[test_case]
fn test_simple_alloc_dealloc() {
    section("Basic operations");
    run_test!("test_simple_alloc_dealloc", {
        let allocator = new_allocator();
        let layout = Layout::from_size_align(8, 4).unwrap();

        unsafe {
            let ptr = allocator.alloc(layout);
            assert!(!ptr.is_null(), "alloc returned null on a fresh heap");

            // Write through the pointer to make sure it's actually usable memory.
            *ptr = 0xAB;
            assert_eq!(*ptr, 0xAB);

            allocator.dealloc(ptr, layout);
        }
    });
}

#[test_case]
fn test_alignment_is_respected() {
    run_test!("test_alignment_is_respected", {
        let allocator = new_allocator();

        for &align in &[8usize, 16, 32, 64, 128] {
            let layout = Layout::from_size_align(1, align).unwrap();
            unsafe {
                let ptr = allocator.alloc(layout);
                assert!(!ptr.is_null(), "alloc returned null for align {}", align);
                assert_eq!(
                    ptr as usize % align,
                    0,
                    "pointer {:#x} not aligned to {}",
                    ptr as usize,
                    align
                );
                allocator.dealloc(ptr, layout);
            }
        }
    });
}

#[test_case]
fn test_multiple_allocations_do_not_overlap() {
    run_test!("test_multiple_allocations_do_not_overlap", {
        let allocator = new_allocator();

        let layouts = [
            Layout::new::<u8>(),
            Layout::new::<u32>(),
            Layout::from_size_align(100, 8).unwrap(),
            Layout::new::<u64>(),
        ];

        let mut ptrs = [core::ptr::null_mut::<u8>(); 4];
        unsafe {
            for (i, layout) in layouts.iter().enumerate() {
                let ptr = allocator.alloc(*layout);
                assert!(!ptr.is_null());
                ptrs[i] = ptr;
                // Stamp each allocation with a distinct byte pattern.
                core::ptr::write_bytes(ptr, (i as u8) + 1, layout.size());
            }

            // Verify no allocation clobbered another's data.
            for (i, layout) in layouts.iter().enumerate() {
                for b in 0..layout.size() {
                    assert_eq!(*ptrs[i].add(b), (i as u8) + 1);
                }
            }

            for (i, layout) in layouts.iter().enumerate() {
                allocator.dealloc(ptrs[i], *layout);
            }
        }
    });
}

#[test_case]
fn test_split_produces_correctly_sized_block() {
    run_test!("test_split_produces_correctly_sized_block", {
        let allocator = new_allocator();

        // Requesting a small block from a heap that starts as one big free
        // block forces at least one split. Just confirm we get back usable,
        // correctly-sized memory (write to the first and last byte).
        let layout = Layout::from_size_align(16, 16).unwrap();
        unsafe {
            let ptr = allocator.alloc(layout);
            assert!(!ptr.is_null());
            *ptr = 1;
            *ptr.add(layout.size() - 1) = 2;
            assert_eq!(*ptr, 1);
            assert_eq!(*ptr.add(layout.size() - 1), 2);
            allocator.dealloc(ptr, layout);
        }
    });
}

#[test_case]
fn test_alloc_returns_null_when_heap_exhausted() {
    section("Bounds & edge cases");
    run_test!("test_alloc_returns_null_when_heap_exhausted", {
        let allocator = new_allocator();
        let layout = Layout::from_size_align(16, 16).unwrap(); // min-order block
        let expected_blocks = HEAP_SIZE / 16;

        let mut count = 0;
        unsafe {
            loop {
                let ptr = allocator.alloc(layout);
                if ptr.is_null() {
                    break;
                }
                count += 1;
                if count > expected_blocks {
                    panic!("allocator handed out more blocks than the heap can hold");
                }
            }
        }
        assert_eq!(
            count, expected_blocks,
            "expected to exhaust heap after {} blocks, got {}",
            expected_blocks, count
        );
    });
}

#[test_case]
fn test_oversized_request_returns_null() {
    run_test!("test_oversized_request_returns_null", {
        let allocator = new_allocator();
        // Bigger than 1 << MAX_ORDER (1 GiB) -- size_to_order should return
        // None and alloc should return null rather than panicking.
        let layout = Layout::from_size_align(1 << 31, 1).unwrap();
        unsafe {
            let ptr = allocator.alloc(layout);
            assert!(ptr.is_null());
        }
    });
}

#[test_case]
fn test_coalescing_on_dealloc() {
    section("Coalescing");
    run_test!("test_coalescing_on_dealloc", {
        let allocator = new_allocator();
        let layout16 = Layout::from_size_align(16, 16).unwrap();
        let total_blocks = HEAP_SIZE / 16;

        // Fill the entire heap with min-order (16 byte) blocks.
        let mut ptrs = [core::ptr::null_mut::<u8>(); 128];
        assert!(total_blocks <= ptrs.len());
        unsafe {
            for i in 0..total_blocks {
                let ptr = allocator.alloc(layout16);
                assert!(!ptr.is_null(), "heap should not be exhausted yet");
                ptrs[i] = ptr;
            }
            // Heap is now fully exhausted: nothing else can be free anywhere.
            assert!(allocator.alloc(layout16).is_null());

            // Pick any allocated block and find its buddy (address ^ 16) among
            // our other allocations.
            let a = ptrs[0] as usize;
            let buddy_addr = a ^ 16;
            let buddy_index = (0..total_blocks)
                .find(|&i| ptrs[i] as usize == buddy_addr)
                .expect("buddy of ptrs[0] must be one of our allocations");

            // Free both halves of this buddy pair. If dealloc correctly merges
            // them, we get one contiguous 32-byte free region back. Since the
            // rest of the heap is still fully allocated, a 32-byte request can
            // *only* succeed if the merge actually happened.
            let base = a.min(buddy_addr) as *mut u8;
            allocator.dealloc(ptrs[0], layout16);
            allocator.dealloc(ptrs[buddy_index], layout16);

            let layout32 = Layout::from_size_align(32, 16).unwrap();
            let merged = allocator.alloc(layout32);
            assert!(
                !merged.is_null(),
                "expected freeing a buddy pair to coalesce into a 32-byte block"
            );
            assert_eq!(
                merged as usize, base as usize,
                "merged block should start at the lower of the two buddy addresses"
            );

            allocator.dealloc(merged, layout32);
        }
    });
}