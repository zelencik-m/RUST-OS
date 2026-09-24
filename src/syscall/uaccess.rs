use alloc::string::String;
use alloc::vec::Vec;

use crate::memory::address_space::is_user_range;
use crate::memory::memory_manager::{FaultAction, MEMORY_MANAGER};
use crate::memory::vma::VmaKind;
use crate::process::process_manager::PROCESS_MANAGER;
use crate::process::PID;
use crate::syscall::errors::SysError;
use x86_64::structures::paging::PageTableFlags;
use x86_64::VirtAddr;

/// Convert a raw u64 into a canonical VirtAddr.
///
/// We manually check canonicality because `VirtAddr::new()` panics on
/// non-canonical addresses, and user input must never panic the kernel.
pub fn canonical_user_addr(addr: u64) -> Result<VirtAddr, SysError> {
    let high = addr >> 48;
    if high != 0 && high != 0xffff {
        return Err(SysError::Efault);
    }
    Ok(VirtAddr::new(addr))
}

/// Validate a user address range.
pub fn validate_user_range(start: u64, size: usize) -> Result<VirtAddr, SysError> {
    let addr = canonical_user_addr(start)?;
    if size == 0 {
        return Ok(addr);
    }
    let _end = start
        .checked_add(size as u64 - 1)
        .ok_or(SysError::Efault)?;
    if !is_user_range(addr, size as u64) {
        return Err(SysError::Efault);
    }
    Ok(addr)
}

/// Copy from user memory into a kernel buffer.
pub fn copy_from_user(
    pid: PID,
    src: VirtAddr,
    dst: &mut [u8],
) -> Result<(), SysError> {
    copy_user_internal(pid, src, dst.as_mut_ptr(), dst.len(), false)
}

/// Copy from a kernel buffer into user memory.
pub fn copy_to_user(
    pid: PID,
    dst: VirtAddr,
    src: &[u8],
) -> Result<(), SysError> {
    copy_user_internal(pid, dst, src.as_ptr() as *mut u8, src.len(), true)
}

/// Copy a NUL-terminated string from userspace.
///
/// Paths are infrequent enough that the byte-at-a-time implementation is a
/// worthwhile simplicity tradeoff here: it stops exactly at NUL and never
/// speculatively reads the page after the string.
pub fn copy_c_string_from_user(
    pid: PID,
    src: u64,
    max_len: usize,
) -> Result<String, SysError> {
    if max_len == 0 {
        return Err(SysError::Einval);
    }

    let base = canonical_user_addr(src)?;
    let mut bytes = Vec::new();

    for i in 0..max_len {
        let mut byte = [0u8; 1];
        copy_from_user(pid, base + i as u64, &mut byte)?;

        if byte[0] == 0 {
            return String::from_utf8(bytes).map_err(|_| SysError::Einval);
        }

        bytes.push(byte[0]);
    }

    // No terminator within the allowed path length.
    Err(SysError::Einval)
}

fn copy_user_internal(
    pid: PID,
    addr: VirtAddr,
    kernel_buffer: *mut u8,
    len: usize,
    write_to_user: bool,
) -> Result<(), SysError> {
    if len == 0 {
        return Ok(());
    }

    let addr = canonical_user_addr(addr.as_u64())?;
    if !is_user_range(addr, len as u64) {
        return Err(SysError::Efault);
    }

    let mut offset = 0usize;

    // Existing kernel convention: ProcessManager -> MemoryManager.
    let mut pm = PROCESS_MANAGER.lock();
    let mut mm_guard = MEMORY_MANAGER.lock();
    let mm = mm_guard
        .as_mut()
        .ok_or(SysError::Enomem)?;

    let proc = pm.process_mut(pid);
    let space = proc
        .address_space
        .as_mut()
        .ok_or(SysError::Efault)?;

    while offset < len {
        let cur = addr + offset as u64;
        let page_offset = cur.as_u64() & 0xFFF;
        let chunk = core::cmp::min(
            (4096 - page_offset) as usize,
            len - offset,
        );

        if space.translate(mm.physical_memory_offset, cur).is_none() {
            match mm.handle_page_fault(space, cur) {
                FaultAction::Resolved => {}
                FaultAction::Guard | FaultAction::Segfault => {
                    return Err(SysError::Efault);
                }
                FaultAction::Oom => {
                    return Err(SysError::Enomem);
                }
            }
        }

        let vma = space
            .get_vma(cur)
            .ok_or(SysError::Efault)?;

        if vma.kind == VmaKind::Guard {
            return Err(SysError::Efault);
        }
        if !vma.flags.contains(PageTableFlags::USER_ACCESSIBLE) {
            return Err(SysError::Efault);
        }
        if write_to_user && !vma.flags.contains(PageTableFlags::WRITABLE) {
            return Err(SysError::Efault);
        }

        let phys = space
            .translate(mm.physical_memory_offset, cur)
            .ok_or(SysError::Efault)?;

        let user_ptr = (mm.physical_memory_offset + phys.as_u64())
            .as_mut_ptr::<u8>();

        unsafe {
            if write_to_user {
                core::ptr::copy_nonoverlapping(
                    kernel_buffer.add(offset),
                    user_ptr,
                    chunk,
                );
            } else {
                core::ptr::copy_nonoverlapping(
                    user_ptr,
                    kernel_buffer.add(offset),
                    chunk,
                );
            }
        }

        offset += chunk;
    }

    Ok(())
}
