#![no_std]
#![feature(abi_x86_interrupt)]

pub mod serial;
pub mod vga_buffer;
pub mod memory;
pub mod task;
pub mod interrupts;
pub mod debug;
pub mod process;
pub mod scheduler;
pub mod sync;
pub mod syscall;
pub mod fs;

extern crate alloc;

use interrupts::{gdt};
use crate::task::executor::Executor;
use crate::task::{Task, keyboard};
use crate::process::process_manager::PROCESS_MANAGER;
use crate::memory::memory_manager::{MemoryManager, MEMORY_MANAGER};
use bootloader::BootInfo;
use crate::process::process_manager;

pub fn init(boot_info: &'static BootInfo) {
    gdt::init();
    interrupts::init_idt();
    unsafe { interrupts::PICS.lock().initialize() };
    unsafe {
        use x86_64::registers::control::Efer;
        use x86_64::registers::control::EferFlags;
        let mut efer = Efer::read();
        efer.insert(EferFlags::NO_EXECUTE_ENABLE);
        Efer::write(efer);
    }

    debugln!("{:?}", boot_info.physical_memory_offset);
    let mut mm = unsafe {
        MemoryManager::new(boot_info.physical_memory_offset, &boot_info.memory_map)
    };

    crate::vga_buffer::KERNEL_CR3_PHYS.store(
        mm.kernel_p4_frame.start_address().as_u64(),
        core::sync::atomic::Ordering::Relaxed,
    );

    mm.init_heap().expect("heap initialization failed");
    *MEMORY_MANAGER.lock() = Some(mm);

    crate::fs::init().expect("filesystem initialization failed");
    
    let mut pm = PROCESS_MANAGER.lock();
    
    // Bootstrap the kernel process (PID 0).
    let kpid = pm.init_kernel_process();

    drop(pm);

    process_manager::create_thread(kpid, kernel_init_thread);

    process_manager::create_idle_thread(crate::process::idle::idle_loop);
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

extern "C" fn kernel_init_thread() -> ! {
    println!("Kernel init thread started. Running Async Executor...");
    
    
    let mut executor = Executor::new();
    executor.spawn(Task::new(example_task()));
    executor.spawn(Task::new(keyboard::print_keypresses()));
    
    executor.run();
}

async fn async_number() -> u32 {
    42
}

async fn example_task() {
    let number = async_number().await;
    println!("async number: {}", number);
}
