// src/process/process_manager

use alloc::vec::Vec;
use alloc::collections::VecDeque;
use core::cell::RefCell;
use alloc::rc::{Rc, Weak};
use alloc::string::{String, ToString};

use crate::process;

use process::{Process, PID};

struct ProcessManager{
    pub processes: Vec<Option<Rc<RefCell<Process>>>>,
    runables: VecDeque<PID>,
}

impl ProcessManager {
    pub fn create_process(&mut self, parrent: RefCell<Weak<RefCell<Process>>>, name: String) -> PID {
        if let Some(free_pid) = self.processes.iter().position(|p| p.is_none()) {
            self.processes[free_pid] = Some(Rc::new(RefCell::new(Process::new(free_pid as u64, parrent, name))));
            return PID(free_pid as u64);
        } else {
            let new_pid = self.processes.len();
            self.processes.push(Some(Rc::new(RefCell::new(Process::new(new_pid as u64, parrent, name)))));
            return PID(new_pid as u64);
        }
    }

    pub fn remove_process(&mut self, pid: PID) -> bool {
        let index = pid.0 as usize;
        if index >= self.processes.len() || self.processes[index].is_none() {
            return false;
        }

        // Setting to None drops the Rc. 
        // NOTE: The Process won't fully delete from memory until the Parent's 
        // `children` Vec also drops its Rc to this process!
        self.processes[index] = None;
        true
    }
}
