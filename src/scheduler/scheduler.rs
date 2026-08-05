// scheduler.rs
use alloc::collections::VecDeque;
use crate::process::thread::TID;
use crate::process::thread::ThreadContext;
use lazy_static::lazy_static;
use spin::Mutex;
use crate::process::process_manager::{ProcessManager, switch_to, PROCESS_MANAGER};
use crate::println;


lazy_static! {
    pub static ref SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler::new());
}

pub struct Scheduler {
    pub current: Option<TID>,
    pub runnables: VecDeque<TID>,
    // Optionally: the kernel context could stay in ProcessManager,
    // but if you prefer you can move it here as well.
}

impl Scheduler {
    pub fn new() -> Self {
        Scheduler {
            current: None,
            runnables: VecDeque::new(),
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

    // You may also want a method to remove a specific TID from runnables
    // if a thread is destroyed while still in the queue.
    pub fn remove_from_runnables(&mut self, tid: TID) {
        self.runnables.retain(|&x| x != tid);
    }

    fn requeue_and_pick_next(&mut self) -> Option<TID> {
        if let Some(cur) = self.current {     // read, don't take
            self.runnables.push_back(cur);
        }
        self.next_runnable()
    }
}

pub fn schedule() {
    let next = SCHEDULER.lock().requeue_and_pick_next(); // lock released here
    if let Some(tid) = next {
        // println!("Switching to thread {:?}", tid);
        switch_to(tid);
    }
}

pub fn yield_now() {
    schedule();
}