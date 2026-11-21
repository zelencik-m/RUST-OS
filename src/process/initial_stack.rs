//! The initial user stack of a new process: `argc` and `argv` in the System V
//! x86_64 layout.
//!
//! The kernel builds this image in a kernel buffer, then copies it to the top
//! of the new process's user stack with `copy_to_user`, and starts the thread
//! with `rsp` pointing at `argc`. A C `_start` then finds its arguments at
//! `[rsp]` (argc) and `rsp + 8` (argv). Phase 4's `crt0` does exactly that.
//!
//! ```text
//!   stack_top ──►  (end of the stack mapping, 16-byte aligned)
//!                  argument strings, each NUL-terminated
//!                  padding, so that rsp ends up 16-byte aligned
//!                  0                  envp terminator (empty environment)
//!                  0                  argv[argc] = NULL
//!                  &string[argc-1]    argv[argc-1]
//!                  ...
//!                  &string[0]         argv[0]
//!   rsp ───────►   argc
//! ```
//!
//! Every entry is a little-endian `u64`. The `argv[i]` values are *user*
//! addresses: where the strings will be once the image is copied to `rsp`,
//! not where they are in the kernel buffer.
//!
//! There is no auxiliary vector (no `AT_*` entries); nothing here reads one.

use alloc::vec::Vec;
use x86_64::VirtAddr;

/// Most arguments a process can be started with (argv[0] included).
pub const MAX_ARGS: usize = 32;

/// Largest initial stack image, in bytes. The user stack is 64 KiB, so this
/// leaves the program at least 48 KiB of its own stack.
pub const MAX_IMAGE_SIZE: usize = 16 * 1024;

/// The bytes to copy to `[rsp, stack_top)`.
#[derive(Debug)]
pub struct InitialStack {
    /// Initial stack pointer: the address of `argc`. A multiple of 16.
    pub rsp: VirtAddr,
    /// Exactly `stack_top - rsp` bytes. `image[0..8]` is `argc`.
    pub image: Vec<u8>,
}

/// More than [`MAX_ARGS`] arguments, or an image larger than
/// [`MAX_IMAGE_SIZE`]. `spawn` reports it as `E2BIG`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArgsTooLarge;

/// Lay out `args` for a stack whose top (highest address, exclusive) is
/// `stack_top`.
///
/// Contract (`tests/initial_stack.rs` checks it):
/// - `rsp` is a multiple of 16, and `rsp + image.len() == stack_top`.
/// - `image[0..8]` is `args.len()`. Then come `args.len()` pointers, then a
///   NULL (argv terminator), then another NULL (envp terminator).
/// - Pointer `i` is the user address of a NUL-terminated copy of `args[i]`
///   inside the image, above the two NULLs.
/// - Too many arguments, or an image over [`MAX_IMAGE_SIZE`], gives
///   `Err(ArgsTooLarge)`.
///
/// `stack_top` must be 16-byte aligned (stack tops are page-aligned); panic
/// otherwise. An empty `args` is allowed: `argc` is 0, and argv is just NULL.
///
/// Hints:
/// - Work out the total size first: strings (with NULs), then the
///   `8 * (1 + argc + 2)` bytes of words. Round the total *up* to a multiple
///   of 16, so `rsp = stack_top - total` is aligned. The padding goes
///   between the words and the strings.
/// - A string at image offset `off` has user address `rsp + off`.
pub fn layout_initial_stack(
    stack_top: VirtAddr,
    args: &[&str],
) -> Result<InitialStack, ArgsTooLarge> {
    assert!(
        stack_top.as_u64() % 16 == 0,
        "stack_top {:#x} is not 16-byte aligned",
        stack_top.as_u64()
    );
    if args.len() > MAX_ARGS {
        return Err(ArgsTooLarge);
    }

    // argc, argv[0..argc], argv NULL, envp NULL.
    let words_size = 8 * (args.len() + 3);
    let strings_size: usize = args.iter().map(|arg| arg.len() + 1).sum();
    // Rounding up keeps rsp aligned: stack_top is, and so is total.
    let total = (words_size + strings_size + 15) & !15;
    if total > MAX_IMAGE_SIZE {
        return Err(ArgsTooLarge);
    }

    let rsp = stack_top.as_u64() - total as u64;
    // Zero-filled, so both NULL terminators and the padding are already in place.
    let mut image = alloc::vec![0u8; total];
    write_u64(&mut image, 0, args.len() as u64);

    // The strings go at the very top; the padding ends up between them and
    // the words.
    let mut string_offset = total - strings_size;
    for (i, arg) in args.iter().enumerate() {
        image[string_offset..string_offset + arg.len()].copy_from_slice(arg.as_bytes());
        // image[string_offset + arg.len()] is already the NUL.
        write_u64(&mut image, 8 + 8 * i, rsp + string_offset as u64);
        string_offset += arg.len() + 1;
    }

    Ok(InitialStack {
        rsp: VirtAddr::new(rsp),
        image,
    })
}

fn write_u64(image: &mut [u8], offset: usize, value: u64) {
    image[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
