use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use alloc::boxed::Box;

use lazy_static::lazy_static;
use spin::Mutex;

use crate::process::thread::{self, TID, Thread, ThreadContext, ThreadState};
use crate::process::{PID, Process, ProcessType};
use crate::scheduler::scheduler::Scheduler;  
use crate::{println, scheduler};


use crate::scheduler::scheduler::SCHEDULER;

pub struct ProcessManager {
    processes: Vec<Option<Process>>,
    threads: Vec<Option<Box<Thread>>>, 
    // runnables: VecDeque<TID>,

    free_pids: Vec<PID>,
    free_tids: Vec<TID>,

    // current: Option<TID>,
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
            free_pids: Vec::new(),
            free_tids: Vec::new(),
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
        SCHEDULER.lock().add_runnable(tid);
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

        SCHEDULER.lock().remove_from_runnables(tid);
    }

    /// Prepares bookkeeping and returns the raw (old, new) context pointers
    /// needed to perform the actual switch. Must be called while holding
    /// the lock; the returned pointers remain valid because `threads` is
    /// stable storage (no reallocation happens between this call and the
    /// switch, since we're not calling create_thread concurrently).
    pub fn prepare_switch(&mut self, scheduler: &mut Scheduler, tid: TID)
            -> (*mut ThreadContext, *const ThreadContext)
    {
        if Some(tid) == scheduler.current {
            let t = self.thread(tid);
            let ctx = t.context_ptr() as *mut ThreadContext;
            return (ctx, ctx);
        }

        let new_ctx: *const ThreadContext = {
            let t = self.thread_mut(tid);
            t.set_state(ThreadState::Running);
            t.context_ptr()
        };

        let old_ctx: *mut ThreadContext = match scheduler.current {
            Some(cur) => {
                let t = self.thread_mut(cur);
                if t.get_state() == ThreadState::Running {
                    t.set_state(ThreadState::Ready);
                }
                t.context_mut()
            }
            None => &mut self.kernel_context as *mut _,
        };

        scheduler.current = Some(tid);
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
    let mut scheduler = SCHEDULER.lock();
    let mut pm = PROCESS_MANAGER.lock();
    let (old_ctx, new_ctx) = pm.prepare_switch(&mut *scheduler, tid);
    drop(scheduler);
    drop(pm);
    unsafe {
        thread::switch_context(old_ctx, new_ctx);
    }
}


impl ProcessManager {

    pub fn run_thread(&mut self, tid: TID) {
        let thread = self.thread_mut(tid);
        thread.set_to_running(); // asserts state == Ready
    }

    /// Transition a thread from Running → Blocked and remove it from the run queue.
    pub fn block_thread(&mut self, tid: TID) {
        let thread = self.thread_mut(tid);
        thread.set_to_blocked(); // asserts state == Running
    }

    /// Transition a thread from Running → Sleeping and remove it from the run queue.
    pub fn sleep_thread(&mut self, tid: TID) {
        let thread = self.thread_mut(tid);
        thread.set_to_sleeping();
    }

    /// Transition a thread from Running → Waiting and remove it from the run queue.
    pub fn wait_thread(&mut self, tid: TID) {
        let thread = self.thread_mut(tid);
        thread.set_to_waiting();
    }

    // --- Wake (Blocked/Sleeping/Waiting → Ready) ---

    /// Transition a thread from Blocked/Sleeping/Waiting → Ready and add it to the run queue.
    pub fn wake_thread(&mut self, tid: TID) {
        let thread = self.thread_mut(tid);
        thread.set_to_ready(); // asserts state is Blocked, Sleeping, or Waiting
    }

    // --- Exit (Running → Exited) ---

    /// Transition a thread from Running → Exited and remove it from the run queue.
    pub fn exit_thread(&mut self, tid: TID) {
        let thread = self.thread_mut(tid);
        thread.set_to_exited(); // asserts state == Running
    }

    // --- Zombie (Exited → Zombie) ---

    /// Transition a thread from Exited → Zombie (no scheduler change).
    pub fn set_zombie(&mut self, tid: TID) {
        let thread = self.thread_mut(tid);
        thread.set_to_zombie(); // asserts state == Exited
    }

    pub fn get_thread_state(&self, tid: TID) -> ThreadState {
        self.thread(tid).get_state()
    }

    pub fn set_thread_ready(&mut self, tid: TID) {
        let thread = self.thread_mut(tid);
        thread.set_to_ready(); // asserts state is Blocked, Sleeping, or Waiting
    }

    pub fn preempt_thread(&mut self, tid: TID) {
        let thread = self.thread_mut(tid);
        thread.preempt_to_ready(); // asserts state == Running
    }
}