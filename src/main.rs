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
use rust_os::scheduler::scheduler::sleep;
use rust_os::process::process_manager;
use x86_64::instructions::interrupts;

fn kernel_main(boot_info: &'static BootInfo) -> ! {
    rust_os::init(boot_info);

    interrupts::without_interrupts(|| {
        debugln!(">>> kernel_main: creating user process");
        // let mut pm = PROCESS_MANAGER.lock();
        let upid = process_manager::create_user_process("test".into());
        debugln!(">>> created user process PID({:?})", upid.0);
        // drop(pm);

        debugln!(">>> creating thread_a");
        let tid_a = process_manager::create_thread(upid, thread_a);
        debugln!(">>> thread_a created TID({:?})", tid_a.0);
        debugln!(">>> creating thread_b");
        let tid_b = process_manager::create_thread(upid, thread_b);
        debugln!(">>> thread_b created TID({:?})", tid_b.0);

        debugln!(">>> entering hlt_loop");
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
