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
use crate::memory::memory_manager::MEMORY_MANAGER;
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

/// Switch execution to `tid`.
///
/// This is a free function (not a method) so the `PROCESS_MANAGER` lock is
/// released **before** the actual context switch — the switch doesn't return
/// until some other thread switches back, so holding the lock across it would
/// deadlock anyone else who needs the process manager.
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
    start: VirtAddr,
    size: usize,
    leaf_flags: PageTableFlags,
    kind: VmaKind,
) -> Vec<PhysFrame<Size4KiB>> {
    let mut mm = MEMORY_MANAGER.lock();
    let mm = mm.as_mut().expect("MemoryManager not initialized");
    let off = mm.physical_memory_offset;
    PROCESS_MANAGER.lock().map_user_region_for(
        pid,
        &mut mm.frame_allocator,
        off,
        start,
        size,
        leaf_flags,
        kind,
    )
}

pub fn register_vma_only(
    pid: PID,
    start: VirtAddr,
    size: usize,
    flags: PageTableFlags,
    kind: VmaKind,
) {
    PROCESS_MANAGER.lock().register_vma_only(pid, start, size, flags, kind);
}

/// Handle an in-bounds page fault safely (Demand Paging).
pub fn demand_map_page(pid: PID, page_addr: VirtAddr, leaf_flags: PageTableFlags) {
    let mut mm_guard = MEMORY_MANAGER.lock();
    let mm = mm_guard.as_mut().expect("MM not initialised");
    let off = mm.physical_memory_offset;

    PROCESS_MANAGER.lock().demand_map_page(
        pid,
        page_addr,
        leaf_flags,
        &mut mm.frame_allocator,
        off,
    );
    drop(mm_guard);

    // Zero the freshly-mapped page so we don't leak kernel data.
    unsafe { core::ptr::write_bytes(page_addr.as_mut_ptr::<u8>(), 0, 4096); }
}

/// Kill the current thread and yield to the scheduler.
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