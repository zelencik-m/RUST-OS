//! Blocking with real threads: `WaitQueue` and `tty::read`.
//!
//! One orchestration test (like `scheduler_stress`): a monitor thread starts
//! reader/waiter threads, waits until the scheduler reports them `Blocked`,
//! then delivers the event (sets a flag, types keys with `tty::push_key`) and
//! checks who woke up with what. Each numbered step is reported as its own
//! PASS line.
//!
//! Typed keys are echoed in cooked mode, so the echoed text shows up in the
//! serial log between the PASS lines. That's expected.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(blocking_test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::vec::Vec;
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use pc_keyboard::DecodedKey;
use rust_os::process::process_manager::{self, PROCESS_MANAGER};
use rust_os::process::thread::{ThreadState, TID};
use rust_os::process::PID;
use rust_os::scheduler::scheduler::{sleep, yield_now, SCHEDULER};
use rust_os::scheduler::wait_queue::WaitQueue;
use rust_os::sync::IrqSafeMutex;
use rust_os::tty::{self, TtyMode};
use x86_64::instructions::interrupts;

/// Per-wait bound. The PIT runs at ~18.2 Hz, so this is about 16 s.
const WAIT_TIMEOUT_TICKS: u64 = 300;
const STEPS: usize = 8;

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

fn blocking_test_runner(tests: &[&dyn Fn()]) {
    assert_eq!(tests.len(), 1, "tty_blocking expects one orchestration test");
    tests[0]();
}

// ---------------------------------------------------------------------------
// Worker threads
// ---------------------------------------------------------------------------

static FLAG: AtomicBool = AtomicBool::new(false);
static QUEUE: WaitQueue = WaitQueue::new();
static FLAG_WAITERS_DONE: AtomicUsize = AtomicUsize::new(0);

/// What each `tty_reader` got back from one `tty::read`, in completion order.
static READS: IrqSafeMutex<Vec<Vec<u8>>> = IrqSafeMutex::new(Vec::new());

fn park() -> ! {
    loop {
        sleep(1_000_000);
    }
}

extern "C" fn flag_waiter() -> ! {
    QUEUE.wait_until(|| FLAG.load(Ordering::SeqCst).then_some(()));
    FLAG_WAITERS_DONE.fetch_add(1, Ordering::SeqCst);
    park()
}

extern "C" fn tty_reader() -> ! {
    let mut buf = [0u8; 64];
    let n = tty::read(&mut buf).expect("kernel threads are never killed");
    READS.lock().push(buf[..n].to_vec());
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
    let deadline = SCHEDULER.lock().tick_count() + WAIT_TIMEOUT_TICKS;
    while !condition() {
        if SCHEDULER.lock().tick_count() > deadline {
            panic!("timed out waiting for: {}", what);
        }
        sleep(1);
    }
}

fn wait_blocked(tid: TID) {
    wait_for("thread to block", || state(tid) == ThreadState::Blocked);
}

fn type_text(text: &str) {
    for c in text.chars() {
        tty::push_key(DecodedKey::Unicode(c));
    }
}

fn reads() -> Vec<Vec<u8>> {
    READS.lock().clone()
}

fn reads_len() -> usize {
    READS.lock().len()
}

fn pass(step: &str) {
    support::report_pass(step);
}

// ---------------------------------------------------------------------------
// The steps
// ---------------------------------------------------------------------------

extern "C" fn monitor() -> ! {
    // 1. A false condition blocks the thread.
    FLAG.store(false, Ordering::SeqCst);
    let waiter = spawn(flag_waiter);
    wait_blocked(waiter);
    assert_eq!(FLAG_WAITERS_DONE.load(Ordering::SeqCst), 0);
    pass("tty_blocking::wait_queue_blocks_while_condition_is_false");

    // 2. A wakeup without the condition (spurious) is followed by a re-check,
    //    and the thread blocks again instead of returning.
    QUEUE.wake_all();
    wait_blocked(waiter);
    sleep(2);
    wait_blocked(waiter);
    assert_eq!(
        FLAG_WAITERS_DONE.load(Ordering::SeqCst),
        0,
        "wait_until returned although the condition is still false"
    );
    pass("tty_blocking::wait_queue_spurious_wakeup_blocks_again");

    // 3. Condition true + wake_all → the waiter returns.
    FLAG.store(true, Ordering::SeqCst);
    QUEUE.wake_all();
    wait_for("flag waiter to return", || FLAG_WAITERS_DONE.load(Ordering::SeqCst) == 1);
    pass("tty_blocking::wait_queue_wakes_when_condition_holds");

    // 4. A condition that is already true returns without blocking. Nobody
    //    calls wake_all here, so a waiter that blocked would never finish.
    spawn(flag_waiter);
    wait_for("second flag waiter to return without a wakeup", || {
        FLAG_WAITERS_DONE.load(Ordering::SeqCst) == 2
    });
    pass("tty_blocking::wait_queue_true_condition_does_not_block");

    // 5. Keys typed before anyone reads are returned without blocking.
    type_text("early\n");
    spawn(tty_reader);
    wait_for("reader of pre-typed input", || reads_len() == 1);
    assert_eq!(reads()[0], b"early\n");
    pass("tty_blocking::input_typed_before_read_does_not_block");

    // 6. A reader blocks until a whole line arrives. A partial line keeps it
    //    blocked.
    let reader = spawn(tty_reader);
    wait_blocked(reader);
    type_text("hel");
    sleep(3);
    wait_blocked(reader);
    assert_eq!(reads_len(), 1, "reader returned before Enter");
    type_text("lo\n");
    wait_for("blocked reader to get its line", || reads_len() == 2);
    assert_eq!(reads()[1], b"hello\n");
    pass("tty_blocking::reader_blocks_until_a_line_arrives");

    // 7. Two readers, one line: exactly one gets it, and the other goes back
    //    to sleep until the next line.
    let first = spawn(tty_reader);
    let second = spawn(tty_reader);
    wait_blocked(first);
    wait_blocked(second);
    type_text("one\n");
    wait_for("one of two readers to get the line", || reads_len() == 3);
    sleep(3);
    wait_for("the other reader to block again", || {
        state(first) == ThreadState::Blocked || state(second) == ThreadState::Blocked
    });
    assert_eq!(reads_len(), 3, "both readers returned for a single line");
    type_text("two\n");
    wait_for("second reader to get the next line", || reads_len() == 4);
    let got = reads();
    assert_eq!(got[2], b"one\n");
    assert_eq!(got[3], b"two\n");
    pass("tty_blocking::two_readers_get_one_line_each");

    // 8. Raw mode wakes a reader on a single byte; cooked Ctrl-D wakes a
    //    reader with EOF (0 bytes).
    assert_eq!(tty::set_mode(TtyMode::Raw), TtyMode::Cooked);
    let raw_reader = spawn(tty_reader);
    wait_blocked(raw_reader);
    type_text("x");
    wait_for("raw reader to get one byte", || reads_len() == 5);
    assert_eq!(reads()[4], b"x");
    assert_eq!(tty::set_mode(TtyMode::Cooked), TtyMode::Raw);

    let eof_reader = spawn(tty_reader);
    wait_blocked(eof_reader);
    type_text("\u{4}");
    wait_for("EOF reader to return", || reads_len() == 6);
    assert!(reads()[5].is_empty(), "Ctrl-D on an empty line must read 0 bytes");
    pass("tty_blocking::raw_byte_and_cooked_eof_wake_readers");

    support::finish_suite();
}

#[test_case]
fn run_tty_blocking_steps() {
    support::begin_suite("tty_blocking", STEPS);

    let monitor_tid = spawn(monitor);
    // Replace the boot-time init thread (which runs the executor forever).
    {
        let mut scheduler = SCHEDULER.lock();
        scheduler.clear_runnables();
        scheduler.add_runnable(monitor_tid);
    }
    interrupts::enable();
    yield_now();
    panic!("tty_blocking: boot context resumed unexpectedly");
}

mod support;
