// src/main.rs

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(rust_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;
use rust_os::{hlt_loop, print, println};

use bootloader::{BootInfo, entry_point};

extern crate alloc;

entry_point!(kernel_main);


use rust_os::process::process_manager::PROCESS_MANAGER;
use rust_os::scheduler::scheduler::sleep;
use rust_os::process::process_manager;

fn kernel_main(boot_info: &'static BootInfo) -> ! {
    rust_os::init(boot_info);
    
use rust_os::memory::address_space::{AddressSpace, mapper_for_address_space};
use x86_64::{VirtAddr, structures::paging::OffsetPageTable};
use x86_64::structures::paging::Translate;
use rust_os::memory::paging;
use rust_os::memory::memory_manager;

    // ---------- checkpoint: verify kernel-half sharing ----------
    unsafe {
        let phys_offset = VirtAddr::new(boot_info.physical_memory_offset);

        // Lock the global memory manager
        let mut guard = memory_manager::MEMORY_MANAGER.lock();
        let mm = guard.as_mut().expect("Memory manager not initialised");

        // Create a test address space (uses mm's frame allocator)
        let new_space = mm.create_address_space();

        // Build a mapper for the new space
        let new_mapper = mapper_for_address_space(new_space.p4_frame, phys_offset);

        // Build a mapper for the currently active (live) space
        let live_p4 = paging::active_level_4_table(phys_offset);
        let live_mapper = OffsetPageTable::new(live_p4, phys_offset);

        // Pick a kernel virtual address and translate
        let test_virt = VirtAddr::new(0xFFFF_FFFF_8000_0000);

        let phys_new = new_mapper.translate_addr(test_virt);
        let phys_live = live_mapper.translate_addr(test_virt);

        assert_eq!(phys_new, phys_live, "Kernel-half sharing failed!");
        println!("[OK] Kernel-half sharing verified. phys_new == phys_live = {:?}", phys_new);
    }

    // main.rs — kernel_main()
    let mut pm = PROCESS_MANAGER.lock();
    let upid = pm.create_user_process("test".into());
    drop(pm); // release before calling the free functions below

    // let tid_a = process_manager::create_thread(upid, thread_a);
    // let tid_b = process_manager::create_thread(upid, thread_b);

    rust_os::hlt_loop();
}

/// This function is called on panic.
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("{}", info);
    hlt_loop();

}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    rust_os::test_panic_handler(info)
}

extern "C" fn thread_a() -> ! {
    let mut counter: u64 = 0;
    loop {
        println!("executing thread A (counter={})", counter);
        loop{
            // println!("thread A (counter={})", counter);
            counter += 1;
            if counter % 100 == 0 {
                break;
            }
        }
        sleep(10);
    }
}

extern "C" fn thread_b() -> ! {
    let mut counter: u64 = 100000000;
    let mut subtracting = true;
    loop {
        println!("executing thread B (counter={})", counter);
        loop{
            // println!("thread B (counter={})", counter);
            if subtracting {
                counter -= 1;
            } else {
                counter += 1;
            }
            if counter == 0 {
                subtracting = false;
            } else if counter == 100000000 {
                subtracting = true;
            }
            counter -= 1;
            if counter % 100 == 0 {
                break;
            }
        }
        // sleep(100);
    }
}


