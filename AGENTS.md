# AGENTS.md

`no_std` x86_64 hobby kernel (blog_os lineage, `bootloader` 0.9 + `bootimage`). See `README.md` for scheduler/process design notes.

## Toolchain & commands
- Nightly Rust required (uses `build-std`, `json-target-spec`, custom target `x86_64-blog_os.json`, set as the default target in `.cargo/config.toml`). The nightly is pinned in `rust-toolchain.toml` (with `rust-src` and `llvm-tools-preview`); bump it deliberately.
- External tools: `bootimage` (cargo runner), `qemu-system-x86_64`, `clang` (used by `build.rs`), and `rust-lld` from the Rust sysroot.
- Quick compile check: `cargo check` (also `cargo check --tests`).
- All tests: `cargo test`. One suite: `cargo test --test <file_stem>` (e.g. `userspace`, `filesystem`, `heap`). Each suite boots its own QEMU (timeout 300s); success = isa-debug-exit code 33.
- `cargo run` boots the kernel in QEMU and **never exits** (the `thread_b`/`thread_c` demos loop forever). Don't run it in the foreground.
- There are no unit tests: `[lib]` and `[[bin]]` have `test = false`. Every test is a `#![no_std]` integration target in `tests/` using `custom_test_frameworks` + `#[test_case]` and `tests/support/mod.rs` (`mod support;`).

## User programs (C → ELF)
- `build.rs` compiles C programs with clang and links them with `user_programs/linker.ld` into `$OUT_DIR/<name>.elf`. The kernel and tests embed them with `include_bytes!(concat!(env!("OUT_DIR"), ...))`.
- To add a program, add it to `PROGRAMS` in `build.rs` **and** embed it where it's used (`src/main.rs`, `tests/userspace.rs`, `tests/elf_images.rs`). The `userspace` runner expects exactly one `#[test_case]` and keys success off per-program output markers.
- `user_programs/syscalls.h` duplicates syscall numbers from `src/syscall/numbers.rs` (Linux numbering plus custom `SYS_SLEEP_TICKS = 1000`). Keep them in sync manually.
- C code must stay freestanding (no SSE, no red zone, large code model). User programs load at `0x00007F8000100000` (P4 slot 255).

## Runtime gotchas
- `rust_os::init` mounts `ext2.img` (committed to git, 32 MiB) as the VFS root over ATA PIO, secondary master (`-drive ... index=2` in `Cargo.toml` bootimage args). A missing or corrupt image panics boot. QEMU never boots the committed image: the cargo runner `scripts/qemu-runner.sh` stages `target/ext2-disk.img` (fresh copy for every test suite; kept between `cargo run`s, delete it to reset), and the bootimage args point at that copy.
- The `userspace` and `filesystem` tests run against an in-memory `RamFs`, not ext2 contents. They still call `init`, so the image still has to mount.
- Lock order is always **SCHEDULER → PROCESS_MANAGER → MEMORY_MANAGER** (`src/process/process_manager/api.rs`). Drop locks before `switch_context`.
- Debug logging (`debugln!`) is enabled only in `src/main.rs`. Test kernels keep it disabled, and `basic_boot` asserts that.
- Output goes to the VGA buffer (`println!`) and to serial (`serial_println!`, which QEMU sends to stdio). Test results are read from serial.
