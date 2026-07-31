// src/process/process_manager

use alloc::vec::Vec;
use alloc::collections::VecDeque;
use alloc::string::String;

use crate::process::{Process, PID, ProcessType};

struct ProcessManager {
    pub processes: Vec<Option<Process>>,   // now owns each Process directly
    runables: VecDeque<PID>,
}

impl ProcessManager {
    pub fn create_process(&mut self, parent: u64, name: String) -> PID {
        // Find first free slot, or append a new one
        if let Some(free_index) = self.processes.iter().position(|p| p.is_none()) {
            let pid = PID(free_index as u64);
            self.processes[free_index] = Some(Process::new(pid, PID(parent), name, ProcessType::User));
            pid
        } else {
            let new_pid = self.processes.len() as u64;
            let pid = PID(new_pid);
            self.processes.push(Some(Process::new(pid, PID(parent), name, ProcessType::User)));
            pid
        }
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