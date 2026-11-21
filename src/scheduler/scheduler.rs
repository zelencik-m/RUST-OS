use alloc::collections::{BinaryHeap, VecDeque};
use core::cmp::Reverse;
use crate::debugln;
use crate::process::thread::TID;
use lazy_static::lazy_static;
use crate::process::process_manager::{switch_to, PROCESS_MANAGER};
use crate::process::thread::ThreadState;
use crate::sync::IrqSafeMutex;
use x86_64::instructions::interrupts;

const TIME_SLICE: u64 = 2; // Number of ticks before a thread is preempted

lazy_static! {
    pub static ref SCHEDULER: IrqSafeMutex<Scheduler> = IrqSafeMutex::new(Scheduler::new());
}

/// A sleeping thread, ordered by wake-up tick (then TID, for a total order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SleepEntry {
    wakeup_tick: u64,
    tid: u64,
}

pub struct Scheduler {
    current: Option<TID>,
    runnables: VecDeque<TID>,
    tick_count: u64,
    /// Min-heap on `wakeup_tick`: the earliest wake-up is always on top, so
    /// `schedule()` only looks at threads that are actually due.
    sleep_queue: BinaryHeap<Reverse<SleepEntry>>,
    idle_thread: Option<TID>,
}

impl Scheduler {
    pub fn new() -> Self {
        Scheduler {
            current: None,
            runnables: VecDeque::new(),
            tick_count: 0,
            sleep_queue: BinaryHeap::new(),
            idle_thread: None,
        }
    }

    pub fn add_runnable(&mut self, tid: TID) {
        self.runnables.push_back(tid);
    }

    pub fn next_runnable(&mut self) -> Option<TID> {
        self.runnables.pop_front()
    }

    pub fn set_current(&mut self, tid: TID) {
        self.current = Some(tid);
    }

    pub fn get_current(&self) -> Option<TID> {
        self.current
    }

    pub fn set_idle_thread(&mut self, tid: TID) {
        self.idle_thread = Some(tid);
    }

    pub fn tick_count(&self) -> u64 {
        self.tick_count
    }

    pub fn is_runnable(&self, tid: TID) -> bool {
        self.runnables.contains(&tid)
    }

    /// Whether any thread is queued to run (the idle thread is never queued).
    pub fn has_runnable(&self) -> bool {
        !self.runnables.is_empty()
    }

    /// Pre-allocate room for `additional` more queued threads.
    ///
    /// Interrupt handlers (the timer's requeue, the keyboard waking readers)
    /// push onto the run queue. Growing it there would allocate inside an
    /// interrupt, and the heap allocator's lock is not IRQ-safe: if the
    /// interrupted code was in the middle of an allocation, that deadlocks.
    /// A `VecDeque` never shrinks, so reserving once at boot keeps those
    /// pushes allocation-free as long as fewer threads are queued.
    pub fn reserve_queue_capacity(&mut self, additional: usize) {
        self.runnables.reserve(additional);
    }

    /// Drop every queued runnable thread. Used by test kernels that replace
    /// the boot-time threads with their own.
    pub fn clear_runnables(&mut self) {
        self.runnables.clear();
    }

    /// Remove `tid` from the run queue and the sleep queue.
    ///
    /// Call this whenever a thread is destroyed. Otherwise a stale sleep
    /// entry could wake an unrelated thread that later reuses the same TID.
    pub fn remove_from_queues(&mut self, tid: TID) {
        self.runnables.retain(|&x| x != tid);
        self.sleep_queue.retain(|Reverse(entry)| entry.tid != tid.0);
    }

    fn requeue_and_pick_next(&mut self) -> Option<TID> {
        let mut pm = PROCESS_MANAGER.lock();

        if let Some(cur) = self.current {
            let state = pm.get_thread_state(cur);
            if Some(cur) != self.idle_thread
                && matches!(state, ThreadState::Running | ThreadState::Ready)
            {
                self.runnables.push_back(cur);
            }
        }

        // Wake every thread whose deadline has passed. The heap is ordered by
        // deadline, so stop at the first entry that is still in the future.
        while let Some(&Reverse(entry)) = self.sleep_queue.peek() {
            if entry.wakeup_tick > self.tick_count {
                break;
            }
            self.sleep_queue.pop();

            let tid = TID(entry.tid);
            // A thread that is no longer Sleeping (e.g. it was killed) just
            // has its stale entry dropped.
            if pm.get_thread_state(tid) == ThreadState::Sleeping {
                pm.wake_thread(tid);
                self.runnables.push_back(tid);
            }
        }
        drop(pm);

        if self.runnables.is_empty() {
            debugln!("No runnable threads, switching to idle thread");
            return self.idle_thread;
        }
        self.next_runnable()
    }

    /// Advance the tick counter. Returns `true` when the current time slice
    /// has expired and the caller should reschedule.
    pub fn tick(&mut self) -> bool {
        self.tick_count = self.tick_count.wrapping_add(1);
        self.tick_count % TIME_SLICE == 0
    }
}

/// Pick the next thread and switch to it.
///
/// Runs with interrupts disabled from start to finish. `switch_to` marks the
/// new thread as current before `switch_context` moves onto its stack. A
/// timer interrupt in that gap would run the scheduler on the old thread's
/// stack while `current` names the new thread, and `switch_context` would save
/// the old stack pointer into the new thread's context. Each thread resumes
/// with its own interrupt flag because `switch_context` saves and restores
/// RFLAGS, and `without_interrupts` re-enables interrupts here if the caller
/// had them on.
pub fn schedule() {
    interrupts::without_interrupts(schedule_with_interrupts_disabled);
}

pub(crate) fn schedule_with_interrupts_disabled() {
    debug_assert!(!interrupts::are_enabled());
    let mut scheduler = SCHEDULER.lock();
    let next = scheduler.requeue_and_pick_next();
    if let Some(tid) = next {
        debugln!("Switching to thread {:?}", tid);
        debugln!("Queue: {:?}", scheduler.runnables);
        drop(scheduler); // release before touching PROCESS_MANAGER/switch_to
        switch_to(tid);
    }
}

pub fn yield_now() {
    schedule();
}

/// Put the current thread to sleep for `ticks` timer ticks.
///
/// Queuing the thread and switching away happen with interrupts disabled, so
/// no interrupt can run the scheduler while the thread is marked Sleeping but
/// still running.
pub fn sleep(ticks: u64) {
    interrupts::without_interrupts(|| {
        {
            let mut scheduler = SCHEDULER.lock();
            let current_tid = scheduler
                .get_current()
                .expect("No current thread to sleep");
            debugln!("Thread {:?} sleeping for {} ticks", current_tid, ticks);
            let wakeup_tick = scheduler.tick_count.wrapping_add(ticks);
            scheduler.sleep_queue.push(Reverse(SleepEntry {
                wakeup_tick,
                tid: current_tid.0,
            }));
            PROCESS_MANAGER.lock().sleep_thread(current_tid);
        }
        schedule_with_interrupts_disabled();
    });
}
