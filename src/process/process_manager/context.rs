use crate::process::thread::{TID, ThreadContext, ThreadState};
use crate::scheduler::scheduler::Scheduler;

use super::manager::ProcessManager;

impl ProcessManager {
    /// Compute the old/new context pointers and update scheduler bookkeeping.
    pub fn commit_switch(
        &mut self,
        scheduler: &mut Scheduler,
        tid: TID,
    ) -> (*mut ThreadContext, *const ThreadContext) {
        let old_ctx: *mut ThreadContext = match scheduler.get_current() {
            Some(cur) => {
                let t = self.thread_mut(cur);
                if t.get_state() == ThreadState::Running {
                    t.set_state(ThreadState::Ready);
                }
                t.context_mut()
            }
            None => &mut self.kernel_context as *mut _,
        };

        // The scheduler can select the currently running thread again. Marking
        // the selected thread Running before handling the old thread would
        // then immediately change that same thread back to Ready.
        let new_ctx: *const ThreadContext = {
            let t = self.thread_mut(tid);
            t.set_state(ThreadState::Running);
            t.context_ptr()
        };

        scheduler.set_current(tid);
        (old_ctx, new_ctx)
    }
}
