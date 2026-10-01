#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(support::test_runner)]
#![reexport_test_harness_main = "test_main"]

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;

extern crate alloc;

entry_point!(test_kernel_main);

fn test_kernel_main(boot_info: &'static BootInfo) -> ! {
    rust_os::init(boot_info);
    test_main();
    rust_os::hlt_loop()
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    support::test_panic_handler(info)
}

#[test_case]
fn test_println_output() {
    rust_os::println!("test_println_output");
}

#[test_case]
fn test_heap_allocation() {
    use alloc::boxed::Box;
    use alloc::vec::Vec;

    let boxed = Box::new([0xA5u8; 32]);
    assert!(boxed.iter().all(|byte| *byte == 0xA5));

    let mut values = Vec::with_capacity(1);
    values.push(10u64);
    for value in 11..75 {
        values.push(value);
    }
    assert_eq!(values.len(), 65);
    assert_eq!(values.first(), Some(&10));
    assert_eq!(values.last(), Some(&74));
    assert_eq!(values.iter().sum::<u64>(), (10..75).sum());
}
mod support;

#[test_case]
fn rescheduling_current_thread_keeps_it_running() {
    use rust_os::process::process_manager::ProcessManager;
    use rust_os::process::thread::ThreadState;
    use rust_os::scheduler::scheduler::Scheduler;

    extern "C" fn unused_entry() -> ! {
        rust_os::hlt_loop()
    }

    // Use local managers: no real context switch or global scheduling occurs.
    let mut manager = ProcessManager::new();
    let pid = manager.init_kernel_process();
    let tid = manager.create_thread_locked(pid, unused_entry);
    let mut scheduler = Scheduler::new();
    manager.commit_switch(&mut scheduler, tid);
    assert_eq!(manager.get_thread_state(tid), ThreadState::Running);

    let (old_context, new_context) = manager.commit_switch(&mut scheduler, tid);
    assert_eq!(old_context as *const _, new_context);
    assert_eq!(scheduler.current, Some(tid));
    assert_eq!(manager.get_thread_state(tid), ThreadState::Running);
}
