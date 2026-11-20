#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(support::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;
use rust_os::memory::address_space::{USER_SPACE_END, USER_SPACE_START};
use rust_os::syscall::errors::SysError;
use rust_os::syscall::uaccess::{canonical_user_addr, validate_user_range};

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
fn canonical_user_addr_accepts_canonical_boundaries() {
    assert_eq!(canonical_user_addr(USER_SPACE_START).unwrap().as_u64(), USER_SPACE_START);
    assert_eq!(canonical_user_addr(USER_SPACE_END).unwrap().as_u64(), USER_SPACE_END);
    assert_eq!(canonical_user_addr(0).unwrap().as_u64(), 0);
}

#[test_case]
fn canonical_user_addr_rejects_noncanonical_sign_extension() {
    assert_eq!(canonical_user_addr(0x0001_0000_0000_0000), Err(SysError::Efault));
    // Bits 63..48 look like sign extension, but bit 47 is clear.
    assert_eq!(canonical_user_addr(0xffff_0000_0000_0000), Err(SysError::Efault));
    assert_eq!(canonical_user_addr(0x0000_8000_0000_0000), Err(SysError::Efault));
}

#[test_case]
fn validate_user_range_checks_entire_range_and_overflow() {
    assert_eq!(validate_user_range(USER_SPACE_START, 1).unwrap().as_u64(), USER_SPACE_START);
    assert_eq!(validate_user_range(USER_SPACE_END, 1).unwrap().as_u64(), USER_SPACE_END);
    assert_eq!(validate_user_range((256u64 << 39) - 1, 2), Err(SysError::Efault));
    assert_eq!(validate_user_range(USER_SPACE_START - 1, 1), Err(SysError::Efault));
    assert_eq!(validate_user_range(u64::MAX, 2), Err(SysError::Efault));
}

#[test_case]
fn validate_zero_length_range_only_requires_canonical_address() {
    assert_eq!(validate_user_range(0, 0).unwrap().as_u64(), 0);
    assert_eq!(validate_user_range(0x0001_0000_0000_0000, 0), Err(SysError::Efault));
}
mod support;
