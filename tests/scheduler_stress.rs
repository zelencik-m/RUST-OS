//! Stress the voluntary scheduling paths (`yield_now`, `sleep`, and thread
//! exit) while the timer is preempting at a high rate.
//!
//! These paths are entered with interrupts enabled, unlike the timer path.
//! If the scheduler lets a timer interrupt in between updating `current` and
//! running `switch_context`, a thread resumes on the wrong stack. Every worker
//! therefore checks after each switch that the scheduler's current thread is
//! itself, and that a value kept on its own stack is unchanged.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(stress_test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;
mod support;

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicUsize, Ordering};
use rust_os::process::process_manager;
use rust_os::process::thread::TID;
use rust_os::process::PID;
use rust_os::scheduler::scheduler::{sleep, yield_now, SCHEDULER};
use x86_64::instructions::interrupts;
use x86_64::instructions::port::Port;

/// PIT input clock in Hz.
const PIT_BASE_HZ: u32 = 1_193_182;
/// Preempt far more often than the default ~18.2 Hz, so timer interrupts
/// land inside short scheduler windows.
const PIT_HZ: u32 = 4_000;

const YIELD_WORKERS: usize = 3;
const YIELD_ITERATIONS: usize = 20_000;
const SLEEP_WORKERS: usize = 2;
const SLEEP_ITERATIONS: usize = 2_000;
/// Short-lived threads that exit through the kill path, created one batch at
/// a time so the reaper and TID recycling are exercised too.
const EXIT_THREADS: usize = 300;
const EXIT_BATCH: usize = 4;

/// Wall-clock bound: about 75 s of guest time at `PIT_HZ`.
const TIMEOUT_TICKS: u64 = 300_000;

static WORKERS_DONE: AtomicUsize = AtomicUsize::new(0);
static EXITED: AtomicUsize = AtomicUsize::new(0);

fn stress_test_runner(tests: &[&dyn Fn()]) {
    assert_eq!(tests.len(), 1, "scheduler_stress expects one orchestration test");
    tests[0]();
}

fn set_pit_frequency(hz: u32) {
    let divisor = (PIT_BASE_HZ / hz) as u16;
    unsafe {
        // Channel 0, lobyte/hibyte access, mode 3 (square wave), binary.
        Port::<u8>::new(0x43).write(0x36);
        Port::<u8>::new(0x40).write(divisor as u8);
        Port::<u8>::new(0x40).write((divisor >> 8) as u8);
    }
}

fn current_tid() -> TID {
    SCHEDULER
        .lock()
        .get_current()
        .expect("worker running with no current thread")
}

/// Check the invariants that a context-switch race would break.
fn check_resumed(me: TID, canary: &u64, expected: u64, iteration: usize) {
    let now = current_tid();
    assert_eq!(
        now, me,
        "thread {:?} resumed after iteration {} while the scheduler says {:?} is current",
        me, iteration, now
    );
    assert_eq!(
        core::hint::black_box(*canary),
        expected,
        "thread {:?} stack canary corrupted after iteration {}",
        me,
        iteration
    );
}

/// Park a finished worker without leaving the scheduler.
fn park() -> ! {
    loop {
        sleep(1_000_000);
    }
}

extern "C" fn yield_worker() -> ! {
    let me = current_tid();
    let canary = 0xC0FF_EE00_0000_0000 | me.0;
    for i in 0..YIELD_ITERATIONS {
        yield_now();
        check_resumed(me, &canary, 0xC0FF_EE00_0000_0000 | me.0, i);
    }
    WORKERS_DONE.fetch_add(1, Ordering::SeqCst);
    park();
}

extern "C" fn sleep_worker() -> ! {
    let me = current_tid();
    let canary = 0x5EE9_0000_0000_0000 | me.0;
    for i in 0..SLEEP_ITERATIONS {
        // Mix zero-tick sleeps (immediately due) with one-tick sleeps.
        sleep((i % 2) as u64);
        check_resumed(me, &canary, 0x5EE9_0000_0000_0000 | me.0, i);
    }
    WORKERS_DONE.fetch_add(1, Ordering::SeqCst);
    park();
}

extern "C" fn short_lived() -> ! {
    let me = current_tid();
    let canary = 0xDEAD_0000_0000_0000 | me.0;
    for i in 0..8 {
        yield_now();
        check_resumed(me, &canary, 0xDEAD_0000_0000_0000 | me.0, i);
    }
    EXITED.fetch_add(1, Ordering::SeqCst);
    process_manager::kill_current_thread_and_schedule(me);
}

extern "C" fn exit_spawner() -> ! {
    let mut spawned = 0;
    while spawned < EXIT_THREADS {
        let batch = core::cmp::min(EXIT_BATCH, EXIT_THREADS - spawned);
        for _ in 0..batch {
            process_manager::create_thread(PID(0), short_lived);
        }
        spawned += batch;
        while EXITED.load(Ordering::SeqCst) < spawned {
            yield_now();
        }
    }
    WORKERS_DONE.fetch_add(1, Ordering::SeqCst);
    park();
}

extern "C" fn monitor() -> ! {
    let expected = YIELD_WORKERS + SLEEP_WORKERS + 1;
    loop {
        if WORKERS_DONE.load(Ordering::SeqCst) == expected {
            support::report_pass("scheduler_stress::voluntary_switches_under_fast_preemption");
            support::finish_suite();
        }
        let ticks = SCHEDULER.lock().tick_count();
        if ticks > TIMEOUT_TICKS {
            panic!(
                "scheduler stress timed out: {} of {} workers done, {} of {} threads exited",
                WORKERS_DONE.load(Ordering::SeqCst),
                expected,
                EXITED.load(Ordering::SeqCst),
                EXIT_THREADS
            );
        }
        sleep(10);
    }
}

entry_point!(test_kernel_main);

fn test_kernel_main(boot_info: &'static BootInfo) -> ! {
    rust_os::init(boot_info);
    test_main();
    rust_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    support::test_panic_handler(info)
}

#[test_case]
fn voluntary_switches_under_fast_preemption() {
    support::begin_suite("scheduler_stress", 1);

    let mut tids = alloc::vec::Vec::new();
    tids.push(process_manager::create_thread(PID(0), monitor));
    for _ in 0..YIELD_WORKERS {
        tids.push(process_manager::create_thread(PID(0), yield_worker));
    }
    for _ in 0..SLEEP_WORKERS {
        tids.push(process_manager::create_thread(PID(0), sleep_worker));
    }
    tids.push(process_manager::create_thread(PID(0), exit_spawner));

    // Replace the boot-time init thread (which runs the executor forever).
    {
        let mut scheduler = SCHEDULER.lock();
        scheduler.clear_runnables();
        for tid in &tids {
            scheduler.add_runnable(*tid);
        }
    }

    set_pit_frequency(PIT_HZ);
    interrupts::enable();
    yield_now();
    panic!("scheduler stress: boot context resumed unexpectedly");
}
