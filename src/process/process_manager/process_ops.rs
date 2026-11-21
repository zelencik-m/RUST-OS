use alloc::string::String;
use alloc::vec::Vec;
use x86_64::registers::control::Cr3;

use crate::memory::address_space::AddressSpace;
use crate::process::thread::{ThreadState, TID};
use crate::process::{PID, Process, ProcessType};

use super::manager::ProcessManager;

impl ProcessManager {
    pub fn create_process(
        &mut self,
        parent: PID,
        name: String,
        process_type: ProcessType,
    ) -> PID {
        let pid = if let Some(free_pid) = self.free_pids.pop() {
            self.processes[free_pid.0 as usize] =
                Some(Process::new(free_pid, parent, name, process_type));
            free_pid
        } else {
            let pid = PID(self.processes.len() as u64);
            self.processes
                .push(Some(Process::new(pid, parent, name, process_type)));
            pid
        };

        self.processes[parent.0 as usize]
            .as_mut()
            .expect("parent process should exist")
            .add_child(pid);

        pid
    }

    /// Idempotent: ensures PID(0) exists and represents the kernel.
    pub fn init_kernel_process(&mut self) -> PID {
        if self.processes.first().map(|p| p.is_some()).unwrap_or(false) {
            return PID(0);
        }
        self.processes.push(Some(Process::new(
            PID(0),
            PID(0),
            String::from("kernel"),
            ProcessType::Kernel,
        )));
        PID(0)
    }

    /// Convenience: create a kernel-mode process as a child of PID(0).
    pub fn create_kernel_process(&mut self, name: String) -> PID {
        self.create_process(PID(0), name, ProcessType::Kernel)
    }

    pub fn create_user_process_with_space(
        &mut self,
        parent: PID,
        name: String,
        address_space: AddressSpace,
    ) -> PID {
        let pid = if let Some(free_pid) = self.free_pids.pop() {
            self.processes[free_pid.0 as usize] =
                Some(Process::new_user(free_pid, parent, name, address_space));
            free_pid
        } else {
            let pid = PID(self.processes.len() as u64);
            self.processes
                .push(Some(Process::new_user(pid, parent, name, address_space)));
            pid
        };
        self.processes[parent.0 as usize]
            .as_mut()
            .expect("parent exists")
            .add_child(pid);
        pid
    }

    /// Whether `pid` is a user process that hasn't exited, i.e. one that
    /// may still `wait4` for its children.
    pub fn is_live_user_process(&self, pid: PID) -> bool {
        self.try_process(pid)
            .map(|process| process.is_user() && process.exit_code.is_none())
            .unwrap_or(false)
    }

    /// Whether a thread of `pid` is still alive (not `Exited`/`Zombie`).
    pub fn has_live_threads(&self, pid: PID) -> bool {
        self.process(pid).threads.iter().any(|&tid| {
            !matches!(
                self.thread(tid).get_state(),
                ThreadState::Exited | ThreadState::Zombie
            )
        })
    }

    /// Hand every child of `pid` (which is exiting) to the kernel, PID 0.
    ///
    /// The kernel never waits, so children that are already zombies and have
    /// no threads left are freed now. A zombie whose thread hasn't been reaped
    /// yet is freed by the reaper instead (`finalize_process_if_empty`), now
    /// that its parent is PID 0. Live children are freed when they exit.
    pub fn reparent_children_to_kernel(&mut self, pid: PID) {
        let children = self.process_mut(pid).take_children();
        for child in children {
            let Some(process) = self.try_process_mut(child) else {
                continue;
            };
            process.set_parent(PID(0));
            let unwanted_zombie = process.exit_code.is_some() && process.threads.is_empty();
            if unwanted_zombie {
                self.destroy_process(child);
            } else {
                self.process_mut(PID(0)).add_child(child);
            }
        }
    }

    /// Unlink `child` from its parent's `children` list (if the parent is
    /// still there). Call this before freeing a process.
    pub fn unlink_from_parent(&mut self, child: PID) {
        let parent = self.try_process(child).and_then(|process| process.parent());
        if let Some(parent) = parent {
            if let Some(parent) = self.try_process_mut(parent) {
                parent.remove_child(child);
            }
        }
    }

    pub fn destroy_process(&mut self, pid: PID) {
        let tids: Vec<TID> = self.process(pid).threads.clone();
        for tid in tids {
            self.remove_thread_locked(tid);
        }
        
        if let Some(mut space) = self.process_mut(pid).address_space.take() {
            let mut mm_guard = crate::memory::memory_manager::MEMORY_MANAGER.lock();
            if let Some(mm) = mm_guard.as_mut() {
                let (_, flags) = Cr3::read();
                let current_cr3 = Cr3::read().0;
                
                // Switch away before freeing the active page tables.
                if current_cr3 == space.cr3() {
                    unsafe { Cr3::write(mm.kernel_p4_frame, flags); }
                }
                mm.destroy_address_space(&mut space);
            }
        }
        
        self.processes[pid.0 as usize] = None;
        self.free_pids.push(pid);
    }
}
