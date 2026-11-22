//! Free-function wrappers that orchestrate locking across subsystems.
//!
//! Lock order is always: **Scheduler → ProcessManager → MemoryManager**.
//! The `PROCESS_MANAGER` lock is never held across a context switch.

use alloc::string::String;
use alloc::vec::Vec;

use x86_64::registers::control::Cr3;
use x86_64::structures::paging::PageTableFlags;
use x86_64::VirtAddr;

use crate::{log_debug, log_trace};
use crate::memory::address_space::AddressSpace;
use crate::memory::memory_manager::{FaultAction, MemoryManager, MEMORY_MANAGER, Oom};
use crate::memory::vma::VmaKind;
use crate::fs::devices::{stderr_file, stdin_file, stdout_file};
use crate::fs::{File, Path};
use crate::process::elf_loader::load_elf;
use crate::process::initial_stack::{layout_initial_stack, ArgsTooLarge};
use crate::process::thread::{self, ThreadState, TID};
use crate::process::{CHILD_EXITS, EXIT_FAULT, PID};
use crate::syscall::uaccess::copy_to_user;
use crate::scheduler::scheduler::SCHEDULER;
use crate::scheduler::wait_queue::Interrupted;

use super::manager::PROCESS_MANAGER;

/// Run `f` on `pid`'s address space and the memory manager.
///
/// Takes PROCESS_MANAGER then MEMORY_MANAGER, which is the documented lock
/// order, and holds both for the duration of `f`. Returns `None` if the
/// process has no user address space (kernel process).
///
/// Panics if the memory manager has not been initialized; that only happens
/// if this is called before `rust_os::init`.
fn with_space_and_mm<R>(
    pid: PID,
    f: impl FnOnce(&mut AddressSpace, &mut MemoryManager) -> R,
) -> Option<R> {
    let mut pm = PROCESS_MANAGER.lock();
    let space = pm.process_mut(pid).address_space.as_mut()?;

    let mut mm_guard = MEMORY_MANAGER.lock();
    let mm = mm_guard.as_mut().expect("MemoryManager not initialized");

    Some(f(space, mm))
}

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
        scheduler.get_current() != Some(tid),
        "remove_thread: refusing to free the stack of the currently running \
         thread (TID {:?}) — switch away first, then reap from a different \
         thread's stack (e.g. the parent, during wait())",
        tid
    );
    scheduler.remove_from_queues(tid);
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

pub fn create_user_thread(pid: PID, entry_point: VirtAddr) -> Result<TID, Oom> {
    let mut scheduler = SCHEDULER.lock();
    let mut pm = PROCESS_MANAGER.lock();
    let tid = pm.create_user_thread_locked(pid, entry_point)?;
    drop(pm);
    scheduler.add_runnable(tid);
    Ok(tid)
}

pub fn switch_to(tid: TID) {
    let mut scheduler = SCHEDULER.lock();
    let mut pm = PROCESS_MANAGER.lock();

    // The reaper skips the current thread because its kernel stack is active.
    pm.reap_dead_threads(scheduler.get_current());

    let new_owner = pm.thread(tid).get_owner();
    let new_is_user = pm.thread(tid).is_user;
    let new_kernel_stack_top = pm.thread(tid).kernel_stack_top;

    log_trace!("switch_to {:?} of PID({})", tid, new_owner.0);

    let changing = scheduler
        .get_current()
        .map(|c| pm.thread(c).get_owner() != new_owner)
        .unwrap_or(true);

    let (old_ctx, new_ctx) = pm.commit_switch(&mut *scheduler, tid);

    if new_is_user {
        unsafe { crate::interrupts::gdt::set_tss_rsp0(new_kernel_stack_top); }
    }

    if changing {
        if let Some(space) = &pm.process(new_owner).address_space {
            let (_, flags) = Cr3::read();
            unsafe { Cr3::write(space.cr3(), flags); }
            log_trace!("user address space of PID({}) active", new_owner.0);
        } else {
            let mm_guard = MEMORY_MANAGER.lock();
            let mm = mm_guard.as_ref().expect("MemoryManager not initialized");
            let (_, flags) = Cr3::read();
            unsafe { Cr3::write(mm.kernel_p4_frame, flags); }
            log_trace!("kernel address space active");
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
            sched.remove_from_queues(*t);
        }
    }
    PROCESS_MANAGER.lock().destroy_process(pid);
}

pub fn create_user_process(name: String) -> Result<PID, Oom> {
    let space = {
        let mut mm = MEMORY_MANAGER.lock();
        let mm = mm.as_mut().expect("MemoryManager not initialized");
        AddressSpace::try_new(&mut mm.frame_allocator, mm.physical_memory_offset).ok_or(Oom)?
    };
    Ok(PROCESS_MANAGER.lock().create_user_process_with_space(PID(0), name, space))
}

/// Start a kernel-owned (parent PID 0) user process from an ELF image, with
/// argv `[name]`, cwd `/` and the console as fds 0–2. Its thread is
/// enqueued.
///
/// The argv matters even for programs that ignore it: a ulib program's
/// crt0 reads `argc` from the initial stack.
pub fn spawn_elf(name: String, elf_bytes: &[u8]) -> Result<(PID, TID), Oom> {
    let argv = [name.as_str()];
    spawn_process(SpawnArgs {
        parent: PID(0),
        name: name.clone(),
        elf: elf_bytes,
        argv: &argv,
        cwd: Path::root(),
        stdio: [Some(stdin_file()), Some(stdout_file()), Some(stderr_file())],
    })
    // A single argv entry only fails to fit if `name` is over ~16 KiB.
    .map_err(|_| Oom)
}

/// Everything a new user process starts with.
pub struct SpawnArgs<'a> {
    /// Recorded as the child's parent, and the child is added to the
    /// parent's `children`. `PID(0)` for processes the kernel starts.
    pub parent: PID,
    /// Process name (for debugging).
    pub name: String,
    /// The whole ELF file.
    pub elf: &'a [u8],
    /// Becomes the child's `argc`/`argv`. By convention `argv[0]` is the
    /// program name, but nothing enforces that, and an empty list is allowed.
    pub argv: &'a [&'a str],
    /// The child's working directory.
    pub cwd: Path,
    /// The child's fds 0, 1 and 2. `None` leaves that fd closed.
    pub stdio: [Option<File>; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnError {
    /// Out of memory, or the ELF could not be loaded (`load_elf` reports
    /// both as `Oom`).
    Oom,
    /// `argv` doesn't fit on the initial stack (`E2BIG`).
    ArgsTooLarge,
}

impl From<Oom> for SpawnError {
    fn from(_: Oom) -> Self {
        SpawnError::Oom
    }
}

impl From<ArgsTooLarge> for SpawnError {
    fn from(_: ArgsTooLarge) -> Self {
        SpawnError::ArgsTooLarge
    }
}

/// Create a user process from `args` and make its first thread runnable.
/// Returns the new process and its thread.
///
/// The thread must not be able to run before the process is complete. The
/// timer can preempt the caller at any point where interrupts are on, so
/// the thread goes on the run queue **last**, after:
/// 1. the process exists with the right parent (and is in the parent's
///    `children`), cwd and stdio;
/// 2. the ELF is loaded (`load_elf`, as in `spawn_elf`);
/// 3. a user stack is allocated, the argv image from
///    `initial_stack::layout_initial_stack(stack_top, argv)` is copied to
///    `[rsp, stack_top)` (`crate::syscall::uaccess::copy_to_user` works for
///    any process), and the thread is created with that `rsp` as its initial
///    user stack pointer (`Thread::new_user`).
///
/// That's why the thread is created with
/// `create_user_thread_with_stack_locked`, which doesn't enqueue.
///
/// On any error, nothing is left behind: destroy the half-built process
/// (`destroy_process`) and return the error. The parent's `children` must
/// not keep the dead PID.
pub fn spawn_process(args: SpawnArgs) -> Result<(PID, TID), SpawnError> {
    let SpawnArgs {
        parent,
        name,
        elf,
        argv,
        cwd,
        stdio,
    } = args;

    // 1. The process: its own address space, linked to `parent`, with its
    //    cwd and stdio.
    let space = {
        let mut mm = MEMORY_MANAGER.lock();
        let mm = mm.as_mut().expect("MemoryManager not initialized");
        AddressSpace::try_new(&mut mm.frame_allocator, mm.physical_memory_offset).ok_or(Oom)?
    };
    let pid = {
        let mut pm = PROCESS_MANAGER.lock();
        let pid = pm.create_user_process_with_space(parent, name, space);
        let process = pm.process_mut(pid);
        process.set_cwd(cwd);
        for (fd, file) in stdio.into_iter().enumerate() {
            process.fd_table[fd] = file;
        }
        pid
    };

    // 2.–3. Everything that can fail, so cleanup is in one place.
    match load_and_create_thread(pid, elf, argv) {
        Ok(tid) => {
            // Last: from here on the timer may run the new thread.
            SCHEDULER.lock().add_runnable(tid);
            Ok((pid, tid))
        }
        Err(error) => {
            PROCESS_MANAGER.lock().unlink_from_parent(pid);
            destroy_process(pid);
            Err(error)
        }
    }
}

/// Load the ELF, set up the user stack with argv, and create the (not yet
/// runnable) thread. PROCESS_MANAGER is not held between the steps:
/// `with_space_and_mm` and `copy_to_user` lock it themselves.
fn load_and_create_thread(pid: PID, elf: &[u8], argv: &[&str]) -> Result<TID, SpawnError> {
    let entry = with_space_and_mm(pid, |space, mm| load_elf(elf, space, mm))
        .expect("new user process has no address space")?;
    let stack_top = with_space_and_mm(pid, |space, mm| mm.allocate_user_stack(space))
        .expect("new user process has no address space")?;

    let initial = layout_initial_stack(stack_top, argv)?;
    copy_to_user(pid, initial.rsp, &initial.image).map_err(|_| SpawnError::Oom)?;

    Ok(PROCESS_MANAGER
        .lock()
        .create_user_thread_with_stack_locked(pid, entry, initial.rsp))
}

/// Why `wait_child` returned without an exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitError {
    /// `child` is not a child of `parent` that is still waitable: no such
    /// process, someone else's child, or already collected (`ECHILD`).
    NotChild,
    /// The *waiting* thread's process is being killed (`EINTR`).
    Interrupted,
}

impl From<Interrupted> for WaitError {
    fn from(_: Interrupted) -> Self {
        WaitError::Interrupted
    }
}

/// `wait4`: block until `child` (a child of `parent`) has exited, then
/// collect it and return its exit code.
///
/// - Returns at once if the child has already exited (it is a zombie).
/// - Otherwise sleeps on `CHILD_EXITS` (`wait_until`) until it exits.
/// - Collecting means the child is no longer `parent`'s child, so a second
///   `wait_child` for it is `NotChild`. Its process slot is freed (and the
///   PID becomes reusable) once its threads have been reaped, right away if
///   that already happened.
/// - `NotChild` if `child` isn't (or is no longer) a child of `parent`.
/// - `Interrupted` if the *calling* thread's process gets a kill request
///   while waiting. Check with `current_kill_requested()`, but not while
///   you hold PROCESS_MANAGER, because it takes SCHEDULER and PM itself.
///
/// `parent` is a parameter (not "the current process") so kernel tests can
/// wait on behalf of a user process.
pub fn wait_child(parent: PID, child: PID) -> Result<i32, WaitError> {
    // The condition runs with interrupts off (see `wait_until`), so a child
    // exit can't slip in between "not exited yet" and going to sleep.
    CHILD_EXITS.wait_until(|| {
        // Before taking PM: current_kill_requested locks SCHEDULER and PM.
        if current_kill_requested() {
            return Some(Err(WaitError::Interrupted));
        }

        let mut pm = PROCESS_MANAGER.lock();
        let is_child = child != parent
            && pm.try_process(child).and_then(|process| process.parent()) == Some(parent);
        if !is_child {
            return Some(Err(WaitError::NotChild));
        }

        // Still running: sleep until the next exit, then check again.
        let code = pm.process(child).exit_code?;

        // Collect it. Now that it's not `parent`'s child, a second wait gets
        // NotChild. With PID 0 as its parent, the reaper frees it if its
        // thread is still waiting to be reaped. Freeing it here in that case
        // would recycle that thread's TID while the reap queue still holds it.
        pm.unlink_from_parent(child);
        pm.process_mut(child).set_parent(PID(0));
        if pm.process(child).threads.is_empty() {
            pm.destroy_process(child);
        }
        Some(Ok(code))
    })
}

/// Ask process `pid` to exit with `code` (Ctrl-C uses `EXIT_INTERRUPTED`).
/// Called from the keyboard IRQ (via `tty::push_key`), so it must not block.
///
/// It must not tear the process down itself. A thread of that process may be
/// in the middle of a syscall, and killing it there would abandon whatever
/// it holds (locks, `Arc`s on its kernel stack). Instead:
/// - record the request in `pending_kill`, if the process exists, is a user
///   process, and hasn't exited or been asked already (the first request
///   wins);
/// - wake its threads that are `Blocked` or `Sleeping`, so they reach a safe
///   point soon. A blocked reader's wait condition sees the kill and returns
///   `Interrupted`. A sleeper's `sleep` returns early. Their stale wait/sleep
///   queue entries are harmless: both queues skip threads that aren't in the
///   state they expect.
///
/// The process then exits in `exit_if_killed`.
pub fn request_kill(pid: PID, code: i32) {
    let mut scheduler = SCHEDULER.lock();
    let mut pm = PROCESS_MANAGER.lock();

    let Some(process) = pm.try_process_mut(pid) else {
        return;
    };
    // Not the kernel, not a process that already exited, and the first
    // request wins.
    if !process.is_user() || process.exit_code.is_some() || process.pending_kill.is_some() {
        return;
    }
    process.pending_kill = Some(code);

    // Wake its waiting threads so they reach a safe point soon. An index
    // loop, not a cloned list: this runs in the keyboard IRQ, which stays
    // allocation-free.
    for i in 0..pm.process(pid).threads.len() {
        let tid = pm.process(pid).threads[i];
        if matches!(
            pm.get_thread_state(tid),
            ThreadState::Blocked | ThreadState::Sleeping
        ) {
            pm.wake_thread(tid);
            scheduler.add_runnable(tid);
        }
    }
}

/// The safe point: if the current thread's process has a pending kill, exit
/// it with that code (never returns). Otherwise do nothing.
///
/// Called on the way back to user mode: at the end of every syscall, and
/// from the timer interrupt when it interrupted user mode (so a program
/// that never makes a syscall can still be killed). No locks are held at
/// those points.
pub fn exit_if_killed(tid: TID) {
    // Look the code up and release PM before exiting: the exit path locks
    // PM again.
    let pending = {
        let pm = PROCESS_MANAGER.lock();
        if !pm.thread_exists(tid) {
            return;
        }
        let owner = pm.thread(tid).get_owner();
        pm.try_process(owner).and_then(|process| process.pending_kill)
    };
    if let Some(code) = pending {
        exit_current_thread(tid, code);
    }
}

/// Whether the current thread's process has a pending kill. For the
/// conditions of interruptible waits (`tty::read`, `wait_child`).
///
/// Takes SCHEDULER, then PROCESS_MANAGER, so don't call it with either held.
pub fn current_kill_requested() -> bool {
    let Some(tid) = SCHEDULER.lock().get_current() else {
        return false;
    };
    let pm = PROCESS_MANAGER.lock();
    if !pm.thread_exists(tid) {
        return false;
    }
    let owner = pm.thread(tid).get_owner();
    pm.try_process(owner)
        .map(|process| process.pending_kill.is_some())
        .unwrap_or(false)
}

pub fn map_user_region_for_process(
    pid: PID,
    _start: VirtAddr, // Unused since `mmap` finds its own region
    size: usize,
    leaf_flags: PageTableFlags,
    kind: VmaKind,
) -> Result<VirtAddr, Oom> {
    with_space_and_mm(pid, |space, mm| mm.mmap(space, size, leaf_flags, kind))
        .expect("Process has no address space")
}

pub fn register_vma_only(
    pid: PID,
    start: VirtAddr,
    size: usize,
    flags: PageTableFlags,
    kind: VmaKind,
) {
    with_space_and_mm(pid, |space, mm| mm.register_vma(space, start, size, flags, kind))
        .expect("Process has no address space");
}

/// Kill the current thread after a fatal fault (exit code `EXIT_FAULT`).
/// Kernel test threads also use it to end themselves.
pub fn kill_current_thread_and_schedule(tid: TID) -> ! {
    exit_current_thread(tid, EXIT_FAULT)
}

/// End the current thread `tid` (`exit(code)`, a fault, or a kill) and
/// switch away. Never returns.
///
/// When the last live thread of a *user* process exits, the process has
/// exited:
/// - its exit code is recorded. A pending kill's code wins over `code`,
///   because the kill is why it's dying;
/// - its files are closed, and its children are re-parented to PID 0;
/// - `wait4` callers are woken (`CHILD_EXITS`), after every lock is dropped.
///
/// The `Process` itself stays as a zombie if its parent may still collect
/// it (see `finalize_process_if_empty`). Kernel threads (PID 0) end here
/// too, without any of that.
pub fn exit_current_thread(tid: TID, code: i32) -> ! {
    // This thread never resumes, so there is no interrupt state to restore.
    // Keep interrupts off from here on, so no interrupt runs on the dying
    // thread's stack after its address space is gone. The next thread's
    // interrupt flag comes back from its saved RFLAGS in switch_context.
    x86_64::instructions::interrupts::disable();
    log_debug!("killing {:?}", tid);
    {
        let mut sched = SCHEDULER.lock();
        sched.remove_from_queues(tid);
        // Keep this TID current until the switch: the reaper must not free the
        // kernel stack this code is using. commit_switch updates it on switch.
    }
    let process_exited = {
        let mut pm = PROCESS_MANAGER.lock();
        let owner = pm.thread(tid).get_owner();
        pm.kill_current_thread(tid); // Running -> Exited -> Zombie (+ destroy AS if last)

        let process_exited =
            owner != PID(0) && pm.process(owner).is_user() && !pm.has_live_threads(owner);
        if process_exited {
            let process = pm.process_mut(owner);
            process.exit_code = Some(process.pending_kill.unwrap_or(code));
            // Close its files now, as Linux does at exit, rather than when a
            // parent eventually collects the zombie.
            process.fd_table.clear();
            pm.reparent_children_to_kernel(owner);
        }

        pm.enqueue_for_reap(tid); // defer freeing the kernel stack & recycling the TID
        process_exited
    };
    // With no locks held: wake_all takes SCHEDULER and PM itself.
    if process_exited {
        CHILD_EXITS.wake_all();
    }
    crate::scheduler::scheduler::schedule();
    crate::hlt_loop();
}

pub fn sys_brk(pid: PID, new_break: VirtAddr) -> Result<VirtAddr, Oom> {
    with_space_and_mm(pid, |space, mm| mm.sys_brk(space, new_break))
        .expect("Process has no address space")
}

pub fn demand_map_page(
    pid: PID,
    page_addr: VirtAddr,
    leaf_flags: PageTableFlags,
) -> Result<(), Oom> {
    with_space_and_mm(pid, |space, mm| mm.demand_map_page(space, page_addr, leaf_flags))
        .expect("Process has no address space")
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

    with_space_and_mm(pid, |space, mm| {
        // Keep anonymous mappings away from the bottom-up brk region.
        let start = space.find_free_region_top_down(size as u64).ok_or(Oom)?;
        mm.mmap_at(space, start, size, flags, VmaKind::Mapped)?;
        Ok(start)
    })
    .ok_or(Oom)?
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

    with_space_and_mm(pid, |space, mm| mm.munmap(space, addr, size)).ok_or(Oom)
}
