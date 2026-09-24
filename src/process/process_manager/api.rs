//! Free-function wrappers that orchestrate locking across subsystems.
//!
//! Lock order is always: **Scheduler → ProcessManager → MemoryManager**.
//! The `PROCESS_MANAGER` lock is never held across a context switch.

use alloc::string::String;
use alloc::vec::Vec;

use x86_64::registers::control::Cr3;
use x86_64::structures::paging::PageTableFlags;
use x86_64::VirtAddr;

use crate::debugln;
use crate::memory::address_space::AddressSpace;
use crate::memory::memory_manager::{FaultAction, MEMORY_MANAGER, Oom};
use crate::memory::vma::VmaKind;
use crate::process::elf_loader::load_elf;
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

pub fn create_user_thread(pid: PID, entry_point: VirtAddr) -> TID {
    let mut scheduler = SCHEDULER.lock();
    let mut pm = PROCESS_MANAGER.lock();
    let tid = pm.create_user_thread_locked(pid, entry_point);
    drop(pm);
    scheduler.add_runnable(tid);
    tid
}

pub fn switch_to(tid: TID) {
    let mut scheduler = SCHEDULER.lock();
    let mut pm = PROCESS_MANAGER.lock();

    // The reaper skips the current thread because its kernel stack is active.
    pm.reap_dead_threads(scheduler.current);

    let new_owner = pm.thread(tid).get_owner();
    let new_is_user = pm.thread(tid).is_user;
    let new_kernel_stack_top = pm.thread(tid).kernel_stack_top;

    debugln!("Thread: {:?}, Process:{:?}", tid, new_owner);

    let changing = scheduler
        .current
        .map(|c| pm.thread(c).get_owner() != new_owner)
        .unwrap_or(true);

    let (old_ctx, new_ctx) = pm.commit_switch(&mut *scheduler, tid);

    if new_is_user {
        unsafe { crate::interrupts::gdt::set_tss_rsp0(new_kernel_stack_top); }
    }

    if changing {
        if let Some(space) = &pm.process(new_owner).address_space {
            debugln!("[switch_to] switching to user CR3 for TID {:?}", tid);
            let (_, flags) = Cr3::read();
            unsafe { Cr3::write(space.cr3(), flags); }
            debugln!("[switch_to] CR3 switch succeeded for TID {:?}", tid);
        } else {
            debugln!("[switch_to] switching to kernel CR3 for TID {:?}", tid);
            let mm_guard = MEMORY_MANAGER.lock();
            let mm = mm_guard.as_ref().expect("MemoryManager not initialized");
            let (_, flags) = Cr3::read();
            unsafe { Cr3::write(mm.kernel_p4_frame, flags); }
            debugln!("[switch_to] kernel CR3 restored for TID {:?}", tid);
        }
    }

    // Locks are acquired Scheduler -> ProcessManager, so release them in the
    // reverse order. Dropping `scheduler` first would restore interrupts while
    // `PROCESS_MANAGER` is still locked. A timer interrupt in that window can
    // enter the scheduler and spin forever on `PROCESS_MANAGER`.
    drop(pm);
    drop(scheduler);

    unsafe {
        thread::switch_context(old_ctx, new_ctx);
    }
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

/// Create a user process, load an ELF image into its private address space,
/// and enqueue a user thread at the ELF entry point.
pub fn spawn_elf(name: String, elf_bytes: &[u8]) -> Result<(PID, TID), Oom> {
    let pid = create_user_process(name);

    let entry_point = {
        let mut pm = PROCESS_MANAGER.lock();
        let space = pm
            .process_mut(pid)
            .address_space
            .as_mut()
            .expect("new user process has no address space");

        let mut mm = MEMORY_MANAGER.lock();
        let mm = mm.as_mut().expect("MemoryManager not initialized");
        load_elf(elf_bytes, space, mm)
    };

    match entry_point {
        Ok(entry_point) => {
            let tid = create_user_thread(pid, entry_point);
            Ok((pid, tid))
        }
        Err(error) => {
            destroy_process(pid);
            Err(error)
        }
    }
}

pub fn map_user_region_for_process(
    pid: PID,
    _start: VirtAddr, // Unused since `mmap` finds its own region
    size: usize,
    leaf_flags: PageTableFlags,
    kind: VmaKind,
) -> Result<VirtAddr, Oom> {
    let mut pm_guard = PROCESS_MANAGER.lock();
    let proc = pm_guard.process_mut(pid);
    let space = proc.address_space.as_mut().expect("Process has no address space");

    let mut mm_guard = MEMORY_MANAGER.lock();
    let mm = mm_guard.as_mut().expect("MemoryManager not initialized");

    mm.mmap(space, size, leaf_flags, kind)
}

pub fn register_vma_only(
    pid: PID,
    start: VirtAddr,
    size: usize,
    flags: PageTableFlags,
    kind: VmaKind,
) {
    let mut pm_guard = PROCESS_MANAGER.lock();
    let proc = pm_guard.process_mut(pid);
    let space = proc.address_space.as_mut().expect("Process has no address space");

    let mut mm_guard = MEMORY_MANAGER.lock();
    let mm = mm_guard.as_mut().expect("MemoryManager not initialized");

    mm.register_vma(space, start, size, flags, kind);
}

pub fn kill_current_thread_and_schedule(tid: TID) -> ! {
    debugln!("Killing thread {:?}", tid);
    {
        let mut sched = SCHEDULER.lock();
        sched.remove_from_runnables(tid);
        // Keep this TID current until the switch: the reaper must not free the
        // kernel stack this code is using. commit_switch updates it on switch.
    }
    {
        let mut pm = PROCESS_MANAGER.lock();
        pm.kill_current_thread(tid); // Running -> Exited -> Zombie (+ destroy AS if last)
        pm.enqueue_for_reap(tid);    // defer freeing the kernel stack & recycling the TID
    }
    crate::scheduler::scheduler::schedule();
    crate::hlt_loop();
}

pub fn sys_brk(pid: PID, new_break: VirtAddr) -> Result<VirtAddr, crate::memory::memory_manager::Oom> {
    let mut pm_guard = PROCESS_MANAGER.lock();
    let proc = pm_guard.process_mut(pid);
    let space = proc.address_space.as_mut().expect("Process has no address space");

    let mut mm_guard = crate::memory::memory_manager::MEMORY_MANAGER.lock();
    let mm = mm_guard.as_mut().expect("MemoryManager not initialized");

    mm.sys_brk(space, new_break)
}

pub fn demand_map_page(
    pid: PID,
    page_addr: VirtAddr,
    leaf_flags: PageTableFlags,
) -> Result<(), crate::memory::memory_manager::Oom> {
    let mut pm_guard = PROCESS_MANAGER.lock();

    let proc = pm_guard.process_mut(pid);
    let space = proc
        .address_space
        .as_mut()
        .expect("Process has no address space");

    let mut mm_guard = MEMORY_MANAGER.lock();
    let mm = mm_guard.as_mut().expect("MemoryManager not initialized");

    mm.demand_map_page(space, page_addr, leaf_flags)
}

pub fn handle_thread_page_fault(tid: TID, fault_addr: VirtAddr) -> (PID, FaultAction) {
    let mut pm_guard = PROCESS_MANAGER.lock();

    let owner = pm_guard.thread(tid).get_owner();

    // Kernel processes have no user address space to resolve the fault.
    if pm_guard.process(owner).address_space.is_none() {
        return (owner, FaultAction::Segfault);
    }

    let space = pm_guard
        .process_mut(owner)
        .address_space
        .as_mut()
        .expect("address space was checked above");

    let mut mm_guard = MEMORY_MANAGER.lock();

    let action = match mm_guard.as_mut() {
        Some(mm) => mm.handle_page_fault(space, fault_addr),
        None => FaultAction::Oom,
    };

    (owner, action)
}

pub fn get_current_brk(pid: PID) -> Option<VirtAddr> {
    let pm_guard = PROCESS_MANAGER.lock();
    let proc = pm_guard.process(pid);
    let space = proc.address_space.as_ref()?;

    Some(space.heap_break)
}

pub fn sys_mmap_anonymous(
    pid: PID,
    size: usize,
    flags: PageTableFlags,
) -> Result<VirtAddr, Oom> {
    if size == 0 || (size & 0xFFF) != 0 {
        return Err(Oom);
    }

    let mut pm_guard = PROCESS_MANAGER.lock();

    let proc = pm_guard.process_mut(pid);

    let space = proc
        .address_space
        .as_mut()
        .ok_or(Oom)?;

    // Keep anonymous mappings away from the bottom-up brk region.
    let start = space
        .find_free_region_top_down(size as u64)
        .ok_or(Oom)?;

    let mut mm_guard = MEMORY_MANAGER.lock();

    let mm = mm_guard
        .as_mut()
        .ok_or(Oom)?;

    mm.mmap_at(space, start, size, flags, VmaKind::Mapped)?;

    Ok(start)
}

pub fn sys_munmap(
    pid: PID,
    addr: VirtAddr,
    size: usize,
) -> Result<(), Oom> {
    if size == 0 {
        return Ok(());
    }

    if (addr.as_u64() & 0xFFF) != 0 || (size & 0xFFF) != 0 {
        return Err(Oom);
    }

    let mut pm_guard = PROCESS_MANAGER.lock();

    let proc = pm_guard.process_mut(pid);

    let space = proc
        .address_space
        .as_mut()
        .ok_or(Oom)?;

    let mut mm_guard = MEMORY_MANAGER.lock();

    let mm = mm_guard
        .as_mut()
        .ok_or(Oom)?;

    mm.munmap(space, addr, size);

    Ok(())
}
