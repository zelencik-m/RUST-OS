//! `SleepLock` and the filesystem lock, with real threads.
//!
//! The filesystem used to hold an `IrqSafeMutex` (interrupts off) for every
//! operation, so a long disk write froze everything, timer included. Now
//! it's serialized by `fs::FS_LOCK`, a `SleepLock`: waiters sleep, and the
//! holder keeps interrupts on. This suite checks the lock itself, and that
//! disk I/O no longer stops other threads (the boot screen relies on it).
//!
//! One orchestration test (like `tty_blocking`): a monitor thread runs the
//! numbered steps against the real ext2 disk.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(sleep_lock_test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use rust_os::fs::{self, file, OpenFlags, Path};
use rust_os::process::process_manager::{self, PROCESS_MANAGER};
use rust_os::process::thread::{ThreadState, TID};
use rust_os::process::PID;
use rust_os::scheduler::scheduler::{sleep, yield_now, SCHEDULER};
use rust_os::sync::SleepLock;
use x86_64::instructions::interrupts;

/// Per-wait bound. The PIT runs at ~18.2 Hz, so this is about 27 s.
const WAIT_TIMEOUT_TICKS: u64 = 500;
const STEPS: usize = 5;

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

fn sleep_lock_test_runner(tests: &[&dyn Fn()]) {
    assert_eq!(tests.len(), 1, "sleep_lock expects one orchestration test");
    tests[0]();
}

// ---------------------------------------------------------------------------
// Shared state and worker threads
// ---------------------------------------------------------------------------

static LOCK: SleepLock = SleepLock::new();

/// Set by `holder` once it holds LOCK; it releases it when RELEASE is set.
static HELD: AtomicBool = AtomicBool::new(false);
static RELEASE: AtomicBool = AtomicBool::new(false);
/// Whether interrupts were on while `holder` held the lock.
static HELD_WITH_INTERRUPTS: AtomicBool = AtomicBool::new(false);
/// Set by `waiter` once it got LOCK.
static WAITER_GOT_IT: AtomicBool = AtomicBool::new(false);

/// For the mutual-exclusion step: a counter updated with a deliberately
/// non-atomic read-yield-write, and how many incrementers have finished.
static COUNTER: AtomicUsize = AtomicUsize::new(0);
static INCREMENTERS_DONE: AtomicUsize = AtomicUsize::new(0);
const INCREMENTS: usize = 20;

/// For the disk steps.
static FS_HOLDER_READY: AtomicBool = AtomicBool::new(false);
static FS_RELEASE: AtomicBool = AtomicBool::new(false);
static VFS_USER_DONE_AT: AtomicU64 = AtomicU64::new(0);
static FS_RELEASED_AT: AtomicU64 = AtomicU64::new(0);

/// The "animation": counts how often it got to run.
static TICKER_RUNS: AtomicU64 = AtomicU64::new(0);
/// Writer results: ticks the write took, and ticker runs meanwhile.
static WRITE_TICKS: AtomicU64 = AtomicU64::new(u64::MAX);
static TICKER_RUNS_DURING_WRITE: AtomicU64 = AtomicU64::new(0);

fn park() -> ! {
    loop {
        sleep(1_000_000);
    }
}

fn ticks() -> u64 {
    SCHEDULER.lock().tick_count()
}

extern "C" fn holder() -> ! {
    let guard = LOCK.lock();
    HELD_WITH_INTERRUPTS.store(interrupts::are_enabled(), Ordering::SeqCst);
    HELD.store(true, Ordering::SeqCst);
    while !RELEASE.load(Ordering::SeqCst) {
        sleep(1); // holding the lock across a sleep: the point of SleepLock
    }
    drop(guard);
    park()
}

extern "C" fn waiter() -> ! {
    let _guard = LOCK.lock();
    WAITER_GOT_IT.store(true, Ordering::SeqCst);
    drop(_guard);
    park()
}

extern "C" fn incrementer() -> ! {
    for _ in 0..INCREMENTS {
        let _guard = LOCK.lock();
        let value = COUNTER.load(Ordering::SeqCst);
        // Give the other incrementer every chance to run in between. Without
        // the lock, it would read the same value and an update would be lost.
        yield_now();
        COUNTER.store(value + 1, Ordering::SeqCst);
    }
    INCREMENTERS_DONE.fetch_add(1, Ordering::SeqCst);
    park()
}

extern "C" fn fs_holder() -> ! {
    let guard = fs::lock();
    FS_HOLDER_READY.store(true, Ordering::SeqCst);
    while !FS_RELEASE.load(Ordering::SeqCst) {
        sleep(1);
    }
    FS_RELEASED_AT.store(ticks(), Ordering::SeqCst);
    drop(guard);
    park()
}

extern "C" fn vfs_user() -> ! {
    fs::with_vfs(|vfs| vfs.stat(&Path::root(), "/")).expect("stat / failed");
    VFS_USER_DONE_AT.store(ticks(), Ordering::SeqCst);
    park()
}

extern "C" fn ticker() -> ! {
    loop {
        TICKER_RUNS.fetch_add(1, Ordering::SeqCst);
        sleep(1);
    }
}

/// Write 256 KiB to a new ext2 file in one VFS operation (holding the
/// filesystem lock the whole time), then remove it.
extern "C" fn disk_writer() -> ! {
    const SIZE: usize = 256 * 1024;
    let chunk = [0x5au8; 4096];
    let start = ticks();
    let runs_before = TICKER_RUNS.load(Ordering::SeqCst);
    fs::with_vfs(|vfs| {
        let flags = OpenFlags::from_raw(file::O_CREAT | file::O_WRONLY | file::O_TRUNC)?;
        let mut handle = vfs.open(&Path::root(), "/p6_big", flags)?;
        let mut written = 0;
        while written < SIZE {
            written += handle.write(&chunk)?;
        }
        Ok(())
    })
    .expect("writing /p6_big failed");
    TICKER_RUNS_DURING_WRITE.store(
        TICKER_RUNS.load(Ordering::SeqCst) - runs_before,
        Ordering::SeqCst,
    );
    WRITE_TICKS.store(ticks() - start, Ordering::SeqCst);
    fs::with_vfs(|vfs| vfs.unlink(&Path::root(), "/p6_big")).expect("unlink failed");
    park()
}

// ---------------------------------------------------------------------------
// Monitor helpers
// ---------------------------------------------------------------------------

fn spawn(entry: extern "C" fn() -> !) -> TID {
    process_manager::create_thread(PID(0), entry)
}

fn state(tid: TID) -> ThreadState {
    PROCESS_MANAGER.lock().get_thread_state(tid)
}

/// Poll `condition` once per tick until it holds; panic after the timeout.
fn wait_for(what: &str, condition: impl Fn() -> bool) {
    let deadline = ticks() + WAIT_TIMEOUT_TICKS;
    while !condition() {
        if ticks() > deadline {
            panic!("timed out waiting for: {}", what);
        }
        sleep(1);
    }
}

fn pass(step: &str) {
    support::report_pass(step);
}

// ---------------------------------------------------------------------------
// The steps
// ---------------------------------------------------------------------------

extern "C" fn monitor() -> ! {
    // 1. Re-entrant: the owner can lock again; it's free only after the
    //    last guard is gone.
    {
        let outer = LOCK.lock();
        let inner = LOCK.lock();
        assert!(LOCK.is_locked());
        drop(inner);
        assert!(LOCK.is_locked(), "released while the outer guard is alive");
        drop(outer);
        assert!(!LOCK.is_locked());
    }
    pass("sleep_lock::reentrant_for_the_owner");

    // 2. Another thread blocks (sleeps, not spins) while the lock is held,
    //    the holder keeps interrupts on, and releasing wakes the waiter.
    spawn(holder);
    wait_for("holder to take the lock", || HELD.load(Ordering::SeqCst));
    assert!(
        HELD_WITH_INTERRUPTS.load(Ordering::SeqCst),
        "interrupts were off while holding a SleepLock"
    );
    let waiter_tid = spawn(waiter);
    wait_for("waiter to block", || state(waiter_tid) == ThreadState::Blocked);
    assert!(LOCK.waiter_count() >= 1);
    sleep(3);
    assert!(!WAITER_GOT_IT.load(Ordering::SeqCst), "waiter got a held lock");
    RELEASE.store(true, Ordering::SeqCst);
    wait_for("waiter to get the lock", || WAITER_GOT_IT.load(Ordering::SeqCst));
    wait_for("lock to be free", || !LOCK.is_locked());
    pass("sleep_lock::waiter_sleeps_until_release");

    // 3. Mutual exclusion even when the holder is switched out mid-update.
    spawn(incrementer);
    spawn(incrementer);
    wait_for("incrementers to finish", || INCREMENTERS_DONE.load(Ordering::SeqCst) == 2);
    assert_eq!(COUNTER.load(Ordering::SeqCst), 2 * INCREMENTS, "an update was lost");
    pass("sleep_lock::mutual_exclusion_across_preemption");

    // 4. A VFS operation waits for the filesystem lock (instead of running
    //    alongside, or spinning).
    spawn(fs_holder);
    wait_for("fs holder", || FS_HOLDER_READY.load(Ordering::SeqCst));
    let user = spawn(vfs_user);
    wait_for("VFS user to block", || state(user) == ThreadState::Blocked);
    sleep(2);
    assert_eq!(VFS_USER_DONE_AT.load(Ordering::SeqCst), 0, "VFS ran while the fs was locked");
    FS_RELEASE.store(true, Ordering::SeqCst);
    wait_for("VFS user to finish", || VFS_USER_DONE_AT.load(Ordering::SeqCst) != 0);
    assert!(VFS_USER_DONE_AT.load(Ordering::SeqCst) >= FS_RELEASED_AT.load(Ordering::SeqCst));
    pass("sleep_lock::vfs_waits_for_the_fs_lock");

    // 5. A long disk write no longer stops the system: the timer keeps
    //    ticking during it, and another thread keeps getting the CPU.
    spawn(ticker);
    spawn(disk_writer);
    wait_for("disk write", || WRITE_TICKS.load(Ordering::SeqCst) != u64::MAX);
    let write_ticks = WRITE_TICKS.load(Ordering::SeqCst);
    let runs = TICKER_RUNS_DURING_WRITE.load(Ordering::SeqCst);
    rust_os::serial_println!(
        "    256 KiB ext2 write: {} ticks, ticker ran {} times meanwhile",
        write_ticks,
        runs
    );
    assert!(write_ticks >= 2, "the write took {} ticks: too fast to tell", write_ticks);
    // It sleeps a tick per run, so it should run about once per tick.
    assert!(
        runs >= write_ticks * 3 / 4,
        "the ticker starved during the write ({} runs in {} ticks)",
        runs,
        write_ticks
    );
    pass("sleep_lock::disk_write_keeps_the_system_running");

    support::finish_suite();
}

#[test_case]
fn run_sleep_lock_steps() {
    support::begin_suite("sleep_lock", STEPS);

    let monitor_tid = spawn(monitor);
    // Replace the boot-time init thread (which runs the executor forever).
    {
        let mut scheduler = SCHEDULER.lock();
        scheduler.clear_runnables();
        scheduler.add_runnable(monitor_tid);
    }
    interrupts::enable();
    yield_now();
    panic!("sleep_lock: boot context resumed unexpectedly");
}

mod support;
