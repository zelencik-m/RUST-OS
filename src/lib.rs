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
pub mod keyboard;
pub mod tty;
pub mod boot_screen;
pub mod status_bar;
pub mod theme;
pub mod programs;

extern crate alloc;

use interrupts::{gdt};
use crate::task::executor::Executor;
use crate::task::Task;
use crate::process::process_manager::PROCESS_MANAGER;
use crate::memory::memory_manager::{MemoryManager, MEMORY_MANAGER};
use bootloader::bootinfo::MemoryRegionType;
use bootloader::BootInfo;
use core::fmt;
use crate::process::process_manager;

/// Initialize the whole kernel, silently: [`init_core`], then mount the
/// root filesystem ([`init_storage`]). Test kernels use this.
pub fn init(boot_info: &'static BootInfo) {
    init_core(boot_info, &mut |_, _| {});
    init_storage().expect("filesystem initialization failed");
}

/// Initialize everything that's fast and must come first: CPU tables,
/// interrupts, memory, the heap, processes and the scheduler. Calls
/// `progress(step, detail)` after each step (the boot screen shows them);
/// the first calls come before the heap exists, so `progress` must not
/// allocate.
///
/// Interrupts stay disabled; the caller enables them when it's ready. After
/// this, threads can be created and run.
pub fn init_core(boot_info: &'static BootInfo, progress: &mut dyn FnMut(&str, fmt::Arguments)) {
    gdt::init();
    interrupts::init_idt();
    progress("CPU tables", format_args!("GDT, TSS with fault stacks, IDT"));

    unsafe { interrupts::PICS.lock().initialize() };
    progress("Interrupts", format_args!("8259 PIC: timer and keyboard IRQs"));

    unsafe {
        use x86_64::registers::control::Efer;
        use x86_64::registers::control::EferFlags;
        let mut efer = Efer::read();
        efer.insert(EferFlags::NO_EXECUTE_ENABLE);
        Efer::write(efer);
    }
    progress("No-execute pages", format_args!("EFER.NXE"));

    log_debug!("physical memory offset {:#x}", boot_info.physical_memory_offset);
    let mut mm = unsafe {
        MemoryManager::new(boot_info.physical_memory_offset, &boot_info.memory_map)
    };

    crate::vga_buffer::KERNEL_CR3_PHYS.store(
        mm.kernel_p4_frame.start_address().as_u64(),
        core::sync::atomic::Ordering::Relaxed,
    );

    let usable: u64 = boot_info
        .memory_map
        .iter()
        .filter(|region| region.region_type == MemoryRegionType::Usable)
        .map(|region| region.range.end_addr() - region.range.start_addr())
        .sum();
    progress("Memory", format_args!("{} MiB usable, 4-level paging", usable >> 20));

    mm.init_heap().expect("heap initialization failed");
    *MEMORY_MANAGER.lock() = Some(mm);
    progress(
        "Kernel heap",
        format_args!("{} MiB buddy allocator", crate::memory::allocator::HEAP_SIZE >> 20),
    );

    let mut pm = PROCESS_MANAGER.lock();
    
    // Bootstrap the kernel process (PID 0).
    let kpid = pm.init_kernel_process();

    drop(pm);

    process_manager::create_thread(kpid, kernel_init_thread);

    process_manager::create_idle_thread(crate::process::idle::idle_loop);

    // Keep the run queue from growing (allocating) inside interrupt handlers.
    crate::scheduler::scheduler::SCHEDULER
        .lock()
        .reserve_queue_capacity(64);
    progress("Scheduler", format_args!("kernel process, idle thread, preemption"));
}

/// Mount the root filesystem: ext2 on the ATA disk, as the VFS root.
///
/// Slow (it reads the disk), so `src/main.rs` runs it in a kernel thread
/// with interrupts on, and the boot screen keeps animating meanwhile. Needs
/// [`init_core`] first (the heap).
pub fn init_storage() -> Result<crate::fs::Ext2Geometry, crate::fs::FsError> {
    crate::fs::init()
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
    log_debug!("kernel init thread started, running the async executor");

    
    let mut executor = Executor::new();
    executor.spawn(Task::new(example_task()));
    
    executor.run();
}

async fn async_number() -> u32 {
    42
}

async fn example_task() {
    let number = async_number().await;
    log_debug!("async example task: number {}", number);
}
