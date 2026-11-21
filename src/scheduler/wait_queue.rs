//! Wait queues: block the current thread until a condition holds.
//!
//! This is the kernel's general "sleep until something happens" primitive.
//! `scheduler::sleep` waits for a deadline; a `WaitQueue` waits for an event
//! (a key arrives, a child process exits, ...). The waiter describes what it
//! is waiting for as a condition closure, and whoever changes that state
//! calls `wake_all`.

use alloc::collections::VecDeque;
use x86_64::instructions::interrupts;

use crate::process::process_manager::PROCESS_MANAGER;
use crate::process::thread::{ThreadState, TID};
use crate::scheduler::scheduler::{schedule_with_interrupts_disabled, SCHEDULER};
use crate::sync::IrqSafeMutex;

pub struct WaitQueue {
    /// Threads that went to sleep on this queue, in arrival order.
    ///
    /// Entries can go stale (e.g. the thread was killed, or a woken thread
    /// was not removed). `wake_all` must therefore only wake threads that
    /// are still `Blocked`, and waiters must re-check their condition after
    /// every wakeup.
    waiters: IrqSafeMutex<VecDeque<TID>>,
}

impl WaitQueue {
    pub const fn new() -> Self {
        Self {
            waiters: IrqSafeMutex::new(VecDeque::new()),
        }
    }

    /// Number of queued waiter entries. Tests use it; it may include stale
    /// entries.
    pub fn waiter_count(&self) -> usize {
        self.waiters.lock().len()
    }

    /// Block the current thread until `condition` returns `Some`, and return
    /// that value.
    ///
    /// **No lost wakeups.** The danger is an event that fires *between*
    /// "condition is false" and "I am blocked": its `wake_all` finds nobody
    /// to wake, and the thread then sleeps forever. On this single-CPU
    /// kernel every such event is delivered by an interrupt (directly, or
    /// via the context switch that lets another thread run). So the check,
    /// the enqueue, marking the thread `Blocked` and switching away all
    /// happen with interrupts disabled. An event can only be delivered
    /// before the check (the check sees it) or after the switch (the thread
    /// is queued and `Blocked`, so `wake_all` finds it).
    ///
    /// **Spurious wakeups.** A wakeup only means "something changed". Another
    /// waiter may already have taken the data, or a stale entry for a reused
    /// TID may have been woken. So after every wakeup the condition is
    /// checked again.
    ///
    /// `condition` runs with interrupts disabled. It may take its own locks
    /// (e.g. the TTY), but must release them before returning. No lock is
    /// held across the switch.
    pub fn wait_until<T>(&self, mut condition: impl FnMut() -> Option<T>) -> T {
        assert_may_block();
        loop {
            let ready = interrupts::without_interrupts(|| {
                if let Some(value) = condition() {
                    return Some(value);
                }
                let tid = SCHEDULER
                    .lock()
                    .get_current()
                    .expect("WaitQueue::wait_until with no current thread");
                self.waiters.lock().push_back(tid);
                PROCESS_MANAGER.lock().block_thread(tid);
                // `requeue_and_pick_next` doesn't requeue Blocked threads, so
                // this returns only after `wake_all` made us runnable again.
                schedule_with_interrupts_disabled();
                None
            });
            if let Some(value) = ready {
                return value;
            }
        }
    }

    /// Wake every thread waiting on this queue.
    ///
    /// Callable from interrupt handlers (the keyboard IRQ), so it neither
    /// allocates nor frees. It pops entries one by one instead of swapping
    /// out the deque, because dropping the old deque would free its buffer.
    /// Pushing onto the run queue doesn't allocate either, thanks to the
    /// capacity reserved at boot.
    ///
    /// Only threads that are still `Blocked` are woken. Anything else is a
    /// stale entry: the thread was killed, or its TID is gone or reused.
    ///
    /// Lock order: waiters → SCHEDULER → PROCESS_MANAGER.
    pub fn wake_all(&self) {
        let mut waiters = self.waiters.lock();
        if waiters.is_empty() {
            return;
        }
        let mut scheduler = SCHEDULER.lock();
        let mut pm = PROCESS_MANAGER.lock();
        while let Some(tid) = waiters.pop_front() {
            if pm.thread_exists(tid) && pm.get_thread_state(tid) == ThreadState::Blocked {
                pm.wake_thread(tid);
                scheduler.add_runnable(tid);
            }
        }
    }
}

/// Panic if the current context must not block.
///
/// A blocking wait has to run in plain thread context with interrupts on.
/// Interrupts are off in two cases: inside an interrupt handler, and while
/// an `IrqSafeMutex` is held, e.g. `sys_read` holding PROCESS_MANAGER while
/// calling into a device. Blocking there would switch away with that lock
/// held, and the scheduler's next lock of it would spin forever.
pub fn assert_may_block() {
    assert!(
        interrupts::are_enabled(),
        "blocking wait with interrupts disabled: an IrqSafeMutex (e.g. \
         PROCESS_MANAGER in sys_read) is held, or this is an interrupt handler"
    );
}
