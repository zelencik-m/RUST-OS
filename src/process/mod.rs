pub mod process_manager;
pub mod thread;
pub mod idle;
pub mod elf_loader;
pub mod usermode;

use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::{File, FsError, Path};
use crate::fs::devices::{stdin_file, stdout_file, stderr_file};
use crate::process::thread::TID;
use crate::memory::AddressSpace;

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
