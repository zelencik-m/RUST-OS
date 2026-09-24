use alloc::alloc::{alloc_zeroed, dealloc};
use core::alloc::Layout;
use core::arch::global_asm;
use crate::{debug, println};
use x86_64::VirtAddr;
use crate::process::usermode::user_thread_trampoline;

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
    pub user_stack_top: Option<VirtAddr>,
    pub is_user: bool,
    pub kernel_stack_top: VirtAddr,
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

        // switch_context pops these slots in reverse order, then returns to
        // entry_addr. A new thread has no interrupt frame; schedule() must run
        // on the interrupted thread's kernel stack through a plain gate (no IST).
        // From stack_top downward:
        // [stack_top - 8]  = entry_addr
        // [stack_top - 16] = rflags
        // [stack_top - 24] = rbp
        // [stack_top - 32] = rbx
        // [stack_top - 40] = r12
        // [stack_top - 48] = r13
        // [stack_top - 56] = r14
        // [stack_top - 64] = r15
        let entry_slot  = stack_top - 8;
        let rflags_slot = stack_top - 16;

        unsafe {
            *(entry_slot as *mut u64) = entry_addr;
            *(rflags_slot as *mut u64) = 0x202;
        }

        let rsp = stack_top - 64;

        Thread {
            tid, owner, state: ThreadState::Ready,
            stack_ptr: raw, stack_layout: layout,
            context: ThreadContext { rsp },
            user_stack_top: None,
            is_user: false,
            kernel_stack_top: VirtAddr::new(stack_top), // Added
        }
    }

    pub fn new_user(tid: TID, owner: PID, stack_size: usize, entry_point: VirtAddr, user_stack_top: VirtAddr) -> Self {
        assert!(stack_size != 0 && stack_size % 16 == 0, "Stack size must be a non-zero multiple of 16");

        let layout = Layout::from_size_align(stack_size, 16).unwrap();
        let raw = unsafe { alloc_zeroed(layout) };
        assert!(!raw.is_null(), "thread stack allocation failed");

        let stack_top = (raw as u64 + stack_size as u64) & !0xF;

        // From stack_top downward; switch_context returns to the trampoline,
        // which pops entry_point and user_stack_top into rdi and rsi.
        // [stack_top - 8]  = user_stack_top
        // [stack_top - 16] = entry_point
        // [stack_top - 24] = user_thread_trampoline
        // [stack_top - 32] = rflags
        // [stack_top - 40] = rbp
        // [stack_top - 48] = rbx
        // [stack_top - 56] = r12
        // [stack_top - 64] = r13
        // [stack_top - 72] = r14
        // [stack_top - 80] = r15
        
        let rsp = stack_top - 80;

        unsafe {
            *((stack_top - 32) as *mut u64) = 0x202;
            *((stack_top - 24) as *mut u64) = user_thread_trampoline as u64;
            *((stack_top - 16) as *mut u64) = entry_point.as_u64();
            *((stack_top - 8) as *mut u64) = user_stack_top.as_u64();
        }

        Thread {
            tid, owner, state: ThreadState::Ready,
            stack_ptr: raw, stack_layout: layout,
            context: ThreadContext { rsp },
            user_stack_top: Some(user_stack_top),
            is_user: true,
            kernel_stack_top: VirtAddr::new(stack_top),
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
    pub fn set_to_ready(&mut self) {
        assert!(
            matches!(self.state, ThreadState::Blocked | ThreadState::Sleeping | ThreadState::Waiting),
            "Thread can only be set to Ready from Blocked, Sleeping, or Waiting, current state: {:?}",
            self.state
        );
        self.state = ThreadState::Ready;
    }

    pub fn preempt_to_ready(&mut self) {
        assert!(
            self.state == ThreadState::Running,
            "Thread can only be preempted to Ready from Running, current state: {:?}",
            self.state
        );
        self.state = ThreadState::Ready;
    }

    pub fn set_to_running(&mut self) {
        assert!(
            self.state == ThreadState::Ready,
            "Thread can only be set to Running from Ready, current state: {:?}",
            self.state
        );
        self.state = ThreadState::Running;
    }

    pub fn set_to_blocked(&mut self) {
        assert!(
            self.state == ThreadState::Running,
            "Thread can only be set to Blocked from Running, current state: {:?}",
            self.state
        );
        self.state = ThreadState::Blocked;
    }

    pub fn set_to_sleeping(&mut self) {
        assert!(
            self.state == ThreadState::Running,
            "Thread can only be set to Sleeping from Running, current state: {:?}",
            self.state
        );
        self.state = ThreadState::Sleeping;
    }

    pub fn set_to_waiting(&mut self) {
        assert!(
            self.state == ThreadState::Running,
            "Thread can only be set to Waiting from Running, current state: {:?}",
            self.state
        );
        self.state = ThreadState::Waiting;
    }

    pub fn set_to_exited(&mut self) {
        assert!(
            self.state == ThreadState::Running,
            "Thread can only be set to Exited from Running, current state: {:?}",
            self.state
        );
        self.state = ThreadState::Exited;
    }

    pub fn set_to_zombie(&mut self) {
        assert!(
            self.state == ThreadState::Exited,
            "Thread can only be set to Zombie from Exited, current state: {:?}",
            self.state
        );
        self.state = ThreadState::Zombie;
    }

    pub fn reap(&mut self) {
        assert!(
            self.state == ThreadState::Zombie,
            "Only Zombie threads can be reaped, current state: {:?}",
            self.state
        );
    }
}
