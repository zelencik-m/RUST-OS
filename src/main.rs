// src/main.rs

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(rust_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;
use rust_os::{hlt_loop, print, serial_println, debugln};

use bootloader::{BootInfo, entry_point};

extern crate alloc;

entry_point!(kernel_main);


use rust_os::process::process_manager::PROCESS_MANAGER;
use rust_os::scheduler::scheduler::{sleep, yield_now};
use rust_os::process::process_manager;
use x86_64::instructions::interrupts;
use x86_64::structures::paging::PageTableFlags;
use x86_64::VirtAddr;
use rust_os::memory::vma::VmaKind; // Make sure this path matches your VmaKind import
use rust_os::process::PID;

use rust_os::fs::{
    with_vfs,
    OpenFlags,
    SeekWhence,
    FileType,
};

// src/main.rs (inside `kernel_main`)
fn kernel_main(boot_info: &'static BootInfo) -> ! {
    rust_os::init(boot_info);

    interrupts::without_interrupts(|| {
        debugln!(">>> kernel_main: creating user process");
        let upid = process_manager::create_user_process("test".into());
        debugln!(">>> created user process PID({:?})", upid.0);

        debugln!(">>> creating thread_a");
        let tid_a = process_manager::create_thread(upid, thread_a);
        debugln!(">>> thread_a created TID({:?})", tid_a.0);
        
        debugln!(">>> creating thread_b");
        let tid_b = process_manager::create_thread(upid, thread_b);
        debugln!(">>> thread_b created TID({:?})", tid_b.0);

        debugln!(">>> creating thread_c");
        let tid_c = process_manager::create_thread(upid, thread_c);
        debugln!(">>> thread_c created TID({:?})", tid_c.0);

        debugln!(">>> creating ring3_exception_test thread");
        let tid_exc = process_manager::create_thread(PID(0), ring3_exception_test);
        debugln!(">>> ring3_exception_test created TID({:?})", tid_exc.0);

        debugln!(">>> creating elf_loader_test thread");
        let tid_elf = process_manager::create_thread(PID(0), elf_loader_test);
        debugln!(">>> elf_loader_test created TID({:?})", tid_elf.0);

        debugln!(">>> creating syscall_test thread");
        let tid_sys = process_manager::create_thread(PID(0), syscall_test);
        debugln!(">>> syscall_test created TID({:?})", tid_sys.0);

        debugln!(">>> creating vfs_test thread");
        let tid_sys = process_manager::create_thread(PID(0), vfs_test);
        debugln!(">>> vfs_test created TID({:?})", tid_sys.0);

        debugln!(">>> entering hlt_loop");
    });
    rust_os::hlt_loop();
}

extern "C" fn ring3_test() -> ! {
    use rust_os::memory::memory_manager::MEMORY_MANAGER;
    use rust_os::interrupts::gdt::{get_selectors, set_tss_rsp0};
    use x86_64::structures::paging::{Mapper, Page, Size4KiB, PageTableFlags};
    use x86_64::VirtAddr;
    use core::arch::asm;
    use x86_64::structures::paging::FrameAllocator;

    debugln!("[ring3_test] starting...");

    let mut mm_guard = MEMORY_MANAGER.lock();
    let mm = mm_guard.as_mut().unwrap();

    // 16 TB -> P4 index 32, a slot the bootloader never touched, so
    // map_to_with_table_flags creates ALL levels fresh with USER_ACCESSIBLE.
    let virt_addr = VirtAddr::new(0x0000_1000_0000_0000);
    let page = Page::<Size4KiB>::containing_address(virt_addr);
    let frame = mm.frame_allocator.allocate_frame().expect("frame alloc failed");

    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;
    let parent_flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;

    unsafe {
        mm.mapper
            .map_to_with_table_flags(page, frame, flags, parent_flags, &mut mm.frame_allocator)
            .expect("map failed")
            .flush();
    }

    // Infinite loop: jmp -2
    let ptr = virt_addr.as_mut_ptr::<u8>();
    unsafe {
        *ptr.offset(0) = 0xEB;
        *ptr.offset(1) = 0xFE;
    }

    let selectors = get_selectors();
    let cs = selectors.user_code_selector.0 as u64 | 3;
    let ss = selectors.user_data_selector.0 as u64 | 3;
    let user_rsp = virt_addr.as_u64() + 4096;

    let mut current_rsp: u64;
    unsafe { asm!("mov {}, rsp", out(reg) current_rsp); }
    unsafe { set_tss_rsp0(VirtAddr::new(current_rsp)); }

    drop(mm_guard); // never hold locks into Ring 3

    unsafe {
        asm!(
            "mov ds, {ds:e}",
            "mov es, {ds:e}",
            "push {ss}",
            "push {rsp}",
            "push {rflags}",
            "push {cs}",
            "push {rip}",
            "iretq",
            ds = in(reg) ss,
            ss = in(reg) ss,
            rsp = in(reg) user_rsp,
            rflags = in(reg) 0x202u64,
            cs = in(reg) cs,
            rip = in(reg) virt_addr.as_u64(),
            options(noreturn)
        );
    }
}

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    x86_64::instructions::interrupts::without_interrupts(|| {
        serial_println!("{}", info);
    });
    hlt_loop();
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    rust_os::test_panic_handler(info)
}

extern "C" fn thread_a() -> ! {
    use rust_os::scheduler::scheduler::SCHEDULER;

    debugln!("[thread_a] starting demand paging checkpoint...");

    let tid = SCHEDULER.lock().get_current().expect("no current thread");
    let pid = PROCESS_MANAGER.lock().thread(tid).get_owner();

    let test_addr =
        VirtAddr::new(rust_os::memory::address_space::USER_SPACE_START);

    process_manager::register_vma_only(
        pid,
        test_addr,
        4096,
        PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
        VmaKind::User,
    );

    unsafe {
        let p = test_addr.as_mut_ptr::<u64>();
        *p = 0xCAFEBABE;
        assert_eq!(*p, 0xCAFEBABE);
    }

    debugln!("[thread_a] in-bounds demand paging test passed!");

    // Unmapped, but still inside the private user P4 slot.
    let bad_addr = VirtAddr::new(
        rust_os::memory::address_space::USER_SPACE_START + 0x1000_000,
    );

    unsafe {
        let p = bad_addr.as_mut_ptr::<u64>();
        *p = 1;
    }

    debugln!("[thread_a] this line should never print");

    loop {
        yield_now();
    }
}

extern "C" fn thread_b() -> ! {
    let mut counter: u64 = 100000000;
    let mut subtracting = true;
    loop {
        serial_println!("executing thread B (counter={})", counter);
        loop {
            if subtracting {
                if counter == 0 {
                    subtracting = false;
                    counter += 1;
                } else {
                    counter -= 1;
                }
            } else {
                counter += 1;
                if counter == 100000000 {
                    subtracting = true;
                }
            }
            if counter % 100 == 0 {
                break;
            }
        }
        sleep(3);
    }
}
extern "C" fn thread_c() -> ! {
    let mut counter: u64 = 0;
    loop {
        serial_println!("executing thread C (counter={})", counter);
        loop{
            // serial_println!("thread A (counter={})", counter);
            counter += 1;
            if counter % 100 == 0 {
                break;
            }
        }
        sleep(10);
    }
}

extern "C" fn ring3_exception_test() -> ! {
    use rust_os::memory::memory_manager::MEMORY_MANAGER;
    use rust_os::interrupts::gdt::{get_selectors, set_tss_rsp0};
    use x86_64::structures::paging::{Mapper, Page, Size4KiB, PageTableFlags};
    use x86_64::VirtAddr;
    use core::arch::asm;
    use x86_64::structures::paging::FrameAllocator;

    debugln!("[ring3_exception_test] starting...");

    let mut mm_guard = MEMORY_MANAGER.lock();
    let mm = mm_guard.as_mut().unwrap();

    // Map a single user-accessible page
    let virt_addr = VirtAddr::new(0x0000_1000_0000_0000);
    let page = Page::<Size4KiB>::containing_address(virt_addr);
    let frame = mm.frame_allocator.allocate_frame().expect("frame alloc failed");

    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;
    let parent_flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;

    unsafe {
        mm.mapper
            .map_to_with_table_flags(page, frame, flags, parent_flags, &mut mm.frame_allocator)
            .expect("map failed")
            .flush();
    }

    let ptr = virt_addr.as_mut_ptr::<u8>();
    unsafe {
        // =========================================================
        // CHOOSE YOUR EXCEPTION BY UNCOMMENTING ONE OF THE OPTIONS
        // =========================================================

        // OPTION 1: Breakpoint Exception (#BP) - RECOVERABLE
        // Triggers `int 3`. Your handler will print the stack frame 
        // and return to Ring 3. We use `jmp +1` to skip the `nop` 
        // so it only traps ONCE, then enters an infinite loop.
        /*
        *ptr.offset(0) = 0xCC; // int 3
        *ptr.offset(1) = 0xEB; // jmp rel8 +1
        *ptr.offset(2) = 0x01; // (skip 1 byte)
        *ptr.offset(3) = 0x90; // nop (skipped)
        *ptr.offset(4) = 0xEB; // jmp -2 (infinite loop)
        *ptr.offset(5) = 0xFE; 
        */

        // OPTION 2: Page Fault (#PF) - FATAL
        // Loads 0x0000_0000_2000_0000 into RAX (Canonical, but unmapped!)
        *ptr.offset(0) = 0x48; *ptr.offset(1) = 0xB8; // mov rax, imm64
        *ptr.offset(2) = 0x00; *ptr.offset(3) = 0x00; *ptr.offset(4) = 0x00; *ptr.offset(5) = 0x20;
        *ptr.offset(6) = 0x00; *ptr.offset(7) = 0x00; *ptr.offset(8) = 0x00; *ptr.offset(9) = 0x00; 
        *ptr.offset(10) = 0x48; *ptr.offset(11) = 0x89; *ptr.offset(12) = 0x00; // mov [rax], rax -> TRAPS HERE with #PF!
        *ptr.offset(13) = 0xEB; *ptr.offset(14) = 0xFE; // jmp -2 (fallback)

        // OPTION 3: General Protection Fault (#GP) or Invalid Opcode (#UD)
        // WARNING: You don't have IDT handlers for these yet! 
        // They will trigger a Double Fault and panic the kernel.
        // *ptr.offset(0) = 0xF4;       // hlt (Privileged -> #GP)
        // *ptr.offset(0) = 0x0F; *ptr.offset(1) = 0x0B; // ud2 (Invalid Opcode -> #UD)
    }

    let selectors = get_selectors();
    let cs = selectors.user_code_selector.0 as u64 | 3;
    let ss = selectors.user_data_selector.0 as u64 | 3;
    let user_rsp = virt_addr.as_u64() + 4096;

    // Save current kernel RSP into TSS RSP0 so the CPU knows where to 
    // switch stacks when the Ring 3 exception occurs.
    let mut current_rsp: u64;
    unsafe { asm!("mov {}, rsp", out(reg) current_rsp); }
    unsafe { set_tss_rsp0(VirtAddr::new(current_rsp)); }

    drop(mm_guard);

    // Jump to Ring 3
    unsafe {
        asm!(
            "mov ds, {ds:e}",
            "mov es, {ds:e}",
            "push {ss}",
            "push {rsp}",
            "push {rflags}",
            "push {cs}",
            "push {rip}",
            "iretq",
            ds = in(reg) ss,
            ss = in(reg) ss,
            rsp = in(reg) user_rsp,
            rflags = in(reg) 0x202u64,
            cs = in(reg) cs,
            rip = in(reg) virt_addr.as_u64(),
            options(noreturn)
        );
    }
}

// 1. Define a wrapper struct that forces 8-byte alignment
#[repr(C, align(8))]
struct AlignedBytes([u8; include_bytes!("../user.elf").len()]);

// 2. Create the static aligned array
static ALIGNED_ELF: AlignedBytes = AlignedBytes(*include_bytes!("../user.elf"));

// 3. Create the slice pointing to the aligned data
static USER_ELF: &[u8] = &ALIGNED_ELF.0;
// src/main.rs (inside elf_loader_test)
extern "C" fn elf_loader_test() -> ! {
    use rust_os::memory::memory_manager::MEMORY_MANAGER;
    use rust_os::process::process_manager::PROCESS_MANAGER;
    use rust_os::process::process_manager::{create_user_process, create_user_thread};
    use rust_os::scheduler::scheduler::sleep;
    use rust_os::serial_println;
    use x86_64::structures::paging::PageTableFlags;
    use x86_64::VirtAddr;
    use rust_os::memory::vma::VmaKind;

    serial_println!("[elf_loader_test] Creating user process...");

    let upid = create_user_process("elf_test".into());

    let user_base = rust_os::memory::address_space::USER_SPACE_START + 0x100000;
    let code_addr = VirtAddr::new(user_base);
    let magic_addr = VirtAddr::new(user_base + 0x1000);

    {
        let mut pm_guard = PROCESS_MANAGER.lock();
        let proc = pm_guard.process_mut(upid);
        let space = proc.address_space.as_mut().unwrap();

        let mut mm_guard = MEMORY_MANAGER.lock();
        let mm = mm_guard.as_mut().unwrap();

        serial_println!("[elf_loader_test] Mapping code and magic pages...");

        let flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::USER_ACCESSIBLE;

        mm.mmap_at(space, code_addr, 4096, flags, VmaKind::Code).unwrap();
        mm.mmap_at(space, magic_addr, 4096, flags, VmaKind::Data).unwrap();

        serial_println!("[elf_loader_test] Writing code via direct map...");

        let code = [
            0xc7, 0x05, 0xf6, 0x0f, 0x00, 0x00,
            0xbe, 0xba, 0xfe, 0xca,
            0xeb, 0xfe,
        ];

        write_user_bytes(space, mm.physical_memory_offset, code_addr, &code);
    }

    serial_println!("[elf_loader_test] Creating user thread...");

    let utid = create_user_thread(upid, code_addr);

    serial_println!("[elf_loader_test] User thread created TID({:?})", utid.0);

    loop {
        sleep(50);

        let pm_guard = PROCESS_MANAGER.lock();
        let proc = pm_guard.process(upid);
        let space = proc.address_space.as_ref().unwrap();

        let mm_guard = MEMORY_MANAGER.lock();
        let mm = mm_guard.as_ref().unwrap();

        let val = read_user_u32(space, mm.physical_memory_offset, magic_addr);

        debugln!(
            "[elf_loader_test] Magic value at {:#x}: {:#x}",
            magic_addr.as_u64(),
            val
        );

        if val == 0xCAFEBABE {
            debugln!("[elf_loader_test] SUCCESS! User thread executed and wrote magic number!");
        }
    }
}

/// Write bytes into a user address space via the kernel's physical-memory
/// direct map, without switching CR3. Assumes the destination fits within a
/// single already-mapped page (fine for this small test).
fn write_user_bytes(
    space: &rust_os::memory::AddressSpace,
    physical_memory_offset: VirtAddr,
    dst: VirtAddr,
    bytes: &[u8],
) {
    let phys = space
        .translate(physical_memory_offset, dst)
        .expect("write_user_bytes: destination not mapped");

    let kernel_ptr = (physical_memory_offset + phys.as_u64()).as_mut_ptr::<u8>();

    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), kernel_ptr, bytes.len());
    }
}

/// Read a u32 from a user address space via the direct map, without
/// switching CR3.
fn read_user_u32(
    space: &rust_os::memory::AddressSpace,
    physical_memory_offset: VirtAddr,
    addr: VirtAddr,
) -> u32 {
    match space.translate(physical_memory_offset, addr) {
        Some(phys) => {
            let kernel_ptr = (physical_memory_offset + phys.as_u64()).as_ptr::<u32>();
            unsafe { core::ptr::read_volatile(kernel_ptr) }
        }
        None => 0, // page not yet mapped/demand-paged in
    }
}

extern "C" fn syscall_test() -> ! {
    use rust_os::memory::memory_manager::MEMORY_MANAGER;
    use rust_os::process::process_manager::PROCESS_MANAGER;
    use x86_64::registers::control::Cr3;
    use rust_os::process::process_manager::{create_user_process, create_user_thread};
    use rust_os::scheduler::scheduler::sleep;
    use rust_os::serial_println;
    use x86_64::structures::paging::PageTableFlags;
    use x86_64::VirtAddr;
    use rust_os::memory::vma::VmaKind;

    serial_println!("[syscall_test] Creating user process...");
    let upid = create_user_process("syscall_test".into());
    
    // Pick an arbitrary user-space address for our code page
    let user_base = rust_os::memory::address_space::USER_SPACE_START + 0x100000;
    
    let target_cr3 = {
        let mut pm_guard = PROCESS_MANAGER.lock();
        let proc = pm_guard.process_mut(upid);
        let space = proc.address_space.as_mut().unwrap();
        let mut mm_guard = MEMORY_MANAGER.lock();
        let mm = mm_guard.as_mut().unwrap();
        
        serial_println!("[syscall_test] Mapping code page...");
        let flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::USER_ACCESSIBLE;
            
        let code_addr = VirtAddr::new(user_base);
        mm.mmap_at(space, code_addr, 4096, flags, VmaKind::Code).unwrap();
        space.cr3()
    };

    let code_addr = VirtAddr::new(user_base);
    let msg_addr = code_addr + 0x40u64; // Place string at offset 64 in the same page

    // Switch to the user address space so we can write the payload directly
    let (current_cr3, cr3_flags) = Cr3::read();
    if current_cr3 != target_cr3 {
        unsafe { Cr3::write(target_cr3, cr3_flags); }
    }

    let ptr = code_addr.as_mut_ptr::<u8>();
    
    // Hand-assembled x86_64 machine code:
    // 1. SYS_WRITE (rax=1, rdi=1, rsi=msg_addr, rdx=16) -> int 0x80
    // 2. SYS_EXIT  (rax=60, rdi=0)                       -> int 0x80
    let mut code = [
        // mov rdi, 1 (stdout)
        0x48u8, 0xc7, 0xc7, 0x01, 0x00, 0x00, 0x00,
        // mov rsi, IMM64 (string address, patched below)
        0x48, 0xbe, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        // mov rdx, 16 (string length)
        0x48, 0xc7, 0xc2, 0x10, 0x00, 0x00, 0x00,
        // mov rax, 1 (SYS_WRITE)
        0x48, 0xc7, 0xc0, 0x01, 0x00, 0x00, 0x00,
        // int 0x80
        0xcd, 0x80,

        // mov rdi, 0 (exit code)
        0x48, 0xc7, 0xc7, 0x00, 0x00, 0x00, 0x00,
        // mov rax, 60 (SYS_EXIT)
        0x48, 0xc7, 0xc0, 0x3c, 0x00, 0x00, 0x00,
        // int 0x80
        0xcd, 0x80,

        // fallback infinite loop (jmp -2) just in case
        0xeb, 0xfe,
    ];

    // Patch the IMM64 placeholder for `mov rsi, ...` with the actual string address
    let msg_addr_bytes = msg_addr.as_u64().to_le_bytes();
    code[9..17].copy_from_slice(&msg_addr_bytes);

    unsafe {
        // Write the code
        core::ptr::copy_nonoverlapping(code.as_ptr(), ptr, code.len());
        
        // Write the string at offset 0x40
        let msg_ptr = (code_addr + 0x40u64).as_mut_ptr::<u8>();
        let msg = b"Hello syscalls!\n";
        core::ptr::copy_nonoverlapping(msg.as_ptr(), msg_ptr, msg.len());
    }

    // Restore kernel CR3 before continuing
    unsafe { Cr3::write(current_cr3, cr3_flags); }

    serial_println!("[syscall_test] Creating user thread...");
    let utid = create_user_thread(upid, code_addr);
    serial_println!("[syscall_test] User thread created TID({:?})", utid.0);

    // Keep the test thread alive so the kernel doesn't halt
    loop {
        sleep(50);
    }
}

extern "C" fn vfs_test() -> ! {
    serial_println!("[vfs_test] starting...");

    with_vfs(|vfs| {
        // ------------------------------------------------------------
        // 1. Create a directory
        // ------------------------------------------------------------
        serial_println!("[vfs_test] mkdir /tmp");
        match vfs.mkdir(&rust_os::fs::Path::root(), "/tmp") {
            Ok(()) => {}
            Err(rust_os::fs::FsError::AlreadyExists) => {
                // Fine if the test is run more than once.
            }
            Err(e) => {
                serial_println!("[vfs_test] mkdir failed: {:?}", e);
                return Err(e);
            }
        }

        // ------------------------------------------------------------
        // 2. Create and open a file
        // ------------------------------------------------------------
        serial_println!("[vfs_test] open /tmp/hello.txt");

        let flags =
            OpenFlags::from_raw(
                rust_os::fs::file::O_RDWR
                    | rust_os::fs::file::O_CREAT
                    | rust_os::fs::file::O_TRUNC,
            )?;

        let mut file = vfs.open(
            &rust_os::fs::Path::root(),
            "/tmp/hello.txt",
            flags,
        )?;

        // ------------------------------------------------------------
        // 3. Write
        // ------------------------------------------------------------
        let message = b"Hello from RamFS!";

        serial_println!(
            "[vfs_test] writing {} bytes",
            message.len()
        );

        let written = file.write(message)?;

        assert_eq!(written, message.len());

        // ------------------------------------------------------------
        // 4. Seek back to beginning
        // ------------------------------------------------------------
        let new_offset = file.seek(0, SeekWhence::Set)?;

        assert_eq!(new_offset, 0);

        // ------------------------------------------------------------
        // 5. Read it back
        // ------------------------------------------------------------
        let mut buffer = [0u8; 64];

        let read = file.read(&mut buffer)?;

        assert_eq!(read, message.len());
        assert_eq!(&buffer[..read], message);

        serial_println!(
            "[vfs_test] read back: {}",
            core::str::from_utf8(&buffer[..read]).unwrap()
        );

        // ------------------------------------------------------------
        // 6. Stat
        // ------------------------------------------------------------
        let stat = file.stat()?;

        assert_eq!(stat.file_type, FileType::Regular);
        assert_eq!(stat.size, message.len() as u64);

        serial_println!(
            "[vfs_test] stat: size={}, type={:?}",
            stat.size,
            stat.file_type
        );

        // ------------------------------------------------------------
        // 7. Directory listing
        // ------------------------------------------------------------
        let entries = vfs.readdir(
            &rust_os::fs::Path::root(),
            "/tmp",
        )?;

        let found = entries.iter().any(|entry| {
            entry.name == "hello.txt"
                && entry.file_type == FileType::Regular
        });

        assert!(found);

        serial_println!("[vfs_test] /tmp contains hello.txt");

        Ok(())
    })
    .expect("[vfs_test] VFS operation failed");

    serial_println!("[vfs_test] PASSED");

    loop {
        sleep(50);
    }
}