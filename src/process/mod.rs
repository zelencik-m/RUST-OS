// src/process/mod.rs


use alloc::rc::{Rc, Weak};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

struct PID(u64);

enum ProcessState{
    NEW,
    RUNNING,
    EXITED,
    DESTROYED,
}

struct ProcessIdentity{
    pid: PID,
    process_state: ProcessState,
    parrent: RefCell<Weak<Process>>,
    name:String,
    children: RefCell<Vec<Rc<Process>>>,
}

struct ProcessContext{

}

pub struct Process{
    identity: ProcessIdentity,
    context: ProcessContext,
}