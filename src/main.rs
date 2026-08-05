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

async fn async_number() -> u32 {
    42
}

async fn example_task() {
    let number = async_number().await;
    println!("async number: {}", number);
}


entry_point!(kernel_main);


use rust_os::process::process_manager::{PROCESS_MANAGER, switch_to};
use rust_os::scheduler::scheduler::{Scheduler, SCHEDULER, yield_now};
use rust_os::process::PID;
use rust_os::process::thread::TID;


fn kernel_main(boot_info: &'static BootInfo) -> ! {
    println!("Hello World{}", "!");
    rust_os::init();
    
    debug!("Loading ");

    let mut mm = unsafe {
        MemoryManager::new(boot_info.physical_memory_offset, &boot_info.memory_map)
    };
    mm.init_heap().expect("heap initialization failed");

    // ________________________________________________
    // Process & Thread Setup
    // ________________________________________________
    {
        let mut pm = PROCESS_MANAGER.lock();
        
        // 1. Bootstrap the kernel process (PID 0)
        let kpid = pm.init_kernel_process();

        // 2. Create the main kernel thread that will run the async executor
        let tid_init = pm.create_thread(kpid, kernel_init_thread);

        // 3. Create a user process with two test threads
        let upid = pm.create_user_process("test".into());
        let tid_a = pm.create_thread(upid, thread_a);
        let tid_b = pm.create_thread(upid, thread_b);

        // Optional: verify TIDs
        assert_eq!(tid_init, TID(0));
        assert_eq!(tid_a, TID(1));
        assert_eq!(tid_b, TID(2));
    } // lock dropped here

    // Hand off execution to the kernel init thread. 
    // This call will never return to kernel_main.
    // switch_to(TID(0));
    // switch_to(TID(0));

    // If switch_to ever returns, halt the CPU.
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
        // yield_now();
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
        // yield_now();
    }
}

extern "C" fn kernel_worker() -> ! {
    let mut ticks: u64 = 0;
    loop {
        ticks += 1;
        // println!("[kernel worker] tick={}", ticks);
        // Alternate through the user threads.
        switch_to(TID(1));
    }
}

// This is the main "background" thread of your kernel
extern "C" fn kernel_init_thread() -> ! {
    println!("Kernel init thread started. Running Async Executor...");
    
    let mut executor = Executor::new();
    executor.spawn(Task::new(example_task()));
    executor.spawn(Task::new(keyboard::print_keypresses()));
    
    // This blocks forever, running async tasks.
    // If you want thread_a and thread_b to still run, you would need to 
    // integrate the scheduler with the executor (e.g., yielding here).
    
    // switch_to(TID(1)); // Start with thread_a
    
    executor.run();
}
