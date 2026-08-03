use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

use lazy_static::lazy_static;
use spin::Mutex;

use crate::process::thread::{self, TID, Thread, ThreadContext, ThreadState};
use crate::process::{PID, Process, ProcessType};

use crate::println;

pub struct ProcessManager {
    processes: Vec<Option<Process>>,
    threads: Vec<Option<Thread>>,
    runnables: VecDeque<TID>,

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
            current: None,
            kernel_context: ThreadContext::zeroed(),
        }
    }

    pub fn create_process(&mut self, parent: PID, name: String) -> PID {
        let new_pid = if let Some(free_index) = self.processes.iter().position(|p| p.is_none()) {
            let pid = PID(free_index as u64);
            self.processes[free_index] = Some(Process::new(pid, parent, name, ProcessType::User));
            pid
        } else {
            let pid = PID(self.processes.len() as u64);
            self.processes.push(Some(Process::new(pid, parent, name, ProcessType::User)));
            pid
        };

        new_pid
    }

    pub fn create_thread(&mut self, pid: PID, entry_point: extern "C" fn() -> !) -> TID {
        let new_tid = if let Some(free_index) = self.threads.iter().position(|p| p.is_none()) {
            let tid = TID(free_index as u64);
            self.threads[free_index] = Some(Thread::new(tid, pid, 4096 * 4, entry_point));
            tid
        } else {
            let tid = TID(self.threads.len() as u64);
            self.threads.push(Some(Thread::new(tid, pid, 4096 * 4, entry_point)));
            tid
        };

        self.processes[pid.0 as usize]
            .as_mut()
            .unwrap()
            .threads
            .push(new_tid);

        self.runnables.push_back(new_tid);

        new_tid
    }

    /// Prepares bookkeeping and returns the raw (old, new) context pointers
    /// needed to perform the actual switch. Must be called while holding
    /// the lock; the returned pointers remain valid because `threads` is
    /// stable storage (no reallocation happens between this call and the
    /// switch, since we're not calling create_thread concurrently).
    fn prepare_switch(&mut self, tid: TID) -> (*mut ThreadContext, *const ThreadContext) {
        let new_ctx: *const ThreadContext = {
            let t = self.threads[tid.0 as usize].as_mut().unwrap();
            t.set_state(ThreadState::Running);
            t.context_ptr()
        };

        let old_ctx: *mut ThreadContext = match self.current {
            Some(cur) => {
                let t = self.threads[cur.0 as usize].as_mut().unwrap();
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

}

/// Switch execution to `tid`. This is a free function (not a method taking
/// `&mut self`) so that the `PROCESS_MANAGER` lock is released BEFORE the
/// actual context switch happens — the switch doesn't return until some
/// other thread switches back, so holding the lock across it would
/// deadlock anyone else who needs to touch the process manager meanwhile.
pub fn switch_to(tid: TID) {
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