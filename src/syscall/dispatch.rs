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
use crate::syscall::uaccess::{
    canonical_user_addr, copy_c_string_from_user, copy_from_user, copy_to_user,
    validate_user_range,
};
use crate::fs::{with_vfs, FileType, OpenFlags, Path, SeekWhence};
use alloc::vec::Vec;
use x86_64::structures::paging::PageTableFlags;

const PATH_MAX: usize = 4096;

#[no_mangle]
extern "C" fn syscall_int_handler(frame: &mut SyscallFrame) {
    // Re-enable interrupts early so syscalls are preemptible and do not block
    // the timer while they are doing work.
    x86_64::instructions::interrupts::enable();

    let tid = match SCHEDULER.lock().get_current() {
        Some(tid) => tid,
        None => panic!("syscall with no current thread"),
    };
    let pid = PROCESS_MANAGER.lock().thread(tid).get_owner();

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
        SYS_READ => ret(sys_read(pid, args[0], args[1], args[2])),
        SYS_WRITE => ret(sys_write(pid, args[0], args[1], args[2])),
        SYS_OPEN => ret(sys_open(pid, args[0], args[1], args[2])),
        SYS_CLOSE => ret(sys_close(pid, args[0])),
        SYS_LSEEK => ret(sys_lseek(pid, args[0], args[1] as i64, args[2])),
        SYS_BRK => ret(sys_brk_syscall(pid, args[0])),
        SYS_MMAP => ret(sys_mmap_syscall(
            pid, args[0], args[1], args[2], args[3], args[4], args[5],
        )),
        SYS_MUNMAP => ret(sys_munmap_syscall(pid, args[0], args[1])),
        SYS_SCHED_YIELD => ret(sys_yield()),
        SYS_SLEEP_TICKS => ret(sys_sleep_ticks(args[0])),
        SYS_GETPID => pid.0,
        SYS_GETTID => tid.0,
        SYS_MKDIR => ret(sys_mkdir(pid, args[0], args[1])),
        SYS_GETDENTS64 => ret(sys_getdents64(pid, args[0], args[1], args[2])),
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

fn process_cwd(pid: PID) -> Path {
    PROCESS_MANAGER.lock().process(pid).cwd().clone()
}

fn sys_open(
    pid: PID,
    user_path: u64,
    raw_flags: u64,
    _mode: u64,
) -> Result<u64, SysError> {
    let path = copy_c_string_from_user(pid, user_path, PATH_MAX)?;
    let flags = OpenFlags::from_raw(raw_flags).map_err(SysError::from_fs_error)?;
    let cwd = process_cwd(pid);

    let file = with_vfs(|vfs| vfs.open(&cwd, &path, flags))
        .map_err(SysError::from_fs_error)?;

    let mut pm = PROCESS_MANAGER.lock();
    pm.process_mut(pid)
        .alloc_fd(file)
        .map_err(SysError::from_fs_error)
}

fn sys_close(pid: PID, fd: u64) -> Result<u64, SysError> {
    let mut pm = PROCESS_MANAGER.lock();
    pm.process_mut(pid)
        .close_fd(fd)
        .map_err(SysError::from_fs_error)?;
    Ok(0)
}

fn sys_read(
    pid: PID,
    fd: u64,
    user_ptr: u64,
    len: u64,
) -> Result<u64, SysError> {
    if len == 0 {
        return Ok(0);
    }

    let size = usize_from_u64(len)?;
    let src = validate_user_range(user_ptr, size)?;

    let mut total = 0usize;
    let mut addr = src;
    let mut buffer = [0u8; 512];

    while total < size {
        let chunk = core::cmp::min(buffer.len(), size - total);

        let n = {
            let mut pm = PROCESS_MANAGER.lock();
            pm.process_mut(pid)
                .file_mut(fd)
                .map_err(SysError::from_fs_error)?
                .read(&mut buffer[..chunk])
                .map_err(SysError::from_fs_error)?
        };

        if n == 0 {
            break;
        }

        copy_to_user(pid, addr, &buffer[..n])?;
        total += n;
        addr = addr + n as u64;

        if n < chunk {
            break;
        }
    }

    Ok(total as u64)
}

fn sys_write(
    pid: PID,
    fd: u64,
    user_ptr: u64,
    len: u64,
) -> Result<u64, SysError> {
    if len == 0 {
        return Ok(0);
    }

    let size = usize_from_u64(len)?;
    let src = validate_user_range(user_ptr, size)?;

    let mut total = 0usize;
    let mut addr = src;
    let mut buffer = [0u8; 512];

    while total < size {
        let chunk = core::cmp::min(buffer.len(), size - total);
        copy_from_user(pid, addr, &mut buffer[..chunk])?;

        let n = {
            let mut pm = PROCESS_MANAGER.lock();
            pm.process_mut(pid)
                .file_mut(fd)
                .map_err(SysError::from_fs_error)?
                .write(&buffer[..chunk])
                .map_err(SysError::from_fs_error)?
        };

        total += n;
        addr = addr + n as u64;

        if n < chunk {
            break;
        }
    }

    Ok(total as u64)
}

fn sys_lseek(
    pid: PID,
    fd: u64,
    offset: i64,
    whence_raw: u64,
) -> Result<u64, SysError> {
    let whence = SeekWhence::from_raw(whence_raw)
        .map_err(SysError::from_fs_error)?;

    let mut pm = PROCESS_MANAGER.lock();
    pm.process_mut(pid)
        .file_mut(fd)
        .map_err(SysError::from_fs_error)?
        .seek(offset, whence)
        .map_err(SysError::from_fs_error)
}

fn sys_mkdir(
    pid: PID,
    user_path: u64,
    _mode: u64,
) -> Result<u64, SysError> {
    let path = copy_c_string_from_user(pid, user_path, PATH_MAX)?;
    let cwd = process_cwd(pid);

    with_vfs(|vfs| vfs.mkdir(&cwd, &path))
        .map_err(SysError::from_fs_error)?;

    Ok(0)
}

/// Return directory entries using the Linux `getdents64` record layout.
///
/// The file offset is used as a directory-entry index, allowing callers to
/// continue with another call when their buffer is too small for every entry.
fn sys_getdents64(
    pid: PID,
    fd: u64,
    user_ptr: u64,
    len: u64,
) -> Result<u64, SysError> {
    let size = usize_from_u64(len)?;
    if size == 0 {
        return Err(SysError::Einval);
    }
    let destination = validate_user_range(user_ptr, size)?;

    let output = {
        let mut pm = PROCESS_MANAGER.lock();
        let file = pm.process_mut(pid)
            .file_mut(fd)
            .map_err(SysError::from_fs_error)?;
        let entries = file.readdir().map_err(SysError::from_fs_error)?;
        let start = usize_from_u64(file.offset())?;
        let mut next = start;
        let mut bytes = Vec::new();

        for entry in entries.iter().skip(start) {
            // linux_dirent64 is a 19-byte header followed by a NUL-terminated
            // name, with each record rounded up to an 8-byte boundary.
            let record_len = (19usize
                .checked_add(entry.name.len())
                .and_then(|n| n.checked_add(1))
                .ok_or(SysError::Einval)?
                + 7) & !7;

            if record_len > size - bytes.len() {
                if bytes.is_empty() {
                    return Err(SysError::Einval);
                }
                break;
            }

            let following = next.checked_add(1).ok_or(SysError::Einval)?;
            // The VFS does not expose inode numbers yet, so provide a stable,
            // non-zero synthetic value for this directory stream.
            bytes.extend_from_slice(&(following as u64).to_le_bytes());
            bytes.extend_from_slice(&(following as i64).to_le_bytes()); // d_off
            bytes.extend_from_slice(&(record_len as u16).to_le_bytes());
            bytes.push(match entry.file_type {
                FileType::Directory => 4, // DT_DIR
                FileType::Regular => 8,   // DT_REG
                FileType::Device => 2,    // DT_CHR
            });
            bytes.extend_from_slice(entry.name.as_bytes());
            bytes.push(0);
            bytes.resize(bytes.len() + record_len - 20 - entry.name.len(), 0);
            next = following;
        }

        file.seek(next as i64, SeekWhence::Set)
            .map_err(SysError::from_fs_error)?;
        bytes
    };

    copy_to_user(pid, destination, &output)?;
    Ok(output.len() as u64)
}

fn sys_brk_syscall(pid: PID, new_break: u64) -> Result<u64, SysError> {
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

    if (flags & MAP_ANONYMOUS) == 0 {
        return Err(SysError::Einval);
    }

    if (flags & MAP_FIXED) != 0 {
        return Err(SysError::Einval);
    }

    if (prot & (PROT_READ | PROT_WRITE | PROT_EXEC)) == 0 {
        return Err(SysError::Einval);
    }

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
