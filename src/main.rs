// src/main.rs

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(rust_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;
use rust_os::{hlt_loop, print, println};
use rust_os::task::{Task, keyboard};
use rust_os::task::executor::Executor;
use rust_os::memory::memory_manager::MemoryManager;
use rust_os::debug;

use bootloader::{BootInfo, entry_point};
use alloc::string::{String, ToString};

extern crate alloc;

use alloc::{boxed::Box, vec, vec::Vec, rc::Rc};



entry_point!(kernel_main);


use rust_os::process::process_manager::{PROCESS_MANAGER, switch_to};
use rust_os::scheduler::scheduler::{Scheduler, SCHEDULER, yield_now, sleep};
use rust_os::process::PID;
use rust_os::process::thread::TID;


fn kernel_main(boot_info: &'static BootInfo) -> ! {
    rust_os::init(boot_info);
    
    {
        let mut pm = PROCESS_MANAGER.lock();

        let upid = pm.create_user_process("test".into());
        let tid_a = pm.create_thread(upid, thread_a);
        let tid_b = pm.create_thread(upid, thread_b);

    } 

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
        sleep(50);
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
        sleep(100);
    }
}


