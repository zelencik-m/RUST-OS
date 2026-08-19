// src/process/mod.rs

pub mod process_manager;
pub mod thread;
pub mod idle;
pub mod elf_loader;

use alloc::rc::{Rc, Weak};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use crate::process::thread::{TID, ThreadState};
use crate::memory::{VmArea, AddressSpace};

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

pub enum ProcessType{
    Kernel,
    User,
}

pub struct ProcessIdentity{
    pid: PID,
    name:String,
    process_type: ProcessType,
    parent: Option<PID>, 
    children: Vec<PID>,
}

struct ProcessContext{

}

pub struct Process {
    identity: ProcessIdentity,
    pub threads: Vec<TID>,
    pub address_space: Option<AddressSpace>,
    // MILESTONE 1.1: `vmas` has been removed from Process and moved to AddressSpace
}

impl Process {
    pub fn new(pid: PID, parent: PID, name: String, process_type: ProcessType) -> Self {
            Self{ 
                identity: ProcessIdentity {
                    pid: pid, 
                    parent: Some(parent), 
                    process_type: process_type,
                    name: name, 
                    children: Vec::new(),
                }, 
            threads: Vec::new(),
            address_space: None,
            }
    }

    pub fn add_child(&mut self, child_pid: PID) {
        self.identity.children.push(child_pid);
    }

    pub fn get_children(&self) -> &Vec<PID> {
        &self.identity.children
    }

    pub fn new_user(pid: PID, parent: PID, name: String, space: AddressSpace) -> Self {
        let mut p = Self::new(pid, parent, name, ProcessType::User);
        p.address_space = Some(space);
        p
    }
}