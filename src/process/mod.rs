pub mod process_manager;
pub mod thread;
pub mod idle;
pub mod elf_loader;
pub mod usermode;
pub mod initial_stack;

use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::{File, FsError, Path};
use crate::fs::devices::{stdin_file, stdout_file, stderr_file};
use crate::process::thread::TID;
use crate::memory::AddressSpace;
use crate::scheduler::wait_queue::WaitQueue;

/// Exit code of a process killed by a fatal CPU exception (page fault,
/// #GP, ...). Shells report 128 + signal number, and Linux sends SIGSEGV
/// (11) for these faults.
pub const EXIT_FAULT: i32 = 128 + 11;
/// Exit code of a process killed by Ctrl-C: 128 + SIGINT (2).
pub const EXIT_INTERRUPTED: i32 = 128 + 2;

/// Woken every time a process exits. `wait4` blocks here until the child it
/// waits for has exited.
///
/// One queue for the whole system, not one per parent: `wake_all` takes the
/// SCHEDULER and PROCESS_MANAGER locks itself, so it can't be reached from
/// inside a `Process` while PROCESS_MANAGER is held. Waking every waiter on
/// every exit is fine: each one re-checks its own child and sleeps again.
pub static CHILD_EXITS: WaitQueue = WaitQueue::new();

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

pub enum ProcessType {
    Kernel,
    User,
}

pub struct ProcessIdentity {
    pid: PID,
    name: String,
    process_type: ProcessType,
    parent: Option<PID>,
    children: Vec<PID>,
}

const MAX_FDS: usize = 256;

pub struct Process {
    identity: ProcessIdentity,
    pub threads: Vec<TID>,
    pub address_space: Option<AddressSpace>,

    /// Per-process open-file table. The integer returned to userspace is the
    /// index into this table.
    pub fd_table: Vec<Option<File>>,

    /// Canonical absolute current working directory.
    pub cwd: Path,

    /// `None` while the process runs. Set once, when its last thread exits:
    /// the code `wait4` reports. A process whose parent is a live user
    /// process stays in the process table with this set (a *zombie*) until
    /// that parent collects it with `wait4`.
    pub exit_code: Option<i32>,

    /// A kill has been requested (Ctrl-C), with this exit code. The process
    /// is not torn down from outside. Its own thread exits at the next safe
    /// point: on the way back to user mode from a syscall, or on a timer
    /// tick that interrupted user mode. See `request_kill`.
    pub pending_kill: Option<i32>,
}

impl Process {
    pub fn new(pid: PID, parent: PID, name: String, process_type: ProcessType) -> Self {
        Self {
            identity: ProcessIdentity {
                pid,
                parent: Some(parent),
                process_type,
                name,
                children: Vec::new(),
            },
            threads: Vec::new(),
            address_space: None,
            fd_table: Vec::new(),
            cwd: Path::root(),
            exit_code: None,
            pending_kill: None,
        }
    }

    pub fn pid(&self) -> PID {
        self.identity.pid
    }

    pub fn name(&self) -> &str {
        &self.identity.name
    }

    pub fn process_type(&self) -> &ProcessType {
        &self.identity.process_type
    }

    pub fn parent(&self) -> Option<PID> {
        self.identity.parent
    }

    pub fn add_child(&mut self, child_pid: PID) {
        self.identity.children.push(child_pid);
    }

    /// Empty the `children` list and return what was in it.
    pub fn take_children(&mut self) -> Vec<PID> {
        core::mem::take(&mut self.identity.children)
    }

    pub fn remove_child(&mut self, child_pid: PID) {
        self.identity.children.retain(|&pid| pid != child_pid);
    }

    /// Re-parent this process (e.g. to PID 0 when its parent exits). Callers
    /// keep both parents' `children` lists in sync.
    pub fn set_parent(&mut self, parent: PID) {
        self.identity.parent = Some(parent);
    }

    pub fn is_user(&self) -> bool {
        matches!(self.identity.process_type, ProcessType::User)
    }

    pub fn get_children(&self) -> &Vec<PID> {
        &self.identity.children
    }

    pub fn new_user(pid: PID, parent: PID, name: String, space: AddressSpace) -> Self {
        let mut p = Self::new(pid, parent, name, ProcessType::User);
        p.address_space = Some(space);
        p.init_stdio();
        p
    }

    /// Install the standard streams as ordinary file objects.
    pub fn init_stdio(&mut self) {
        self.fd_table.clear();
        self.fd_table.push(Some(stdin_file()));
        self.fd_table.push(Some(stdout_file()));
        self.fd_table.push(Some(stderr_file()));
    }

    pub fn alloc_fd(&mut self, file: File) -> Result<u64, FsError> {
        for (fd, slot) in self.fd_table.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(file);
                return Ok(fd as u64);
            }
        }

        if self.fd_table.len() >= MAX_FDS {
            return Err(FsError::TooManyFiles);
        }

        let fd = self.fd_table.len() as u64;
        self.fd_table.push(Some(file));
        Ok(fd)
    }

    pub fn file_mut(&mut self, fd: u64) -> Result<&mut File, FsError> {
        self.fd_table
            .get_mut(fd as usize)
            .and_then(|slot| slot.as_mut())
            .ok_or(FsError::BadFd)
    }

    pub fn close_fd(&mut self, fd: u64) -> Result<(), FsError> {
        let slot = self
            .fd_table
            .get_mut(fd as usize)
            .ok_or(FsError::BadFd)?;

        if slot.take().is_none() {
            return Err(FsError::BadFd);
        }

        Ok(())
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Replace the working directory. Callers must pass a canonical absolute
    /// path that they have already checked is a directory.
    pub fn set_cwd(&mut self, cwd: Path) {
        self.cwd = cwd;
    }
}
