use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use alloc::boxed::Box;

use lazy_static::lazy_static;
use spin::Mutex;

use crate::process::thread::{self, TID, Thread, ThreadContext, ThreadState};
use crate::process::{PID, Process, ProcessType};

use crate::println;

pub struct ProcessManager {
    processes: Vec<Option<Process>>,
    threads: Vec<Option<Box<Thread>>>, 
    runnables: VecDeque<TID>,

    free_pids: Vec<PID>,
    free_tids: Vec<TID>,

    current: Option<TID>,
    kernel_context: ThreadContext,
}

lazy_static! {
    pub static ref PROCESS_MANAGER: Mutex<ProcessManager> = Mutex::new(ProcessManager::new());
}

impl ProcessManager {
    pub fn new() -> Self {
        ProcessManager {
            processes: Vec::new(),
            threads: Vec::new(),
            runnables: VecDeque::new(),
            free_pids: Vec::new(),
            free_tids: Vec::new(),
            current: None,
            kernel_context: ThreadContext::zeroed(),
        }
    }

    pub fn create_process(
        &mut self,
        parent: PID,
        name: String,
        process_type: ProcessType,
    ) -> PID {
        let pid = if let Some(free_pid) = self.free_pids.pop() {
            // Reuse a previously freed PID
            self.processes[free_pid.0 as usize] =
                Some(Process::new(free_pid, parent, name, process_type));
            free_pid
        } else {
            // No free slots – grow the vector
            let pid = PID(self.processes.len() as u64);
            self.processes.push(
                Some(Process::new(pid, parent, name, process_type)),
            );
            pid
        };
        pid
    }

    pub fn create_thread(&mut self, pid: PID, entry_point: extern "C" fn() -> !) -> TID {
        let tid = if let Some(free_tid) = self.free_tids.pop() {
            self.threads[free_tid.0 as usize] =
                Some(Box::new(Thread::new(free_tid, pid, 4096 * 4, entry_point)));
            free_tid
        } else {
            let tid = TID(self.threads.len() as u64);
            self.threads.push(
                Some(Box::new(Thread::new(tid, pid, 4096 * 4, entry_point))),
            );
            tid
        };

        // Use the new accessor to attach the TID to the process.
        self.process_mut(pid).threads.push(tid);
        self.runnables.push_back(tid);
        tid
    }

    /// Idempotent: ensures PID(0) exists and represents the kernel.
    pub fn init_kernel_process(&mut self) -> PID {
        if self.processes.first().map(|p| p.is_some()).unwrap_or(false) {
            return PID(0);
        }
        // PID(0) is the kernel; its parent is itself (or you could use None).
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

    /// Convenience: create a user-mode process as a child of PID(0).
    pub fn create_user_process(&mut self, name: String) -> PID {
        self.create_process(PID(0), name, ProcessType::User)
    }

    pub fn destroy_process(&mut self, pid: PID) {
        // First destroy all threads belonging to this process.
        let tids: Vec<TID> = self.process(pid).threads.clone();
        for tid in tids {
            self.remove_thread(tid);
        }
        // Now clear the process slot and recycle the PID.
        self.processes[pid.0 as usize] = None;
        self.free_pids.push(pid);
    }

    pub fn remove_thread(&mut self, tid: TID) {
        // Detach from process
        let pid = self.thread(tid).get_owner();
        self.process_mut(pid).threads.retain(|&x| x != tid);

        // Clear slot and recycle TID; the old Box is dropped here, freeing memory
        self.threads[tid.0 as usize] = None;
        self.free_tids.push(tid);
    }

    fn next_runnable(&mut self) -> Option<TID> {
        while let Some(tid) = self.runnables.pop_front() {
            if self.threads[tid.0 as usize].is_some() {
                return Some(tid);
            }
        }
        None
    }

    /// Prepares bookkeeping and returns the raw (old, new) context pointers
    /// needed to perform the actual switch. Must be called while holding
    /// the lock; the returned pointers remain valid because `threads` is
    /// stable storage (no reallocation happens between this call and the
    /// switch, since we're not calling create_thread concurrently).
    fn prepare_switch(&mut self, tid: TID) -> (*mut ThreadContext, *const ThreadContext) {
        if Some(tid) == self.current {
            let t = self.thread(tid);
            let ctx = t.context_ptr() as *mut ThreadContext;
            return (ctx, ctx);
        }

        let new_ctx: *const ThreadContext = {
            let t = self.thread_mut(tid);
            t.set_state(ThreadState::Running);
            t.context_ptr()
        };

        let old_ctx: *mut ThreadContext = match self.current {
            Some(cur) => {
                let t = self.thread_mut(cur);
                if t.get_state() == ThreadState::Running {
                    t.set_state(ThreadState::Ready);
                }
                t.context_mut()
            }
            None => &mut self.kernel_context as *mut _,
        };

        self.current = Some(tid);
        (old_ctx, new_ctx)
    }

    pub fn thread(&self, tid: TID) -> &Thread {
        self.threads
            .get(tid.0 as usize)
            .and_then(Option::as_ref)
            .map(|b| &**b)
            .expect("thread(): invalid TID")
    }

    pub fn thread_mut(&mut self, tid: TID) -> &mut Thread {
        self.threads
            .get_mut(tid.0 as usize)
            .and_then(Option::as_mut)
            .map(|b| &mut **b)
            .expect("thread_mut(): invalid TID")
    }

    /// Immutable access to a process.
    pub fn process(&self, pid: PID) -> &Process {
        self.processes
            .get(pid.0 as usize)
            .and_then(Option::as_ref)
            .expect("process(): invalid or empty process slot")
    }

    /// Mutable access to a process.
    pub fn process_mut(&mut self, pid: PID) -> &mut Process {
        self.processes
            .get_mut(pid.0 as usize)
            .and_then(Option::as_mut)
            .expect("process_mut(): invalid or empty process slot")
    }

}

/// Switch execution to `tid`. This is a free function (not a method taking
/// `&mut self`) so that the `PROCESS_MANAGER` lock is released BEFORE the
/// actual context switch happens — the switch doesn't return until some
/// other thread switches back, so holding the lock across it would
/// deadlock anyone else who needs to touch the process manager meanwhile.
pub fn switch_to(tid: TID) {
    
    // unsafe { core::arch::asm!("cli", options(nostack, preserves_flags)); }

    let (old_ctx, new_ctx) = {
        let mut pm = PROCESS_MANAGER.lock();
        pm.prepare_switch(tid)
        // `pm` (the MutexGuard) is dropped HERE, at the end of this block,
        // BEFORE we touch the actual asm switch below.
    };

    unsafe {
        thread::switch_context(old_ctx, new_ctx);
    }
}