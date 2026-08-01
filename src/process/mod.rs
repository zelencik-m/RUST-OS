// src/process/mod.rs

pub mod process_manager;
pub mod thread;

use alloc::rc::{Rc, Weak};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use crate::process::thread::{TID, ThreadState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PID(pub u64);

impl PID {
    pub fn new(value: u64) -> Self {
        PID(value)
    }

    pub fn value(&self) -> u64 {
        self.0
    }
}

enum ProcessState{
    New,
    Running,
    Exited,
    Destroyed,
}

pub enum ProcessType{
    Kernel,
    User,
}

pub struct ProcessIdentity{
    pid: PID,
    process_state: ProcessState,
    name:String,
    process_type: ProcessType,
    parent: Option<PID>, 
    children: Vec<PID>,
}

struct ProcessContext{

}

pub struct Process {
    identity: ProcessIdentity,

    threads: Vec<TID>,
}

impl Process {
    pub fn new(pid: PID, parent: PID, name: String, process_type: ProcessType) -> Self {
            Self{ 
                identity: ProcessIdentity {
                    pid: pid, 
                    process_state: ProcessState::New , 
                    parent: Some(parent), 
                    process_type: process_type,
                    name: name, 
                    children: Vec::new(),
                }, 
                threads: Vec::new(),
            }
    }
}