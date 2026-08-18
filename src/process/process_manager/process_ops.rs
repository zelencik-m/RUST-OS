// src/process/process_manager/process_ops.rs

use alloc::string::String;
use alloc::vec::Vec;
use x86_64::registers::control::Cr3;

use crate::memory::address_space::AddressSpace;
use crate::process::thread::TID;
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
                
                // If we are somehow destroying the currently active process, switch CR3 first
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