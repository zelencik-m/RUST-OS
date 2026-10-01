#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(support::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;
use rust_os::memory::address_space::{is_user_range, USER_P4_END, USER_SPACE_END, USER_SPACE_START};
use rust_os::memory::vma::{VmArea, VmaKind};
use x86_64::{structures::paging::PageTableFlags, VirtAddr};

#[no_mangle]
pub extern "C" fn _start() -> ! {
    test_main();
    rust_os::hlt_loop()
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    support::test_panic_handler(info)
}

#[test_case]
fn user_range_accepts_only_addresses_inside_user_slot() {
    assert!(is_user_range(VirtAddr::new(USER_SPACE_START), 1));
    assert!(is_user_range(VirtAddr::new(USER_SPACE_END), 1));
    let end = (USER_P4_END as u64) << 39;
    assert!(is_user_range(VirtAddr::new(end - 1), 1));
    assert!(is_user_range(VirtAddr::new(USER_SPACE_START), end - USER_SPACE_START));
    assert!(!is_user_range(VirtAddr::new(USER_SPACE_START - 1), 1));
    assert!(!is_user_range(VirtAddr::new(0xffff_8000_0000_0000), 1));
}

#[test_case]
fn user_range_rejects_ranges_that_cross_or_wrap_the_boundary() {
    assert!(!is_user_range(VirtAddr::new((256u64 << 39) - 1), 2));
    assert!(!is_user_range(VirtAddr::new(USER_SPACE_START), (1 << 48) + 1));
    assert!(!is_user_range(VirtAddr::new(u64::MAX - 3), 8));
}

#[test_case]
fn zero_length_ranges_preserve_existing_behavior() {
    assert!(is_user_range(VirtAddr::new(USER_SPACE_START), 0));
    assert!(is_user_range(VirtAddr::new(0), 0));
}

#[test_case]
fn vma_contains_start_and_last_byte_but_not_end() {
    let area = VmArea {
        start: VirtAddr::new(0x4000),
        size: 0x2000,
        flags: PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE,
        kind: VmaKind::Data,
    };

    assert!(area.contains(VirtAddr::new(0x4000)));
    assert!(area.contains(VirtAddr::new(0x5fff)));
    assert!(!area.contains(VirtAddr::new(0x3fff)));
    assert!(!area.contains(VirtAddr::new(0x6000)));
    assert_eq!(area.end().as_u64(), 0x6000);
}

#[test_case]
fn empty_vma_contains_no_addresses() {
    let area = VmArea {
        start: VirtAddr::new(0x4000),
        size: 0,
        flags: PageTableFlags::empty(),
        kind: VmaKind::Guard,
    };

    assert!(!area.contains(area.start));
}
mod support;
