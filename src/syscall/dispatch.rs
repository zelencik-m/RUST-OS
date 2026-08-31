// src/syscalls/dispatch.rs
use crate::memory::address_space::{is_user_range, USER_SPACE_END, USER_SPACE_START};
use crate::process::process_manager::{
    get_current_brk, kill_current_thread_and_schedule, sys_brk, sys_mmap_anonymous,
    sys_munmap, PROCESS_MANAGER,
};
use crate::process::thread::TID;
use crate::process::PID;
use crate::scheduler::scheduler::SCHEDULER;
use crate::syscall::errors::SysError;
use crate::syscall::frame::SyscallFrame;
use crate::syscall::numbers::*;
use crate::syscall::uaccess::{canonical_user_addr, copy_from_user, validate_user_range};
use x86_64::structures::paging::PageTableFlags;

#[no_mangle]
extern "C" fn syscall_int_handler(frame: &mut SyscallFrame) {
    // FIX 1: Re-enable interrupts early so syscalls are preemptible and don't block the timer.
    // SAFETY: We are not holding any IrqSafeMutex locks yet. IrqSafeMutex will temporarily
    // disable interrupts when acquired and restore them when dropped, preventing deadlocks
    // from timer interrupts firing while a lock is held.
    x86_64::instructions::interrupts::enable();

    // FIX 2: Extract `tid` and `pid` by briefly locking and immediately dropping the guards.
    // WARNING: Do not bind these locks to a variable that spans the `dispatch()` call,
    // or you will deadlock since IrqSafeMutex/spin::Mutex are not reentrant.
    let tid = match SCHEDULER.lock().get_current() {
        Some(tid) => tid,
        None => panic!("syscall with no current thread"),
    };
    let pid = PROCESS_MANAGER.lock().thread(tid).get_owner();

    // Special-case syscalls that never return.
    if frame.rax == SYS_EXIT {
        kill_current_thread_and_schedule(tid);
    }

    frame.rax = dispatch(pid, tid, frame);
}

fn dispatch(pid: PID, tid: TID, frame: &SyscallFrame) -> u64 {
    let args = [
        frame.rdi,
        frame.rsi,
        frame.rdx,
        frame.r10,
        frame.r8,
        frame.r9,
    ];

    match frame.rax {
        SYS_WRITE => ret(sys_write(pid, args[0], args[1], args[2])),
        SYS_BRK => ret(sys_brk_syscall(pid, args[0])),
        SYS_MMAP => ret(sys_mmap_syscall(
            pid, args[0], args[1], args[2], args[3], args[4], args[5],
        )),
        SYS_MUNMAP => ret(sys_munmap_syscall(pid, args[0], args[1])),
        SYS_SCHED_YIELD => ret(sys_yield()),
        SYS_SLEEP_TICKS => ret(sys_sleep_ticks(args[0])),
        SYS_GETPID => pid.0,
        SYS_GETTID => tid.0,
        _ => {
            crate::debugln!("syscall: unknown syscall number {}", frame.rax);
            SysError::Enosys.to_errno()
        }
    }
}

fn ret(res: Result<u64, SysError>) -> u64 {
    match res {
        Ok(value) => value,
        Err(err) => err.to_errno(),
    }
}

fn usize_from_u64(value: u64) -> Result<usize, SysError> {
    let converted = value as usize;
    if converted as u64 == value {
        Ok(converted)
    } else {
        Err(SysError::Einval)
    }
}

fn align_up_page(value: usize) -> Option<usize> {
    value.checked_add(4095).map(|v| v & !4095)
}

fn sys_yield() -> Result<u64, SysError> {
    crate::scheduler::scheduler::yield_now();
    Ok(0)
}

fn sys_sleep_ticks(ticks: u64) -> Result<u64, SysError> {
    crate::scheduler::scheduler::sleep(ticks);
    Ok(0)
}

fn sys_write(
    pid: PID,
    fd: u64,
    user_ptr: u64,
    len: u64,
) -> Result<u64, SysError> {
    // Only stdout/stderr for now.
    if fd != 1 && fd != 2 {
        return Err(SysError::Ebadf);
    }
    if len == 0 {
        return Ok(0);
    }

    let size = usize_from_u64(len)?;
    let src = validate_user_range(user_ptr, size)?;

    let mut written = 0usize;
    let mut addr = src;
    let mut buffer = [0u8; 512];

    while written < size {
        let chunk = core::cmp::min(buffer.len(), size - written);
        
        // `copy_from_user` acquires and releases IrqSafeMutex locks. Because we enabled 
        // interrupts at the top of the syscall handler, interrupts will be restored to 
        // ENABLED after this call returns, allowing the timer to preempt us between chunks.
        copy_from_user(pid, addr, &mut buffer[..chunk])?;
        
        write_chunk(fd, &buffer[..chunk]);
        written += chunk;
        addr = addr + chunk as u64;
    }

    Ok(written as u64)
}

fn write_chunk(fd: u64, bytes: &[u8]) {
    // Fast path for valid UTF-8.
    if let Ok(s) = core::str::from_utf8(bytes) {
        // Print to serial so you see it in your terminal!
        crate::serial_print!("{}", s);
        crate::print!("{}", s);
        return;
    }

    // Fallback: best-effort byte output.
    for &byte in bytes {
        crate::serial_print!("{}", byte as char);
        crate::print!("{}", byte as char);
    }
}

fn sys_brk_syscall(pid: PID, new_break: u64) -> Result<u64, SysError> {
    // brk(0) queries the current break.
    if new_break == 0 {
        return get_current_brk(pid)
            .map(|v| v.as_u64())
            .ok_or(SysError::Efault);
    }

    let addr = canonical_user_addr(new_break)?;
    if addr.as_u64() < USER_SPACE_START || addr.as_u64() > USER_SPACE_END {
        return Err(SysError::Einval);
    }

    match sys_brk(pid, addr) {
        Ok(v) => Ok(v.as_u64()),
        Err(_) => Err(SysError::Enomem),
    }
}

const PROT_READ: u64 = 0x1;
const PROT_WRITE: u64 = 0x2;
const PROT_EXEC: u64 = 0x4;
const MAP_FIXED: u64 = 0x10;
const MAP_ANONYMOUS: u64 = 0x20;

fn sys_mmap_syscall(
    pid: PID,
    _addr: u64,
    len: u64,
    prot: u64,
    flags: u64,
    _fd: u64,
    _offset: u64,
) -> Result<u64, SysError> {
    let len = usize_from_u64(len)?;
    if len == 0 {
        return Err(SysError::Einval);
    }

    // For now, only anonymous mappings are supported.
    if (flags & MAP_ANONYMOUS) == 0 {
        return Err(SysError::Einval);
    }

    // Fixed mappings are not supported yet.
    if (flags & MAP_FIXED) != 0 {
        return Err(SysError::Einval);
    }

    // PROT_NONE is not supported yet.
    if (prot & (PROT_READ | PROT_WRITE | PROT_EXEC)) == 0 {
        return Err(SysError::Einval);
    }

    // FIX 3: W^X enforcement. Reject mappings that are both writable and executable.
    if (prot & PROT_WRITE) != 0 && (prot & PROT_EXEC) != 0 {
        return Err(SysError::Einval);
    }

    let size = align_up_page(len).ok_or(SysError::Einval)?;

    let mut pt_flags =
        PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
    if (prot & PROT_WRITE) != 0 {
        pt_flags |= PageTableFlags::WRITABLE;
    }
    if (prot & PROT_EXEC) == 0 {
        pt_flags |= PageTableFlags::NO_EXECUTE;
    }

    let start = sys_mmap_anonymous(pid, size, pt_flags)
        .map_err(|_| SysError::Enomem)?;

    Ok(start.as_u64())
}

fn sys_munmap_syscall(
    pid: PID,
    addr: u64,
    len: u64,
) -> Result<u64, SysError> {
    let len = usize_from_u64(len)?;
    if len == 0 {
        return Err(SysError::Einval);
    }

    let addr = canonical_user_addr(addr)?;

    // munmap address must be page-aligned.
    if (addr.as_u64() & 0xFFF) != 0 {
        return Err(SysError::Einval);
    }

    let size = align_up_page(len).ok_or(SysError::Einval)?;

    if !is_user_range(addr, size as u64) {
        return Err(SysError::Efault);
    }

    sys_munmap(pid, addr, size).map_err(|_| SysError::Einval)?;
    Ok(0)
}