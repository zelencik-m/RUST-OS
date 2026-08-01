// src/process/process_manager

use alloc::vec::Vec;
use alloc::collections::VecDeque;
use alloc::string::String;

use crate::process::{Process, PID, ProcessType};
use crate::process::thread::{TID, Thread, ThreadState};

struct ProcessManager {
    processes: Vec<Option<Process>>,
    threads: Vec<Option<Thread>>,

    runables: VecDeque<TID>,
}

impl ProcessManager {
    pub fn create_process(&mut self, parent: u64, name: String) -> PID {
        // Find first free slot, or append a new one
        let mut new_pid = PID(self.processes.len() as u64);
        if let Some(free_index) = self.processes.iter().position(|p| p.is_none()) {
            new_pid = PID(free_index as u64);
            self.processes[free_index] = Some(Process::new(new_pid, PID(parent), name, ProcessType::User));
        } else {
            self.processes.push(Some(Process::new(new_pid, PID(parent), name, ProcessType::User)));
        }

        // create a main thread for the new process


        new_pid
    }

    pub fn remove_process(&mut self, pid: PID) -> bool {
        let index = pid.0 as usize;
        if index >= self.processes.len() || self.processes[index].is_none() {
            return false;
        }

        // Dropping the Process here – no more Rc/RefCell, so it's freed immediately
        self.processes[index] = None;
        true
    }
}