// src/process/mod.rs

pub mod process_manager;

use alloc::rc::{Rc, Weak};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

pub struct PID(u64);

enum ProcessState{
    NEW,
    RUNNING,
    EXITED,
    DESTROYED,
}

struct ProcessIdentity{
    pid: PID,
    process_state: ProcessState,
    name:String,
    parrent: RefCell<Weak<RefCell<Process>>>, 
    children: RefCell<Vec<Rc<RefCell<Process>>>>,
}

struct ProcessContext{

}

pub struct Process{
    identity: ProcessIdentity,
    context: ProcessContext,
}

impl Process {
    pub fn new(pid: u64, parrent: RefCell<Weak<RefCell<Process>>>, name: String) -> Self {
            Self{ 
                identity: ProcessIdentity {
                    pid: PID(pid), 
                    process_state: ProcessState::NEW , 
                    parrent: parrent, 
                    name: name, 
                    children: RefCell::new(Vec::new()),
                }, 
                context: ProcessContext {

                } 
            }
    }
}