use alloc::vec::Vec;
use alloc::boxed::Box;

use lazy_static::lazy_static;

use crate::process::thread::{TID, Thread, ThreadContext};
use crate::process::{PID, Process};
use crate::sync::IrqSafeMutex;

pub struct ProcessManager {
    pub(crate) processes: Vec<Option<Process>>,
    pub(crate) threads: Vec<Option<Box<Thread>>>,
    pub(crate) free_pids: Vec<PID>,
    pub(crate) free_tids: Vec<TID>,
    pub(crate) kernel_context: ThreadContext,
}

lazy_static! {
    pub static ref PROCESS_MANAGER: IrqSafeMutex<ProcessManager> =
        IrqSafeMutex::new(ProcessManager::new());
}

impl ProcessManager {
    pub fn new() -> Self {
        ProcessManager {
            processes: Vec::new(),
            threads: Vec::new(),
            free_pids: Vec::new(),
            free_tids: Vec::new(),
            kernel_context: ThreadContext::zeroed(),
        }
    }

    // --- accessors ---

    pub fn thread(&self, tid: TID) -> &Thread {
        self.threads
            .get(tid.0 as usize)
            .and_then(Option::as_ref)
            .map(|b| &**b)
            .expect("thread(): invalid TID")
    }

    pub fn thread_mut(&mut self, tid: TID) -> &mut Thread {
        self.threads
            .get_mut(tid.0 as usize)
            .and_then(Option::as_mut)
            .map(|b| &mut **b)
            .expect("thread_mut(): invalid TID")
    }

    pub fn process(&self, pid: PID) -> &Process {
        self.processes
            .get(pid.0 as usize)
            .and_then(Option::as_ref)
            .expect("process(): invalid or empty process slot")
    }

    pub fn process_mut(&mut self, pid: PID) -> &mut Process {
        self.processes
            .get_mut(pid.0 as usize)
            .and_then(Option::as_mut)
            .expect("process_mut(): invalid or empty process slot")
    }
}