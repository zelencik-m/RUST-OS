use x86_64::{
    structures::paging::{Page, PageTableFlags, Size4KiB},
    VirtAddr,
};
use xmas_elf::{program::Type, ElfFile};

use crate::debugln;
use crate::memory::address_space::AddressSpace;
use crate::memory::memory_manager::{MemoryManager, Oom};
use crate::memory::vma::VmaKind;

pub fn load_elf(
    elf_bytes: &[u8],
    space: &mut AddressSpace,
    mm: &mut MemoryManager,
) -> Result<VirtAddr, Oom> {
    debugln!("[ELF] Parsing ELF headers...");

    let elf = ElfFile::new(elf_bytes).map_err(|_| Oom)?;

    debugln!("[ELF] Iterating segments...");

    for ph in elf.program_iter() {
        let p_type = match ph.get_type() {
            Ok(t) => t,
            Err(_) => continue,
        };

        if p_type != Type::Load {
            continue;
        }

        let virt_addr = ph.virtual_addr();
        let mem_size = ph.mem_size();
        let file_size = ph.file_size();
        let offset = ph.offset();
        let flags = ph.flags();

        if mem_size == 0 {
            continue;
        }

        let page_start = virt_addr & !0xFFF;
        let offset_in_page = virt_addr & 0xFFF;

        let total_size = offset_in_page
            .checked_add(mem_size)
            .ok_or(Oom)?;

        let page_aligned_size = total_size
            .checked_add(0xFFF)
            .ok_or(Oom)?
            & !0xFFF;

        let start = VirtAddr::new_truncate(page_start);
        let size = page_aligned_size as usize;

        let mut pt_flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::USER_ACCESSIBLE;

        if !flags.is_execute() {
            pt_flags |= PageTableFlags::NO_EXECUTE;
        }

        let kind = if flags.is_execute() {
            VmaKind::Code
        } else {
            VmaKind::Data
        };

        debugln!(
            "[ELF] Mapping segment at {:#x} (size: {} bytes)...",
            start.as_u64(),
            size
        );

        mm.mmap_at(space, start, size, pt_flags, kind)?;

        if file_size > 0 {
            let src_start = offset as usize;
            let src_end = src_start
                .checked_add(file_size as usize)
                .ok_or(Oom)?;

            if src_end > elf_bytes.len() {
                return Err(Oom);
            }

            let src = &elf_bytes[src_start..src_end];
            let dst = VirtAddr::new_truncate(virt_addr);

            debugln!(
                "[ELF] Copying {} bytes to {:#x} via direct map...",
                src.len(),
                dst.as_u64()
            );

            copy_to_address_space(
                space,
                mm.physical_memory_offset,
                dst,
                src,
            )?;
        }

        if mem_size > file_size {
            let bss_addr = virt_addr
                .checked_add(file_size)
                .ok_or(Oom)?;

            let bss_start = VirtAddr::new_truncate(bss_addr);
            let bss_size = (mem_size - file_size) as usize;

            debugln!(
                "[ELF] Zeroing BSS at {:#x} ({} bytes)...",
                bss_start.as_u64(),
                bss_size
            );

            zero_in_address_space(
                space,
                mm.physical_memory_offset,
                bss_start,
                bss_size,
            )?;
        }
    }

    let entry_point = elf.header.pt2.entry_point();

    debugln!(
        "[ELF] Load complete! Entry point: {:#x}",
        entry_point
    );

    Ok(VirtAddr::new_truncate(entry_point))
}

/// Copy bytes into a target address space without switching CR3.
///
/// The destination virtual address is translated through the target address
/// space's page tables, then written via the kernel physical-memory direct map.
fn copy_to_address_space(
    space: &AddressSpace,
    physical_memory_offset: VirtAddr,
    dst: VirtAddr,
    mut src: &[u8],
) -> Result<(), Oom> {
    let mut dst = dst;

    while !src.is_empty() {
        let page = Page::<Size4KiB>::containing_address(dst);
        let page_offset = dst.as_u64() - page.start_address().as_u64();

        let chunk = core::cmp::min(
            (4096u64 - page_offset) as usize,
            src.len(),
        );

        let phys = space
            .translate(physical_memory_offset, dst)
            .ok_or(Oom)?;

        let kernel_dst = physical_memory_offset + phys.as_u64();
        let kernel_dst_ptr = kernel_dst.as_mut_ptr::<u8>();

        unsafe {
            core::ptr::copy_nonoverlapping(
                src.as_ptr(),
                kernel_dst_ptr,
                chunk,
            );
        }

        dst = dst + chunk as u64;
        src = &src[chunk..];
    }

    Ok(())
}

/// Zero bytes in a target address space without switching CR3.
///
/// The destination virtual address is translated through the target address
/// space's page tables, then zeroed via the kernel physical-memory direct map.
fn zero_in_address_space(
    space: &AddressSpace,
    physical_memory_offset: VirtAddr,
    dst: VirtAddr,
    mut len: usize,
) -> Result<(), Oom> {
    let mut dst = dst;

    while len > 0 {
        let page = Page::<Size4KiB>::containing_address(dst);
        let page_offset = dst.as_u64() - page.start_address().as_u64();

        let chunk = core::cmp::min(
            (4096u64 - page_offset) as usize,
            len,
        );

        let phys = space
            .translate(physical_memory_offset, dst)
            .ok_or(Oom)?;

        let kernel_dst = physical_memory_offset + phys.as_u64();
        let kernel_dst_ptr = kernel_dst.as_mut_ptr::<u8>();

        unsafe {
            core::ptr::write_bytes(kernel_dst_ptr, 0, chunk);
        }

        dst = dst + chunk as u64;
        len -= chunk;
    }

    Ok(())
}
