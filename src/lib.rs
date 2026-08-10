// src/lib.rs

#![no_std]
#![cfg_attr(test, no_main)]
#![feature(custom_test_frameworks)]
#![test_runner(crate::test_runner)]
#![reexport_test_harness_main = "test_main"]
#![feature(abi_x86_interrupt)]

pub mod serial;
pub mod vga_buffer;
pub mod memory;
pub mod task;
pub mod interrupts;
pub mod debug;
pub mod process;
pub mod scheduler;

extern crate alloc;

use core::panic::PanicInfo;
use interrupts::{gdt};
use crate::task::executor::Executor;
use crate::task::{Task, keyboard};
use crate::process::process_manager::PROCESS_MANAGER;
use crate::memory::memory_manager::{MemoryManager, MEMORY_MANAGER};
use bootloader::{BootInfo, entry_point};
use crate::process::process_manager;
use crate::memory::frame_allocator::{self, FRAME_ALLOCATOR, BootInfoFrameAllocator};


use x86_64::structures::paging::FrameAllocator;


#[cfg(test)]
use bootloader::{entry_point, BootInfo};

#[cfg(test)]
entry_point!(test_kernel_main);


pub fn init(boot_info: &'static BootInfo) {
    gdt::init();
    interrupts::init_idt();
    unsafe { interrupts::PICS.lock().initialize() };
    x86_64::instructions::interrupts::enable();

    let mut mm = unsafe {
        MemoryManager::new(boot_info.physical_memory_offset, &boot_info.memory_map)
    };
    mm.init_heap().expect("heap initialization failed");

    *MEMORY_MANAGER.lock() = Some(mm);
    unsafe { frame_allocator::init_global_allocator(&boot_info.memory_map) };

    let mut pm = PROCESS_MANAGER.lock();
    
    // 1. Bootstrap the kernel process (PID 0)
    let kpid = pm.init_kernel_process();

    drop(pm);

    // 2. Create the main kernel thread that will run the async executor
    let tid_init = process_manager::create_thread(kpid, kernel_init_thread);

    let idle_tid = process_manager::create_idle_thread(crate::process::idle::idle_loop);


}

pub trait Testable {
    fn run(&self) -> ();
}

impl<T> Testable for T
where
    T: Fn(),
{
    fn run(&self) {
        serial_print!("{}...\t", core::any::type_name::<T>());
        self();
        serial_println!("[ok]");
    }
}

pub fn test_runner(tests: &[&dyn Testable]) {
    serial_println!("Running {} tests", tests.len());
    for test in tests {
        test.run();
    }
    exit_qemu(QemuExitCode::Success);
}

pub fn test_panic_handler(info: &PanicInfo) -> ! {
    serial_println!("[failed]\n");
    serial_println!("Error: {}\n", info);
    exit_qemu(QemuExitCode::Failed);
    hlt_loop();
}

#[cfg(test)]
fn test_kernel_main(boot_info: &'static BootInfo) -> ! { 
    use memory::memory_manager::MemoryManager;
    init(boot_info);

    test_main();
    
    hlt_loop();
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

pub fn exit_qemu(exit_code: QemuExitCode) {
    use x86_64::instructions::port::Port;

    unsafe {
        let mut port = Port::new(0xf4);
        port.write(exit_code as u32);
    }
}

pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}

#[test_case]
fn trivial_assertion() {
    assert_eq!(1, 1);
}

#[test_case]
fn test_println_output() {
    println!("test_println_output");
}

#[test_case]
fn test_heap_allocation() {
    use alloc::boxed::Box;
    let x = Box::new(42);
    assert_eq!(*x, 42);
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
    
    
    executor.run();
}

async fn async_number() -> u32 {
    42
}

async fn example_task() {
    let number = async_number().await;
    println!("async number: {}", number);
}

