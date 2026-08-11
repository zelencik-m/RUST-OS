// src/process/process_manager.rs

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use alloc::boxed::Box;

use lazy_static::lazy_static;
use spin::Mutex;

use crate::process::thread::{self, TID, Thread, ThreadContext, ThreadState};
use crate::process::{PID, Process, ProcessType};
use crate::scheduler::scheduler::Scheduler;
use crate::{println, scheduler, debugln};
use crate::memory::address_space::{AddressSpace, map_user_region};
use crate::memory::vma::{VmArea, VmaKind};
use crate::memory::memory_manager::MEMORY_MANAGER;
use crate::scheduler::scheduler::SCHEDULER;
use x86_64::structures::paging::{PageTableFlags, Size4KiB};
use x86_64::VirtAddr;

use x86_64::structures::paging::PhysFrame;
use x86_64::structures::paging::FrameAllocator;
use x86_64::structures::paging::Mapper;
use x86_64::registers::control::Cr3;
use x86_64::instructions::interrupts;

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

        self.processes[parent.0 as usize]
            .as_mut()
            .expect("parent process should exist")
            .add_child(pid);

        pid
    }

    pub fn create_thread_locked(&mut self, pid: PID, entry_point: extern "C" fn() -> !) -> TID {
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

        self.process_mut(pid).threads.push(tid);
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
        let tids: Vec<TID> = self.process(pid).threads.clone();
        for tid in tids {
            self.remove_thread_locked(tid);
        }
        self.processes[pid.0 as usize] = None;
        self.free_pids.push(pid);
    }

    pub fn remove_thread_locked(&mut self, tid: TID) {
        let pid = self.thread(tid).get_owner();
        self.process_mut(pid).threads.retain(|&x| x != tid);

        // Clear slot and recycle TID; the old Box is dropped here, freeing
        // the thread's stack. Callers MUST guarantee this thread is not the
        // one currently executing — see the free function `remove_thread`.
        self.threads[tid.0 as usize] = None;
        self.free_tids.push(tid);
    }

    pub fn commit_switch(&mut self, scheduler: &mut Scheduler, tid: TID)
            -> (*mut ThreadContext, *const ThreadContext)
    {
        // if Some(tid) == scheduler.current {
        //     let t = self.thread(tid);
        //     let ctx = t.context_ptr() as *mut ThreadContext;
        //     return (ctx, ctx);
        // }

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
            self.processes.push(
                Some(Process::new_user(pid, parent, name, address_space)),
            );
            pid
        };
        self.processes[parent.0 as usize]
            .as_mut().expect("parent exists")
            .add_child(pid);
        pid
    }

    /// Map a user region into a process and record the VMA.
    pub fn map_user_region_for(
        &mut self,
        pid: PID,
        fa: &mut impl FrameAllocator<Size4KiB>,
        off: VirtAddr,
        start: VirtAddr,
        size: usize,
        leaf_flags: PageTableFlags,
        kind: VmaKind,
    ) -> Vec<PhysFrame<Size4KiB>> {
        let process = self.process_mut(pid);
        let space = process.address_space.as_mut()
            .expect("map_user_region_for: kernel process has no address space");

        let frames = map_user_region(space, fa, off, start, size, leaf_flags);

        // Borrow of `space` ends here; now safe to mutate `process.vmas`.
        process.vmas.push(VmArea { start, size: size as u64, flags: leaf_flags, kind });
        frames
    }

}


pub fn create_thread(pid: PID, entry_point: extern "C" fn() -> !) -> TID {
    interrupts::without_interrupts(|| {
        let mut scheduler = SCHEDULER.lock();
        let mut pm = PROCESS_MANAGER.lock();
        let tid = pm.create_thread_locked(pid, entry_point);
        drop(pm);
        scheduler.add_runnable(tid);
        tid
    })
}

pub fn remove_thread(tid: TID) {
    let mut scheduler = SCHEDULER.lock();
    assert!(
        scheduler.current != Some(tid),
        "remove_thread: refusing to free the stack of the currently running \
         thread (TID {:?}) — switch away first, then reap from a different \
         thread's stack (e.g. the parent, during wait())",
        tid
    );
    scheduler.remove_from_runnables(tid);
    drop(scheduler);

    PROCESS_MANAGER.lock().remove_thread_locked(tid);
}

pub fn create_idle_thread(entry: extern "C" fn() -> !) -> TID {
    interrupts::without_interrupts(|| {
        let mut scheduler = SCHEDULER.lock();
        let mut pm = PROCESS_MANAGER.lock();
        let tid = pm.create_idle_thread_locked(entry);
        drop(pm);
        scheduler.set_idle_thread(tid);
        tid
    })
}

/// Switch execution to `tid`. This is a free function (not a method taking
/// `&mut self`) so that the `PROCESS_MANAGER` lock is released BEFORE the
/// actual context switch happens — the switch doesn't return until some
/// other thread switches back, so holding the lock across it would
/// deadlock anyone else who needs to touch the process manager meanwhile.
///
/// Locks are taken `Scheduler -> ProcessManager`, matching every other
/// free function in this module (fix 2).
pub fn switch_to(tid: TID) {
     interrupts::without_interrupts(|| {
        let mut scheduler = SCHEDULER.lock();
        let mut pm = PROCESS_MANAGER.lock();

        let new_owner = pm.thread(tid).get_owner();

        debugln!("Thread: {:?}, Process:{:?}",tid, new_owner );

        // Check if we are switching to a thread of a different process.
        let changing = scheduler
            .current
            .map(|c| pm.thread(c).get_owner() != new_owner)
            .unwrap_or(true); // first ever switch → treat as change

        if changing {
            debugln!("Changing");
            if let Some(space) = &pm.process(new_owner).address_space {
                let (_, flags) = Cr3::read();
                unsafe { Cr3::write(space.cr3(), flags); }
                debugln!("[CR3 switch] to PID {}", new_owner.0);
            }
        }

        let (old_ctx, new_ctx) = pm.commit_switch(&mut *scheduler, tid);
        drop(scheduler);
        drop(pm);

        unsafe { thread::switch_context(old_ctx, new_ctx); }
    })
}

pub fn destroy_process(pid: PID) {
    let tids: Vec<TID> = {
        let pm = PROCESS_MANAGER.lock();
        pm.process(pid).threads.clone()
    };
    {
        let mut sched = SCHEDULER.lock();
        for t in &tids { sched.remove_from_runnables(*t); }
    }
    PROCESS_MANAGER.lock().destroy_process(pid);
}

impl ProcessManager {
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

pub fn create_user_process(name: String) -> PID {
    interrupts::without_interrupts(|| {
        let space = {
            let mut mm = MEMORY_MANAGER.lock();
            let mm = mm.as_mut().expect("MemoryManager not initialized");
            AddressSpace::new(&mut mm.frame_allocator, mm.physical_memory_offset)
        };
        PROCESS_MANAGER.lock().create_user_process_with_space(PID(0), name, space)
    })
}

/// Free function wrapper to map memory safely
pub fn map_user_region_for_process(
    pid: PID, start: VirtAddr, size: usize,
    leaf_flags: PageTableFlags, kind: VmaKind,
) -> Vec<PhysFrame<Size4KiB>> {
    // Lock order: MM first, then PM
    let mut mm = MEMORY_MANAGER.lock();
    let mm = mm.as_mut().expect("MemoryManager not initialized");
    let off = mm.physical_memory_offset;
    PROCESS_MANAGER.lock()
        .map_user_region_for(pid, &mut mm.frame_allocator, off, start, size, leaf_flags, kind)
}