use alloc::boxed::Box;
use x86_64::registers::control::Cr3;
use x86_64::VirtAddr;
use alloc::collections::VecDeque;
use crate::process::thread::{TID, Thread, ThreadState};
use crate::process::PID;
use crate::memory::memory_manager::{MEMORY_MANAGER, Oom};
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

    pub fn create_user_thread_locked(&mut self, pid: PID, entry_point: VirtAddr) -> Result<TID, Oom> {
        // Allocate the user stack before taking a TID, so a failure does not
        // leak a recycled TID slot.
        let user_stack = {
            let space = self
                .process_mut(pid)
                .address_space
                .as_mut()
                .expect("create_user_thread_locked: process has no address space");
            let mut mm_guard = MEMORY_MANAGER.lock();
            let mm = mm_guard.as_mut().ok_or(Oom)?;
            mm.allocate_user_stack(space)?
        };

        let tid = if let Some(free_tid) = self.free_tids.pop() {
            free_tid
        } else {
            TID(self.threads.len() as u64)
        };

        let thread = Box::new(Thread::new_user(tid, pid, 4096 * 4, entry_point, user_stack));

        if (tid.0 as usize) < self.threads.len() {
            self.threads[tid.0 as usize] = Some(thread);
        } else {
            self.threads.push(Some(thread));
        }

        self.process_mut(pid).threads.push(tid);
        Ok(tid)
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

    pub fn enqueue_for_reap(&mut self, tid: TID) {
        if !self.reap_queue.contains(&tid) {
            self.reap_queue.push_back(tid);
        }
    }

    pub fn thread_exists(&self, tid: TID) -> bool {
        self.threads
            .get(tid.0 as usize)
            .map(|slot| slot.is_some())
            .unwrap_or(false)
    }

    /// Free every queued zombie thread **except `current`** — the thread
    /// whose kernel stack we are executing on right now.
    ///
    /// Must be called while holding `PROCESS_MANAGER` (and with interrupts
    /// disabled, which `IrqSafeMutex` already guarantees), because dropping
    /// a `Thread` deallocates its kernel stack through the global allocator.
    pub fn reap_dead_threads(&mut self, current: Option<TID>) {
        let mut deferred = VecDeque::new();
        while let Some(tid) = self.reap_queue.pop_front() {
            // Can't free the stack we're running on; try again next switch.
            if Some(tid) == current {
                deferred.push_back(tid);
                continue;
            }
            // The thread may already have been removed by another path.
            if !self.thread_exists(tid) {
                continue;
            }
            let owner = self.thread(tid).get_owner();
            self.remove_thread_locked(tid);
            self.finalize_process_if_empty(owner);
        }
        self.reap_queue = deferred;
    }

    /// If `pid` has no threads left, drop the `Process` and recycle the PID.
    /// The address space was normally already destroyed by `kill_current_thread`
    /// when the last thread died; we still tear it down defensively.
    fn finalize_process_if_empty(&mut self, pid: PID) {
        if pid == PID(0) {
            return; // never destroy the kernel process
        }
        let should_finalize = self
            .processes
            .get(pid.0 as usize)
            .map(|slot| slot.as_ref().map(|p| p.threads.is_empty()).unwrap_or(false))
            .unwrap_or(false);
        if !should_finalize {
            return;
        }
        // Release any address space left behind during thread teardown.
        if let Some(mut space) = self.process_mut(pid).address_space.take() {
            let mut mm_guard = crate::memory::memory_manager::MEMORY_MANAGER.lock();
            if let Some(mm) = mm_guard.as_mut() {
                let (current_cr3, flags) = Cr3::read();
                if current_cr3 == space.cr3() {
                    unsafe { Cr3::write(mm.kernel_p4_frame, flags); }
                }
                mm.destroy_address_space(&mut space);
            }
        }
        self.processes[pid.0 as usize] = None;
        self.free_pids.push(pid);
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
        self.exit_thread(tid);
        self.set_zombie(tid);
        
        let owner = self.thread(tid).get_owner();
        
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
