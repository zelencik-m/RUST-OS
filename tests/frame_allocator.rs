//! The physical frame allocator (`BootInfoFrameAllocator`), after boot.
//!
//! Fresh frames come from the memory map's usable regions in order. The
//! allocator used to find "the next one" by walking the map from the start
//! on every allocation, which made boot (mapping the heap) take seconds;
//! now it keeps a cursor. These tests pin down what it hands out.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(support::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::vec::Vec;
use bootloader::bootinfo::{MemoryMap, MemoryRegionType};
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use rust_os::memory::MEMORY_MANAGER;
use rust_os::sync::IrqSafeMutex;
use x86_64::structures::paging::{FrameAllocator, PhysFrame, Size4KiB};

entry_point!(test_kernel_main);

static MEMORY_MAP: IrqSafeMutex<Option<&'static MemoryMap>> = IrqSafeMutex::new(None);

fn test_kernel_main(boot_info: &'static BootInfo) -> ! {
    rust_os::init(boot_info);
    *MEMORY_MAP.lock() = Some(&boot_info.memory_map);
    test_main();
    rust_os::hlt_loop()
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    support::test_panic_handler(info)
}

fn allocate(count: usize) -> Vec<PhysFrame<Size4KiB>> {
    let mut mm = MEMORY_MANAGER.lock();
    let allocator = &mut mm.as_mut().expect("memory manager").frame_allocator;
    (0..count)
        .map(|_| allocator.allocate_frame().expect("out of frames"))
        .collect()
}

fn free(frames: &[PhysFrame<Size4KiB>]) {
    let mut mm = MEMORY_MANAGER.lock();
    let allocator = &mut mm.as_mut().expect("memory manager").frame_allocator;
    for &frame in frames {
        unsafe { allocator.free_frame(frame) };
    }
}

fn in_usable_memory(frame: PhysFrame<Size4KiB>) -> bool {
    let address = frame.start_address().as_u64();
    MEMORY_MAP
        .lock()
        .expect("memory map")
        .iter()
        .filter(|region| region.region_type == MemoryRegionType::Usable)
        .any(|region| region.range.start_addr() <= address && address + 4096 <= region.range.end_addr())
}

/// The start of the first usable region that begins at or after `address`.
fn next_usable_start(address: u64) -> Option<u64> {
    MEMORY_MAP
        .lock()
        .expect("memory map")
        .iter()
        .filter(|region| region.region_type == MemoryRegionType::Usable)
        .map(|region| region.range.start_addr())
        .filter(|&start| start >= address)
        .min()
}

#[test_case]
fn fresh_frames_are_distinct_usable_and_in_map_order() {
    // More than fits in one small region, so the cursor has to move on to
    // the next usable region at least once.
    let frames = allocate(3000);
    for &frame in &frames {
        assert_eq!(frame.start_address().as_u64() % 4096, 0, "{:?} isn't page aligned", frame);
        assert!(in_usable_memory(frame), "{:?} isn't in usable memory", frame);
    }
    let mut sorted = frames.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), frames.len(), "a frame was handed out twice");

    // Frames freed earlier (during boot, or by other tests) are reused
    // first, newest first. After those, fresh frames follow the memory map:
    // each is the next page, or the start of the next usable region.
    const REUSED_AT_MOST: usize = 64;
    for pair in frames[REUSED_AT_MOST..].windows(2) {
        let (a, b) = (pair[0].start_address().as_u64(), pair[1].start_address().as_u64());
        let next_page = a + 4096;
        assert!(
            b == next_page || Some(b) == next_usable_start(next_page),
            "{:#x} followed {:#x}: not the next page or the next usable region",
            b,
            a
        );
    }
    free(&frames);
}

#[test_case]
fn freed_frames_are_reused_first() {
    let frames = allocate(2);
    free(&frames[..1]);
    let again = allocate(1);
    assert_eq!(again[0], frames[0], "a freed frame is handed out again first");
    free(&again);
    free(&frames[1..]);
}

#[test_case]
fn many_allocations_stay_fast() {
    // With the old walk-from-the-start lookup, 20000 allocations (after the
    // ~2600 boot used) cost ~250 million iterator steps: tens of seconds
    // here, past the suite's timeout. With the cursor they're instant.
    let frames = allocate(20_000);
    assert_eq!(frames.len(), 20_000);
    free(&frames);
}

mod support;
