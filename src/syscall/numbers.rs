// src/syscalls/numbers.rs

// Linux-compatible numbers for the syscalls we implement.
pub const SYS_WRITE: u64 = 1;
pub const SYS_MMAP: u64 = 9;
pub const SYS_MUNMAP: u64 = 11;
pub const SYS_BRK: u64 = 12;
pub const SYS_SCHED_YIELD: u64 = 24;
pub const SYS_GETPID: u64 = 39;
pub const SYS_EXIT: u64 = 60;
pub const SYS_GETTID: u64 = 186;

// Custom helper syscall for testing.
// Not Linux-compatible.
pub const SYS_SLEEP_TICKS: u64 = 1000;