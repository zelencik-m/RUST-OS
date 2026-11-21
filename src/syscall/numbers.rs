// Linux-compatible numbers for the syscalls we implement.
pub const SYS_READ: u64 = 0;
pub const SYS_WRITE: u64 = 1;
pub const SYS_OPEN: u64 = 2;
pub const SYS_CLOSE: u64 = 3;
pub const SYS_LSEEK: u64 = 8;
pub const SYS_MMAP: u64 = 9;
pub const SYS_MUNMAP: u64 = 11;
pub const SYS_BRK: u64 = 12;
pub const SYS_SCHED_YIELD: u64 = 24;
pub const SYS_GETPID: u64 = 39;
pub const SYS_GETCWD: u64 = 79;
pub const SYS_CHDIR: u64 = 80;
pub const SYS_MKDIR: u64 = 83;
pub const SYS_RMDIR: u64 = 84;
pub const SYS_UNLINK: u64 = 87;
pub const SYS_EXIT: u64 = 60;
pub const SYS_GETTID: u64 = 186;
pub const SYS_GETDENTS64: u64 = 217;

// Custom helper syscall for testing.
pub const SYS_SLEEP_TICKS: u64 = 1000;
// tty_mode(fd, mode): 0 = cooked, 1 = raw. Returns the previous mode.
pub const SYS_TTY_MODE: u64 = 1001;
