// src/process/process_manager/api.rs

//! Free-function wrappers that orchestrate locking across subsystems.
//!
//! Lock order is always: **Scheduler → ProcessManager → MemoryManager**.
//! The `PROCESS_MANAGER` lock is never held across a context switch.

use alloc::string::String;
use alloc::vec::Vec;

use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{FrameAllocator, PageTableFlags, PhysFrame, Size4KiB};
use x86_64::VirtAddr;

use crate::debugln;
use crate::memory::address_space::AddressSpace;
use crate::memory::memory_manager::{MEMORY_MANAGER, Oom};
use crate::memory::vma::VmaKind;
use crate::process::thread::{self, TID};
use crate::process::PID;
use crate::scheduler::scheduler::SCHEDULER;

use super::manager::PROCESS_MANAGER;

pub fn create_thread(pid: PID, entry_point: extern "C" fn() -> !) -> TID {
    let mut scheduler = SCHEDULER.lock();
    let mut pm = PROCESS_MANAGER.lock();
    let tid = pm.create_thread_locked(pid, entry_point);
    drop(pm);
    scheduler.add_runnable(tid);
    tid
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
    let mut scheduler = SCHEDULER.lock();
    let mut pm = PROCESS_MANAGER.lock();
    let tid = pm.create_idle_thread_locked(entry);
    drop(pm);
    scheduler.set_idle_thread(tid);
    tid
}

pub fn switch_to(tid: TID) {
    let mut scheduler = SCHEDULER.lock();
    let mut pm = PROCESS_MANAGER.lock();

    let new_owner = pm.thread(tid).get_owner();

    debugln!("Thread: {:?}, Process:{:?}", tid, new_owner);

    let changing = scheduler
        .current
        .map(|c| pm.thread(c).get_owner() != new_owner)
        .unwrap_or(true);

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
}

pub fn destroy_process(pid: PID) {
    let tids: Vec<TID> = {
        let pm = PROCESS_MANAGER.lock();
        pm.process(pid).threads.clone()
    };
    {
        let mut sched = SCHEDULER.lock();
        for t in &tids {
            sched.remove_from_runnables(*t);
        }
    }
    PROCESS_MANAGER.lock().destroy_process(pid);
}

pub fn create_user_process(name: String) -> PID {
    let space = {
        let mut mm = MEMORY_MANAGER.lock();
        let mm = mm.as_mut().expect("MemoryManager not initialized");
        AddressSpace::new(&mut mm.frame_allocator, mm.physical_memory_offset)
    };
    PROCESS_MANAGER.lock().create_user_process_with_space(PID(0), name, space)
}

pub fn map_user_region_for_process(
    pid: PID,
    _start: VirtAddr, // Unused since `mmap` finds its own region
    size: usize,
    leaf_flags: PageTableFlags,
    kind: VmaKind,
) -> Result<VirtAddr, Oom> {
    let mut mm_guard = MEMORY_MANAGER.lock();
    let mm = mm_guard.as_mut().expect("MemoryManager not initialized");
    let mut pm_guard = PROCESS_MANAGER.lock();
    let proc = pm_guard.process_mut(pid);
    let space = proc.address_space.as_mut().expect("Process has no address space");
    
    mm.mmap(space, size, leaf_flags, kind)
}

pub fn register_vma_only(
    pid: PID,
    start: VirtAddr,
    size: usize,
    flags: PageTableFlags,
    kind: VmaKind,
) {
    let mut mm_guard = MEMORY_MANAGER.lock();
    let mm = mm_guard.as_mut().expect("MemoryManager not initialized");
    let mut pm_guard = PROCESS_MANAGER.lock();
    let proc = pm_guard.process_mut(pid);
    let space = proc.address_space.as_mut().expect("Process has no address space");
    
    mm.register_vma(space, start, size, flags, kind);
}

pub fn kill_current_thread_and_schedule(tid: TID) -> ! {
    debugln!("Killing thread {:?}", tid);
    {
        let mut sched = SCHEDULER.lock();
        sched.remove_from_runnables(tid);
        sched.current = None; // critical: don't save our context
    }
    {
        let mut pm = PROCESS_MANAGER.lock();
        pm.kill_current_thread(tid);
    }

    crate::scheduler::scheduler::schedule();
    crate::hlt_loop();
}

pub fn sys_brk(pid: PID, new_break: VirtAddr) -> Result<VirtAddr, crate::memory::memory_manager::Oom> {
    let mut mm_guard = crate::memory::memory_manager::MEMORY_MANAGER.lock();
    let mm = mm_guard.as_mut().expect("MemoryManager not initialized");
    let mut pm_guard = PROCESS_MANAGER.lock();
    let proc = pm_guard.process_mut(pid);
    let space = proc.address_space.as_mut().expect("Process has no address space");
    
    space.sys_brk(mm, new_break)
}