use alloc::alloc::{alloc_zeroed, dealloc};
use core::alloc::Layout;
use core::arch::global_asm;
use crate::println;

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
    Blocked,
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

        println!("Thread stack allocated at {:p} with size {}", raw, stack_size);

        let stack_top = (raw as u64 + stack_size as u64) & !0xF;
        let entry_addr = entry as usize as u64;

        // Fake "return address" slot the initial `ret` in switch_context
        // will pop into rip.
        let entry_slot = stack_top - 16;
        unsafe {
            *(entry_slot as *mut u64) = entry_addr;
        }

        // rsp points below 6 fake pushed registers (r15,r14,r13,r12,rbx,rbp)
        // that switch_context's pop sequence will consume, landing rsp at
        // entry_slot right before the ret.
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
    "ret",
);

extern "C" {
    pub fn switch_context(old: *mut ThreadContext, new: *const ThreadContext);
}