// src/process/elf_loader.rs

use x86_64::{
    structures::paging::PageTableFlags,
    VirtAddr,
    registers::control::Cr3,
};
use xmas_elf::{ElfFile, program::Type};
use crate::memory::address_space::AddressSpace;
use crate::memory::memory_manager::{MemoryManager, Oom};
use crate::memory::vma::VmaKind;
use crate::debugln; // Ensure we print to VGA/Serial

pub fn load_elf(
    elf_bytes: &[u8],
    space: &mut AddressSpace,
    mm: &mut MemoryManager,
) -> Result<VirtAddr, Oom> {
    debugln!("[ELF] Parsing ELF headers...");
    let elf = ElfFile::new(elf_bytes).map_err(|_| Oom)?;

    debugln!("[ELF] Switching CR3 to target address space...");
    let (current_cr3, cr3_flags) = Cr3::read();
    let target_cr3 = space.cr3();
    
    if current_cr3 != target_cr3 {
        unsafe { Cr3::write(target_cr3, cr3_flags); }
    }

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
        let total_size = offset_in_page + mem_size;
        let page_aligned_size = (total_size + 0xFFF) & !0xFFF;

        let start = VirtAddr::new_truncate(page_start);
        let size = page_aligned_size as usize;

        let mut pt_flags = PageTableFlags::PRESENT 
                         | PageTableFlags::WRITABLE 
                         | PageTableFlags::USER_ACCESSIBLE;
        
        if !flags.is_execute() {
            pt_flags |= PageTableFlags::NO_EXECUTE;
        }

        let kind = if flags.is_execute() { VmaKind::Code } else { VmaKind::Data };

        debugln!("[ELF] Mapping segment at {:#x} (size: {})...", start.as_u64(), size);
        mm.mmap_at(space, start, size, pt_flags, kind)?;
        debugln!("[ELF] Mapped successfully. Copying {} bytes...", file_size);

        if file_size > 0 {
            let src = &elf_bytes[offset as usize .. (offset + file_size) as usize];
            let dst = VirtAddr::new_truncate(virt_addr).as_mut_ptr::<u8>();
            unsafe {
                // If it hangs here, it's a Page Fault deadlock!
                core::ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len());
            }
            debugln!("[ELF] Copy complete.");
        }
        
        if mem_size > file_size {
            debugln!("[ELF] Zeroing BSS...");
            let bss_start = VirtAddr::new_truncate(virt_addr + file_size).as_mut_ptr::<u8>();
            let bss_size = (mem_size - file_size) as usize;
            unsafe {
                core::ptr::write_bytes(bss_start, 0, bss_size);
            }
        }
    }

    debugln!("[ELF] Restoring original CR3...");
    if current_cr3 != target_cr3 {
        unsafe { Cr3::write(current_cr3, cr3_flags); }
    }

    let entry_point = elf.header.pt2.entry_point();
    debugln!("[ELF] Load complete! Entry point: {:#x}", entry_point);
    Ok(VirtAddr::new_truncate(entry_point))
}