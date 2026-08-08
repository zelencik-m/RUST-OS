// src/scheduler/scheduler.rs
use alloc::collections::VecDeque;
use crate::debugln;
use crate::process::thread::TID;
use crate::process::thread::ThreadContext;
use lazy_static::lazy_static;
use spin::Mutex;
use crate::process::process_manager::{ProcessManager, switch_to, PROCESS_MANAGER};
use crate::process::thread::ThreadState;
use crate::println;
use x86_64::instructions::interrupts;

const TIME_SLICE: u64 = 2; // Number of ticks before a thread is preempted

lazy_static! {
    pub static ref SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler::new());
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SleepThread{
    tid:TID,
    wakeup_tick:u64,
}

pub struct Scheduler {
    pub current: Option<TID>,
    pub runnables: VecDeque<TID>,
    pub tick_count: u64,    
    pub sleep_queue: VecDeque<SleepThread>,
    pub idle_thread: Option<TID>,
    // Optionally: the kernel context could stay in ProcessManager,
    // but if you prefer you can move it here as well.
}

impl Scheduler {
    pub fn new() -> Self {
        Scheduler {
            current: None,
            runnables: VecDeque::new(),
            tick_count: 0,
            sleep_queue: VecDeque::new(),
            idle_thread: None,
        }
    }

    pub fn add_runnable(&mut self, tid: TID) {
        self.runnables.push_back(tid);
    }

    pub fn next_runnable(&mut self) -> Option<TID> {
        // Optionally filter out TIDs that are no longer valid.
        // For now, just pop front.
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

    // You may also want a method to remove a specific TID from runnables
    // if a thread is destroyed while still in the queue.
    pub fn remove_from_runnables(&mut self, tid: TID) {
        self.runnables.retain(|&x| x != tid);
    }

    fn requeue_and_pick_next(&mut self) -> Option<TID> {
        if let Some(cur) = self.current {
            let mut pm = PROCESS_MANAGER.lock();
            let state = pm.get_thread_state(cur);
            if state == ThreadState::Running {
                pm.preempt_thread(cur);          // <-- was pm.set_thread_ready(cur)
                drop(pm);
                self.runnables.push_back(cur);
            } else if state == ThreadState::Ready {
                drop(pm);
                self.runnables.push_back(cur);
            }
        }

        let mut remaining = self.sleep_queue.len();
        while remaining > 0 {
            if let Some(sleep_thread) = self.sleep_queue.pop_front() {
                remaining -= 1;
                let mut pm = PROCESS_MANAGER.lock();
                if self.tick_count >= sleep_thread.wakeup_tick
                    && pm.get_thread_state(sleep_thread.tid) == ThreadState::Sleeping
                {
                    // Wake the thread: set state to Ready and add to run queue
                    pm.set_thread_ready(sleep_thread.tid);
                    drop(pm);
                    self.add_runnable(sleep_thread.tid);
                } else {
                    self.sleep_queue.push_back(sleep_thread);
                }
            }
        }

        // Pick next runnable thread. Don't silently drop TIDs that
        // aren't Ready yet — put them back at the tail instead.
        if self.runnables.is_empty() {
            debugln!("No runnable threads, switching to idle thread");
            return self.idle_thread;
        } else {
            let mut scanned = 0;
            let total = self.runnables.len();
            while scanned < total {
                if let Some(next) = self.next_runnable() {
                    let pm = PROCESS_MANAGER.lock();
                    if pm.get_thread_state(next) == ThreadState::Ready {
                        return Some(next);
                    } else {
                        drop(pm);
                        self.runnables.push_back(next); // don't lose it
                    }
                }
                scanned += 1;
            }
        }
        None
    }

    pub fn tick(&mut self) -> bool {
        debugln!("Tick: {}", self.tick_count);
        self.tick_count = self.tick_count.wrapping_add(1);
        if self.tick_count % TIME_SLICE == 0 {
            true // signal: time to reschedule
        } else {
            false
        }
    }

}


pub fn schedule() {
    interrupts::without_interrupts(|| {
        let mut scheduler = SCHEDULER.lock();
        let next = scheduler.requeue_and_pick_next();
        if let Some(tid) = next {
            debugln!("Switching to thread {:?}", tid);
            debugln!("Queue: {:?}", scheduler.runnables);
            drop(scheduler); // release before touching PROCESS_MANAGER/switch_to
            PROCESS_MANAGER.lock().run_thread(tid);
            switch_to(tid);
        }
    });
}

pub fn yield_now() {
    schedule();
}

pub fn sleep(ticks: u64) {
    interrupts::without_interrupts(|| {
        let mut scheduler = SCHEDULER.lock();
        if let Some(current_tid) = scheduler.get_current() {
            debugln!("Thread {:?} sleeping for {} ticks", current_tid, ticks);
            let wakeup_tick = scheduler.tick_count.wrapping_add(ticks);
            scheduler.sleep_queue.push_back(SleepThread { tid: current_tid, wakeup_tick });
            PROCESS_MANAGER.lock().sleep_thread(current_tid);
            drop(scheduler);
        } else {
            panic!("No current thread to sleep");
        }
    });
    schedule();
}