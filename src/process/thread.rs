// src/process/thread.rs

use alloc::alloc::{alloc_zeroed, dealloc};
use core::alloc::Layout;
use core::arch::global_asm;
use crate::{debug, println};

use crate::process::PID;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TID(pub u64);

impl TID {
    pub fn new(value: u64) -> Self {
        TID(value)
    }

    pub fn value(&self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadState {
    Ready,
    Running,
    Sleeping,
    Blocked,
    Waiting,
    Zombie,
    Exited,
}

#[repr(C)]
pub struct ThreadContext {
    rsp: u64,
}

impl ThreadContext {
    pub fn zeroed() -> Self {
        ThreadContext { rsp: 0 }
    }
}

pub struct Thread {
    tid: TID,
    owner: PID,
    state: ThreadState,

    stack_ptr: *mut u8,
    stack_layout: Layout,

    context: ThreadContext,
}

// SAFETY: `stack_ptr` is heap memory uniquely owned by this `Thread`.
// It's never aliased, and access only ever happens while holding the
// `Mutex<ProcessManager>` that owns the `Thread`.
unsafe impl Send for Thread {}

impl Thread {
    pub fn new(tid: TID, owner: PID, stack_size: usize, entry: extern "C" fn() -> !) -> Self {
        assert!(
            stack_size != 0 && stack_size % 16 == 0,
            "Stack size must be a non-zero multiple of 16"
        );

        let layout = Layout::from_size_align(stack_size, 16).unwrap();
        let raw = unsafe { alloc_zeroed(layout) };
        assert!(!raw.is_null(), "thread stack allocation failed");

        debug!("Thread stack allocated at {:p} with size {}", raw, stack_size);

        let stack_top = (raw as u64 + stack_size as u64) & !0xF;
        let entry_addr = entry as usize as u64;

        // Fake initial stack frame, laid out to match switch_context's
        // push/pop order in reverse (since switch_context pops back-to-front):
        //
        //   switch_context pushes:  rflags, rbp, rbx, r12, r13, r14, r15
        //   switch_context pops:    r15, r14, r13, r12, rbx, rbp, rflags
        //   then: ret  (pops the fake "return address" -> entry_addr)
        //
        // So from stack_top downward, memory must contain (high -> low):
        //   [stack_top - 8]  = entry_addr   (consumed by `ret`)
        //   [stack_top - 16] = rflags       (consumed by `popfq`)
        //   [stack_top - 24] = rbp
        //   [stack_top - 32] = rbx
        //   [stack_top - 40] = r12
        //   [stack_top - 48] = r13
        //   [stack_top - 56] = r14
        //   [stack_top - 64] = r15
        let entry_slot  = stack_top - 8;
        let rflags_slot = stack_top - 16;

        unsafe {
            *(entry_slot as *mut u64) = entry_addr;
            // IF (bit 9) set so the thread starts with interrupts enabled;
            // bit 1 is reserved and always set on real hardware.
            *(rflags_slot as *mut u64) = 0x202;
        }

        // rbp, rbx, r12, r13, r14, r15 slots (stack_top-24 .. stack_top-64)
        // are left as zero from alloc_zeroed — fine as initial dummy values.

        let rsp = stack_top - 64;

        Thread {
            tid,
            owner,
            state: ThreadState::Ready,
            stack_ptr: raw,
            stack_layout: layout,
            context: ThreadContext { rsp },
        }
    }

    pub fn get_tid(&self) -> TID {
        self.tid
    }

    pub fn get_owner(&self) -> PID {
        self.owner
    }

    pub fn get_state(&self) -> ThreadState {
        self.state
    }

    pub fn set_state(&mut self, new_state: ThreadState) {
        self.state = new_state;
    }

    pub fn context_mut(&mut self) -> *mut ThreadContext {
        &mut self.context as *mut _
    }

    pub fn context_ptr(&self) -> *const ThreadContext {
        &self.context as *const _
    }
}

impl Drop for Thread {
    fn drop(&mut self) {
        unsafe { dealloc(self.stack_ptr, self.stack_layout) };
    }
}

global_asm!(
    ".global switch_context",
    "switch_context:",
    "pushfq",
    "push rbp",
    "push rbx",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "mov [rdi], rsp",
    "mov rsp, [rsi]",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbx",
    "pop rbp",
    "popfq",
    "ret",
);

extern "C" {
    pub fn switch_context(old: *mut ThreadContext, new: *const ThreadContext);
}

//           +---------+
//           |  Ready  |
//           +----+----+
//                |
//                v
//           +---------+
//           | Running |
//           +----+----+
//                |
//     +----------+----------+----------+
//     |          |          |          |
//     v          v          v          v
// +---------+ +---------+ +---------+ +---------+
// | Blocked | | Sleeping| | Waiting | | Exited  |
// +----+----+ +----+----+ +----+----+ +----+----+
//      |           |           |               |
//      |           |           |               |
//      +-----------+-----------+               v
//                  |                      +---------+
//                  |                      | Zombie  |
//                  +----------------------+---------+

impl Thread {
    // Existing methods remain the same, but we add/update transitions.

    pub fn set_to_ready(&mut self) {
        assert!(
            matches!(self.state, ThreadState::Blocked | ThreadState::Sleeping | ThreadState::Waiting),
            "Thread can only be set to Ready from Blocked, Sleeping, or Waiting"
        );
        self.state = ThreadState::Ready;
    }

    /// Preemption path: Running -> Ready when a thread is requeued after
    /// using up its time slice. Distinct from `set_to_ready`, which is only
    /// for waking a thread that was Blocked/Sleeping/Waiting.
    pub fn preempt_to_ready(&mut self) {
        assert!(
            self.state == ThreadState::Running,
            "Thread can only be preempted to Ready from Running"
        );
        self.state = ThreadState::Ready;
    }

    pub fn set_to_running(&mut self) {
        assert!(
            self.state == ThreadState::Ready,
            "Thread can only be set to Running from Ready"
        );
        self.state = ThreadState::Running;
    }

    pub fn set_to_blocked(&mut self) {
        assert!(
            self.state == ThreadState::Running,
            "Thread can only be set to Blocked from Running"
        );
        self.state = ThreadState::Blocked;
    }

    pub fn set_to_sleeping(&mut self) {
        assert!(
            self.state == ThreadState::Running,
            "Thread can only be set to Sleeping from Running"
        );
        self.state = ThreadState::Sleeping;
    }

    pub fn set_to_waiting(&mut self) {
        assert!(
            self.state == ThreadState::Running,
            "Thread can only be set to Waiting from Running"
        );
        self.state = ThreadState::Waiting;
    }

    pub fn set_to_exited(&mut self) {
        assert!(
            self.state == ThreadState::Running,
            "Thread can only be set to Exited from Running"
        );
        self.state = ThreadState::Exited;
    }

    pub fn set_to_zombie(&mut self) {
        assert!(
            self.state == ThreadState::Exited,
            "Thread can only be set to Zombie from Exited"
        );
        self.state = ThreadState::Zombie;
    }

    // Optional: a method to reap a zombie (this would typically be called
    // by the parent process to clean up the thread descriptor).
    pub fn reap(&mut self) {
        assert!(
            self.state == ThreadState::Zombie,
            "Only Zombie threads can be reaped"
        );
        // The Drop impl will free the stack. We could additionally mark
        // the thread as "reaped" but we can just let it be dropped.
        // This method is a marker; actual cleanup happens on drop.
        // If you need to keep the Thread struct alive but mark it as
        // reaped, you'd need another state, but it's simpler to drop it.
    }
}