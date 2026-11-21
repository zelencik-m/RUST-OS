use crate::memory::address_space::{is_user_range, USER_SPACE_END, USER_SPACE_START};
use crate::process::initial_stack::MAX_ARGS;
use crate::process::process_manager::{
    exit_current_thread, exit_if_killed, get_current_brk, spawn_process, sys_brk,
    sys_mmap_anonymous, sys_munmap, wait_child, SpawnArgs, SpawnError, WaitError,
    PROCESS_MANAGER,
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
use crate::fs::{with_vfs, File, FileType, FsError, InodeRef, OpenFlags, Path, SeekWhence};
use alloc::string::String;
use alloc::vec::Vec;
use x86_64::structures::paging::PageTableFlags;
use x86_64::VirtAddr;

const PATH_MAX: usize = 4096;

/// Largest program file `spawn` loads. Programs are read whole into the
/// kernel heap (10 MiB) before they are loaded.
const MAX_PROGRAM_SIZE: u64 = 4 * 1024 * 1024;

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
        // Like Linux, only the low 8 bits of the status survive.
        exit_current_thread(tid, (frame.rdi & 0xff) as i32);
    }

    frame.rax = dispatch(pid, tid, frame);

    // Safe point for a pending kill (Ctrl-C), on the way back to user mode.
    // A blocking syscall cut short by the kill returns EINTR first.
    exit_if_killed(tid);
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
        SYS_TTY_MODE => ret(sys_tty_mode(pid, args[0], args[1])),
        SYS_TTY_FOREGROUND => ret(sys_tty_foreground(pid, args[0], args[1])),
        SYS_SPAWN => ret(sys_spawn(pid, args[0], args[1])),
        SYS_WAIT4 => ret(sys_wait4(pid, args[0], args[1], args[2], args[3])),
        SYS_GETPID => pid.0,
        SYS_GETTID => tid.0,
        SYS_MKDIR => ret(sys_mkdir(pid, args[0], args[1])),
        SYS_RMDIR => ret(sys_rmdir(pid, args[0])),
        SYS_UNLINK => ret(sys_unlink(pid, args[0])),
        SYS_GETCWD => ret(sys_getcwd(pid, args[0], args[1])),
        SYS_CHDIR => ret(sys_chdir(pid, args[0])),
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

/// `read(fd, buf, len)`.
///
/// Regular files are read in 512-byte chunks while holding PROCESS_MANAGER
/// (the fd's offset lives in the fd table). Device fds (the console) are
/// different, because `tty::read` can block for as long as nobody types.
/// That lock keeps interrupts off, so blocking while holding it would
/// deadlock the scheduler. See `read_device`.
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

    let device = {
        let mut pm = PROCESS_MANAGER.lock();
        let file = pm
            .process_mut(pid)
            .file_mut(fd)
            .map_err(SysError::from_fs_error)?;
        let inode = file.inode();
        if inode.file_type() == FileType::Device {
            // Same check (and errno) as `File::read`, which this path skips.
            if !file.flags().can_read() {
                return Err(SysError::from_fs_error(FsError::ReadOnly));
            }
            Some(inode)
        } else {
            None
        }
    };
    if let Some(inode) = device {
        return read_device(pid, inode, src, size);
    }

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

/// Read from a device with no locks held: the caller has already released
/// PROCESS_MANAGER and kept only the inode. Devices have no file offset.
///
/// Exactly one `read_at` (of at most 512 bytes). A terminal returns as soon
/// as *some* input is there (a line, or raw bytes). Looping for more, the way
/// the regular-file path does after a full chunk, would block again even
/// though bytes were already returned.
fn read_device(
    pid: PID,
    inode: InodeRef,
    destination: VirtAddr,
    size: usize,
) -> Result<u64, SysError> {
    let mut buffer = [0u8; 512];
    let chunk = core::cmp::min(buffer.len(), size);
    let n = inode
        .read_at(0, &mut buffer[..chunk])
        .map_err(SysError::from_fs_error)?;
    copy_to_user(pid, destination, &buffer[..n])?;
    Ok(n as u64)
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

/// `EBADF` if `fd` isn't open, `ENOTTY` if it isn't the console.
fn require_terminal(pid: PID, fd: u64) -> Result<(), SysError> {
    let is_terminal = PROCESS_MANAGER
        .lock()
        .process_mut(pid)
        .file_mut(fd)
        .map_err(SysError::from_fs_error)?
        .inode()
        .is_terminal();
    if is_terminal {
        Ok(())
    } else {
        Err(SysError::Enotty)
    }
}

/// `tty_mode(fd, mode)`: switch the console between cooked (0) and raw (1)
/// and return the previous mode. `ENOTTY` if `fd` is not the console.
fn sys_tty_mode(pid: PID, fd: u64, raw_mode: u64) -> Result<u64, SysError> {
    require_terminal(pid, fd)?;
    let mode = crate::tty::TtyMode::from_raw(raw_mode).ok_or(SysError::Einval)?;
    Ok(crate::tty::set_mode(mode).as_raw())
}

/// `tty_foreground(fd, pid)`: make `pid` the process a cooked-mode Ctrl-C
/// kills (0: nobody), and return the previous one (0: nobody). Like Linux's
/// `tcsetpgrp`, minus process groups.
///
/// `fd` must be the console (`EBADF`/`ENOTTY`). `pid` must be one of the
/// caller's children (`ESRCH`), so a program can't make Ctrl-C kill an
/// unrelated process. `wait4` clears it again once that child is collected.
fn sys_tty_foreground(pid: PID, fd: u64, target: u64) -> Result<u64, SysError> {
    require_terminal(pid, fd)?;
    let target = if target == 0 {
        None
    } else {
        let pm = PROCESS_MANAGER.lock();
        match pm.try_process(PID(target)) {
            Some(child) if child.parent() == Some(pid) => Some(PID(target)),
            _ => return Err(SysError::Esrch),
        }
    };
    Ok(crate::tty::set_foreground(target).map(|p| p.0).unwrap_or(0))
}

/// `spawn(path, argv)`: start the program at `path` (resolved against the
/// cwd) as a child process, and return the child's pid. A rust-os syscall,
/// the shell's replacement for `fork` + `execve`.
///
/// - `argv` is a NULL-terminated array of string pointers, like `execve`'s.
///   It becomes the child's `argc`/`argv` as given (pass the program name
///   as `argv[0]`). A NULL `argv` means no arguments (`argc` 0).
/// - The child inherits the cwd and fds 0–2. Each one is the same open file
///   (`File::share`), not a copy, so output written by parent and child
///   lands in order. Closed fds stay closed.
/// - Errors: `ENOENT`/`ENOTDIR` (path), `EACCES` (not a regular file),
///   `ENOEXEC` (not an x86_64 ELF), `E2BIG` (more than `MAX_ARGS`
///   arguments, or too large for the initial stack), `EFAULT` (bad pointer),
///   `EINVAL` (an argument has no NUL within `PATH_MAX`, or isn't UTF-8),
///   `ENOMEM`.
fn sys_spawn(pid: PID, user_path: u64, user_argv: u64) -> Result<u64, SysError> {
    let raw_path = copy_c_string_from_user(pid, user_path, PATH_MAX)?;
    let args = copy_argv_from_user(pid, user_argv)?;
    let cwd = process_cwd(pid);
    let path = Path::resolve(&cwd, &raw_path).map_err(SysError::from_fs_error)?;
    let elf = read_program(&path)?;

    let stdio = {
        let pm = PROCESS_MANAGER.lock();
        let process = pm.process(pid);
        [0usize, 1, 2].map(|fd| {
            process
                .fd_table
                .get(fd)
                .and_then(Option::as_ref)
                .map(File::share)
        })
    };

    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let (child, _) = spawn_process(SpawnArgs {
        parent: pid,
        name: String::from(path.file_name().unwrap_or("/")),
        elf: &elf,
        argv: &argv,
        cwd,
        stdio,
    })
    .map_err(|error| match error {
        SpawnError::Oom => SysError::Enomem,
        SpawnError::ArgsTooLarge => SysError::E2big,
    })?;
    Ok(child.0)
}

/// Copy a NULL-terminated `char *argv[]` out of user memory.
fn copy_argv_from_user(pid: PID, user_argv: u64) -> Result<Vec<String>, SysError> {
    let mut args = Vec::new();
    if user_argv == 0 {
        return Ok(args);
    }
    loop {
        let slot = user_argv
            .checked_add(8 * args.len() as u64)
            .ok_or(SysError::Efault)?;
        let mut pointer = [0u8; 8];
        copy_from_user(pid, validate_user_range(slot, 8)?, &mut pointer)?;
        let pointer = u64::from_le_bytes(pointer);
        if pointer == 0 {
            return Ok(args);
        }
        if args.len() == MAX_ARGS {
            return Err(SysError::E2big);
        }
        args.push(copy_c_string_from_user(pid, pointer, PATH_MAX)?);
    }
}

/// Read a whole program file for `spawn`, checking that it is a regular
/// file and an x86_64 ELF.
fn read_program(path: &Path) -> Result<Vec<u8>, SysError> {
    let root = Path::root();
    // The outer Result is the VFS's, the inner one is spawn's own checks.
    let bytes = with_vfs(|vfs| -> Result<Result<Vec<u8>, SysError>, FsError> {
        let stat = vfs.stat(&root, path.as_str())?;
        if stat.file_type != FileType::Regular {
            return Ok(Err(SysError::Eacces));
        }
        if stat.size > MAX_PROGRAM_SIZE {
            return Ok(Err(SysError::Enomem));
        }
        let mut file = vfs.open(&root, path.as_str(), OpenFlags::read_only())?;
        let mut bytes = alloc::vec![0u8; stat.size as usize];
        let mut filled = 0;
        while filled < bytes.len() {
            let n = file.read(&mut bytes[filled..])?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        bytes.truncate(filled);
        Ok(Ok(bytes))
    })
    .map_err(SysError::from_fs_error)??;

    // e_ident: magic, ELFCLASS64, little-endian; e_machine: x86_64.
    let is_x86_64_elf = bytes.len() >= 20
        && bytes[..4] == *b"\x7fELF"
        && bytes[4] == 2
        && bytes[5] == 1
        && u16::from_le_bytes([bytes[18], bytes[19]]) == 0x3e;
    if !is_x86_64_elf {
        return Err(SysError::Enoexec);
    }
    Ok(bytes)
}

/// `wait4(pid, status, options, rusage)`: wait for child `pid` to exit,
/// collect it, and return `pid`.
///
/// Differences from Linux:
/// - Only `pid > 0` (one specific child) is supported. `-1` (any child) and
///   process groups give `EINVAL`, as does any `options` flag.
/// - `*status` (an `int`, skipped if `status` is NULL) receives the plain
///   exit code (0–255; 130 after Ctrl-C, 139 after a fault), not Linux's
///   encoded wait status. There are no `WEXITSTATUS` macros to decode.
/// - `rusage` is ignored.
///
/// `ECHILD` if `pid` isn't a child of the caller (or was already
/// collected). `EINTR` if the caller is killed while waiting.
fn sys_wait4(
    pid: PID,
    child: u64,
    user_status: u64,
    options: u64,
    _rusage: u64,
) -> Result<u64, SysError> {
    if (child as i64) <= 0 || options != 0 {
        return Err(SysError::Einval);
    }
    let status = match user_status {
        0 => None,
        address => Some(validate_user_range(address, 4)?),
    };

    let child = PID(child);
    let code = wait_child(pid, child).map_err(|error| match error {
        WaitError::NotChild => SysError::Echild,
        WaitError::Interrupted => SysError::Eintr,
    })?;
    crate::tty::clear_foreground_if(child);

    if let Some(address) = status {
        copy_to_user(pid, address, &code.to_le_bytes())?;
    }
    Ok(child.0)
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

fn sys_rmdir(pid: PID, user_path: u64) -> Result<u64, SysError> {
    let path = copy_c_string_from_user(pid, user_path, PATH_MAX)?;
    let cwd = process_cwd(pid);

    with_vfs(|vfs| vfs.rmdir(&cwd, &path)).map_err(SysError::from_fs_error)?;
    Ok(0)
}

fn sys_unlink(pid: PID, user_path: u64) -> Result<u64, SysError> {
    let path = copy_c_string_from_user(pid, user_path, PATH_MAX)?;
    let cwd = process_cwd(pid);

    with_vfs(|vfs| vfs.unlink(&cwd, &path)).map_err(SysError::from_fs_error)?;
    Ok(0)
}

/// Linux `getcwd(buf, size)`: copy the cwd plus a NUL into `buf` and return
/// the number of bytes written (including the NUL). A buffer too small for
/// the whole path gives `ERANGE`. This check comes before the pointer check,
/// as in Linux, so `getcwd(NULL, 0)` is `ERANGE` and not `EFAULT`.
fn sys_getcwd(pid: PID, user_buf: u64, size: u64) -> Result<u64, SysError> {
    let size = usize_from_u64(size)?;
    let cwd = process_cwd(pid);

    let mut bytes = Vec::with_capacity(cwd.as_str().len() + 1);
    bytes.extend_from_slice(cwd.as_str().as_bytes());
    bytes.push(0);
    if bytes.len() > size {
        return Err(SysError::Erange);
    }

    let destination = validate_user_range(user_buf, bytes.len())?;
    copy_to_user(pid, destination, &bytes)?;
    Ok(bytes.len() as u64)
}

/// Linux `chdir(path)`: make `path` (resolved against the current cwd) the
/// process's working directory.
///
/// PROCESS_MANAGER is held only briefly, to read and then to set the cwd.
/// The VFS lookup in between may do disk I/O, and PM is an IRQ-safe lock
/// (interrupts are off while it is held). The cwd is set only after every
/// check has passed, so a failed chdir leaves it unchanged.
fn sys_chdir(pid: PID, user_path: u64) -> Result<u64, SysError> {
    let raw = copy_c_string_from_user(pid, user_path, PATH_MAX)?;
    let cwd = process_cwd(pid);
    let path = Path::resolve(&cwd, &raw).map_err(SysError::from_fs_error)?;

    let stat = with_vfs(|vfs| vfs.stat(&cwd, path.as_str()))
        .map_err(SysError::from_fs_error)?;
    if stat.file_type != FileType::Directory {
        return Err(SysError::Enotdir);
    }

    PROCESS_MANAGER.lock().process_mut(pid).set_cwd(path);
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
