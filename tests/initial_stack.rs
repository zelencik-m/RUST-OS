//! The initial user stack layout (`argc`, `argv`, `envp`), checked as plain
//! data: `layout_initial_stack` only builds bytes, so no process is needed.
//! See the diagram in `src/process/initial_stack.rs`.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(support::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use rust_os::process::initial_stack::{
    layout_initial_stack, ArgsTooLarge, InitialStack, MAX_ARGS, MAX_IMAGE_SIZE,
};
use x86_64::VirtAddr;

entry_point!(test_kernel_main);

fn test_kernel_main(boot_info: &'static BootInfo) -> ! {
    rust_os::init(boot_info);
    test_main();
    rust_os::hlt_loop()
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    support::test_panic_handler(info)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A page-aligned stack top in the user half, like `allocate_user_stack`'s.
const TOP: u64 = 0x0000_7F80_0042_0000;

fn layout(args: &[&str]) -> InitialStack {
    layout_initial_stack(VirtAddr::new(TOP), args).expect("layout_initial_stack failed")
}

/// The `index`-th u64 of the image (index 0 is argc).
fn word(stack: &InitialStack, index: usize) -> u64 {
    let start = index * 8;
    assert!(
        start + 8 <= stack.image.len(),
        "image too short for word {} ({} bytes)",
        index,
        stack.image.len()
    );
    u64::from_le_bytes(stack.image[start..start + 8].try_into().unwrap())
}

/// Image offset of user address `address`, checked to be inside the image.
fn offset_of(stack: &InitialStack, address: u64) -> usize {
    let rsp = stack.rsp.as_u64();
    assert!(
        address >= rsp && address < rsp + stack.image.len() as u64,
        "pointer {:#x} is outside the image [{:#x}, {:#x})",
        address,
        rsp,
        rsp + stack.image.len() as u64
    );
    (address - rsp) as usize
}

/// The NUL-terminated string at user address `address`.
fn string_at(stack: &InitialStack, address: u64) -> Vec<u8> {
    let start = offset_of(stack, address);
    let length = stack.image[start..]
        .iter()
        .position(|&b| b == 0)
        .expect("string has no NUL terminator inside the image");
    stack.image[start..start + length].to_vec()
}

/// Checks every rule of the contract for `args`.
fn check_layout(args: &[&str]) {
    let stack = layout(args);
    let rsp = stack.rsp.as_u64();
    assert_eq!(rsp % 16, 0, "rsp {:#x} is not 16-byte aligned", rsp);
    assert_eq!(
        rsp + stack.image.len() as u64,
        TOP,
        "the image must end exactly at stack_top"
    );

    let argc = args.len();
    assert_eq!(word(&stack, 0), argc as u64, "argc");
    assert_eq!(word(&stack, 1 + argc), 0, "argv[argc] must be NULL");
    assert_eq!(word(&stack, 2 + argc), 0, "envp must be empty (one NULL)");

    // The strings live above the words (argc, argv[], NULL, NULL).
    let words_end = rsp + 8 * (argc as u64 + 3);
    let mut regions: Vec<(u64, u64)> = Vec::new();
    for (i, arg) in args.iter().enumerate() {
        let pointer = word(&stack, 1 + i);
        assert!(
            pointer >= words_end,
            "argv[{}] = {:#x} points into the pointer array (ends at {:#x})",
            i,
            pointer,
            words_end
        );
        assert_eq!(string_at(&stack, pointer), arg.as_bytes(), "argv[{}]", i);
        let end = pointer + arg.len() as u64 + 1;
        for &(other_start, other_end) in &regions {
            assert!(
                end <= other_start || pointer >= other_end,
                "argv[{}] overlaps another argument",
                i
            );
        }
        regions.push((pointer, end));
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test_case]
fn empty_argv_has_argc_zero_and_null_terminators() {
    check_layout(&[]);
    let stack = layout(&[]);
    // argc, argv NULL, envp NULL = 24 bytes, rounded up to 32.
    assert_eq!(stack.image.len(), 32);
}

#[test_case]
fn argc_and_argv_point_at_the_strings() {
    check_layout(&["proc_child", "echo", "hello world"]);
}

#[test_case]
fn single_argument_layout() {
    check_layout(&["sh"]);
    let stack = layout(&["sh"]);
    // 4 words (32) + "sh\0" (3) = 35, rounded up to 48.
    assert_eq!(stack.image.len(), 48);
}

#[test_case]
fn empty_string_arguments_are_kept() {
    check_layout(&["", "x", ""]);
}

#[test_case]
fn rsp_is_aligned_for_every_size() {
    // Different counts and string lengths hit every padding amount.
    let mut owned: Vec<String> = Vec::new();
    for count in 0..6 {
        for length in 0..20 {
            owned.clear();
            for i in 0..count {
                owned.push("abcdefghijklmnopqrstuvwxyz"[..(length + i) % 26].into());
            }
            let args: Vec<&str> = owned.iter().map(String::as_str).collect();
            check_layout(&args);
        }
    }
}

#[test_case]
fn max_args_fit_and_one_more_is_rejected() {
    let args = ["a"; MAX_ARGS];
    check_layout(&args);
    let too_many = ["a"; MAX_ARGS + 1];
    assert_eq!(
        layout_initial_stack(VirtAddr::new(TOP), &too_many).unwrap_err(),
        ArgsTooLarge
    );
}

#[test_case]
fn image_size_limit_is_exact() {
    // One argument: 4 words (32 bytes) + the string + its NUL, rounded up to
    // 16. A 16351-byte string makes exactly MAX_IMAGE_SIZE (16384).
    let fits: String = core::iter::repeat('a').take(MAX_IMAGE_SIZE - 33).collect();
    let stack = layout(&[fits.as_str()]);
    assert_eq!(stack.image.len(), MAX_IMAGE_SIZE);
    check_layout(&[fits.as_str()]);

    let too_big: String = core::iter::repeat('a').take(MAX_IMAGE_SIZE - 32).collect();
    assert_eq!(
        layout_initial_stack(VirtAddr::new(TOP), &[too_big.as_str()]).unwrap_err(),
        ArgsTooLarge
    );
}

#[test_case]
fn many_large_arguments_are_rejected() {
    let big: String = core::iter::repeat('b').take(4000).collect();
    let args = [big.as_str(); 5];
    assert_eq!(
        layout_initial_stack(VirtAddr::new(TOP), &args).unwrap_err(),
        ArgsTooLarge
    );
}

mod support;
