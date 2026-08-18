// src/process/process_manager/thread_ops.rs

use alloc::boxed::Box;
use x86_64::registers::control::Cr3;

use crate::process::thread::{TID, Thread, ThreadState};
use crate::process::PID;
use crate::memory::memory_manager::MEMORY_MANAGER;
use super::manager::ProcessManager;

impl ProcessManager {
    pub fn create_thread_locked(&mut self, pid: PID, entry_point: extern "C" fn() -> !) -> TID {
        let tid = if let Some(free_tid) = self.free_tids.pop() {
            self.threads[free_tid.0 as usize] = Some(Box::new(Thread::new(free_tid, pid, 4096 * 4, entry_point)));
            free_tid
        } else {
            let tid = TID(self.threads.len() as u64);
            self.threads.push(Some(Box::new(Thread::new(tid, pid, 4096 * 4, entry_point))));
            tid
        };

        // MILESTONE 4.1: Automatically allocate a user stack if this is a user process
        let proc = self.process_mut(pid);
        if let Some(space) = proc.address_space.as_mut() {
            let mut mm_guard = MEMORY_MANAGER.lock();
            if let Some(mm) = mm_guard.as_mut() {
                if let Ok(stack_top) = mm.allocate_user_stack(space) {
                    if let Some(t_opt) = self.threads[tid.0 as usize].as_mut() {
                        t_opt.user_stack_top = Some(stack_top);
                    }
                }
            }
        }

        self.process_mut(pid).threads.push(tid);
        tid
    }

    pub fn create_idle_thread_locked(&mut self, entry: extern "C" fn() -> !) -> TID {
        let kernel_pid = self.init_kernel_process();

        let tid = if let Some(free_tid) = self.free_tids.pop() {
            free_tid
        } else {
            TID(self.threads.len() as u64)
        };

        let thread = Box::new(Thread::new(tid, kernel_pid, 4096 * 4, entry));

        if (tid.0 as usize) < self.threads.len() {
            self.threads[tid.0 as usize] = Some(thread);
        } else {
            self.threads.push(Some(thread));
        }

        self.process_mut(kernel_pid).threads.push(tid);
        tid
    }

    /// Remove a thread and recycle its TID.
    ///
    /// Callers MUST guarantee this thread is not the one currently executing —
    /// see the free function `remove_thread`.
    pub fn remove_thread_locked(&mut self, tid: TID) {
        let pid = self.thread(tid).get_owner();
        self.process_mut(pid).threads.retain(|&x| x != tid);

        self.threads[tid.0 as usize] = None;
        self.free_tids.push(tid);
    }

    // --- state transitions ---

    pub fn block_thread(&mut self, tid: TID) {
        self.thread_mut(tid).set_to_blocked();
    }

    pub fn sleep_thread(&mut self, tid: TID) {
        self.thread_mut(tid).set_to_sleeping();
    }

    pub fn wait_thread(&mut self, tid: TID) {
        self.thread_mut(tid).set_to_waiting();
    }

    /// Blocked/Sleeping/Waiting → Ready.
    pub fn wake_thread(&mut self, tid: TID) {
        self.thread_mut(tid).set_to_ready();
    }

    pub fn exit_thread(&mut self, tid: TID) {
        self.thread_mut(tid).set_to_exited();
    }

    pub fn set_zombie(&mut self, tid: TID) {
        self.thread_mut(tid).set_to_zombie();
    }

    pub fn preempt_thread(&mut self, tid: TID) {
        self.thread_mut(tid).preempt_to_ready();
    }

    pub fn get_thread_state(&self, tid: TID) -> ThreadState {
        self.thread(tid).get_state()
    }

    pub fn kill_current_thread(&mut self, tid: TID) {
        self.exit_thread(tid); // Running -> Exited
        self.set_zombie(tid);  // Exited  -> Zombie
        
        let owner = self.thread(tid).get_owner();
        
        // Check if any OTHER thread in this process is still alive
        let has_living_threads = self.process(owner).threads.iter().any(|&t| {
            let state = self.thread(t).get_state();
            t != tid && state != ThreadState::Exited && state != ThreadState::Zombie
        });
        
        if !has_living_threads {
            if let Some(mut space) = self.process_mut(owner).address_space.take() {
                let mut mm_guard = crate::memory::memory_manager::MEMORY_MANAGER.lock();
                if let Some(mm) = mm_guard.as_mut() {
                    // CRITICAL: Switch to kernel CR3 before freeing the user page tables!
                    let (_, flags) = Cr3::read();
                    unsafe { Cr3::write(mm.kernel_p4_frame, flags); }
                    
                    mm.destroy_address_space(&mut space);
                }
            }
        }
    }
}