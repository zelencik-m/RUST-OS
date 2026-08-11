// src/main.rs

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(rust_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;
use rust_os::{hlt_loop, print, println, debugln};

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

        debugln!(">>> entering hlt_loop (scheduler will take over via timer interrupts)");
    });
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
    use rust_os::scheduler::scheduler::SCHEDULER;

    println!("[thread_a] starting demand paging checkpoint...");

    // 1. Get current TID and PID
    let tid = SCHEDULER.lock().get_current().expect("no current thread");
    let pid = PROCESS_MANAGER.lock().thread(tid).get_owner();

    // 2. Register a VMA without mapping it
    let test_addr = VirtAddr::new(0x1000_0000);
    process_manager::register_vma_only(
        pid,
        test_addr,
        4096,
        PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
        VmaKind::User, // Make sure your enum has a User variant, or change as needed
    );

    // 3. In-bounds access: should trigger #PF, get demand-mapped, and succeed
    unsafe {
        let p = test_addr.as_mut_ptr::<u64>();
        *p = 0xCAFEBABE; // Traps here, handler maps page, returns, and executes store again
        assert_eq!(*p, 0xCAFEBABE);
    }
    println!("[thread_a] in-bounds demand paging test passed!");

    // 4. Out-of-bounds access: should trigger #PF and kill this process
    unsafe {
        let p = VirtAddr::new(0xDEAD_BEEF_0000).as_mut_ptr::<u64>();
        *p = 1; // Traps here, handler detects out-of-bounds, kills process
    }
    
    println!("[thread_a] this line should never print");
    
    loop {
        yield_now();
    }
}

extern "C" fn thread_b() -> ! {
    let mut counter: u64 = 100000000;
    let mut subtracting = true;
    loop {
        println!("executing thread B (counter={})", counter);
        loop{
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

extern "C" fn thread_c() -> ! {
    let mut counter: u64 = 0;
    loop {
        println!("executing thread C (counter={})", counter);
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