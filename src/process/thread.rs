// src/process/thread.rs

use alloc::rc::{Rc, Weak};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use crate::process::{PID, Process};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TID(pub u64);

impl TID {
    pub fn new(value: u64) -> Self {
        TID(value)
    }

    pub fn value(&self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadState {
    Ready,
    Running,
    Blocked,
    Exited,
}

pub struct ThreadContext {

}

pub struct Thread {
    tid: TID,

    owner: PID,

    state: ThreadState,

    context: ThreadContext,
}