
pub mod gdt;


// src/interrupts/mod.rs

use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};
use lazy_static::lazy_static;
use pic8259::ChainedPics;
use spin;
use x86_64::structures::idt::PageFaultErrorCode;

use crate::hlt_loop;
use crate::println;
use crate::print;
use crate::scheduler::scheduler::schedule;
use crate::scheduler::scheduler::{Scheduler, SCHEDULER, yield_now};
use crate::PROCESS_MANAGER;
use crate::process::PID;
use crate::MEMORY_MANAGER;
use crate::process::thread::TID;

use x86_64::VirtAddr;
use x86_64::structures::paging::PageTableFlags;
use x86_64::structures::paging::Page;
use x86_64::structures::paging::Size4KiB;
use x86_64::structures::paging::FrameAllocator;
use x86_64::structures::paging::Mapper;

pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

pub static PICS: spin::Mutex<ChainedPics> =
    spin::Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = PIC_1_OFFSET,
    Keyboard, 
}

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        unsafe {
            idt.double_fault.set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }

        idt[InterruptIndex::Timer.as_usize()]
            .set_handler_fn(timer_interrupt_handler);

        idt[InterruptIndex::Keyboard.as_usize()]
            .set_handler_fn(keyboard_interrupt_handler);

        idt.page_fault.set_handler_fn(page_fault_handler);

        idt


    };
}

pub fn init_idt() {
    IDT.load();
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;
    use crate::memory::memory_manager::{FaultAction, MEMORY_MANAGER};

    let fault_addr = Cr2::read();

    // 1. Who faulted? Drop the scheduler lock immediately.
    let tid = match SCHEDULER.lock().get_current() {
        Some(t) => t,
        None => panic!(
            "PAGE FAULT in kernel, no current thread\n  addr: {:?}\n  err:  {:?}\n{:#?}",
            fault_addr, error_code, stack_frame
        ),
    };

    // 2. Lock PROCESS_MANAGER first, then MEMORY_MANAGER to strictly enforce lock order
    // and completely eliminate deadlock risks.
    let action = {
        let mut pm = PROCESS_MANAGER.lock();
        let owner = pm.thread(tid).get_owner();
        
        let proc = pm.process_mut(owner);
        let space = match proc.address_space.as_mut() {
            Some(s) => s,
            None => {
                // Kernel page fault!
                panic!(
                    "KERNEL PAGE FAULT\n  addr: {:?}\n  err:  {:?}\n{:#?}",
                    fault_addr, error_code, stack_frame
                );
            }
        };

        let mut mm_guard = MEMORY_MANAGER.lock();
        let mm = mm_guard.as_mut().expect("MemoryManager not initialized");
        
        mm.handle_page_fault(space, fault_addr)
    };

    // 3. Act on the answer
    match action {
        Ok(FaultAction::Resolved) => {
            // return -> iretq retries the faulting instruction
        }
        Ok(FaultAction::Segfault) => {
            println!("PAGE FAULT: killing TID({:?})", tid);
            println!("  faulting addr: {:?}", fault_addr);
            println!("  error code:    {:?}", error_code);
            println!("{:#?}", stack_frame);
            crate::process::process_manager::kill_current_thread_and_schedule(tid);
        }
        Err(_) => {
            println!("PAGE FAULT: OOM while mapping TID({:?})", tid);
            println!("  faulting addr: {:?}", fault_addr);
            println!("  error code:    {:?}", error_code);
            println!("{:#?}", stack_frame);
            crate::process::process_manager::kill_current_thread_and_schedule(tid);
        }
    }
}

extern "x86-interrupt" fn keyboard_interrupt_handler(
    _stack_frame: InterruptStackFrame)
{
    use pc_keyboard::{layouts, HandleControl, Keyboard, ScancodeSet1};
    use spin::Mutex;
    use x86_64::instructions::port::Port;

    static KEYBOARD: Mutex<Keyboard<layouts::Us104Key, ScancodeSet1>> =
        Mutex::new(Keyboard::new(
            ScancodeSet1::new(),
            layouts::Us104Key,
            HandleControl::Ignore,
        ));

    let keyboard = KEYBOARD.lock();
    let mut port = Port::new(0x60);

    let scancode: u8 = unsafe { port.read() };
    crate::task::keyboard::add_scancode(scancode);

    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Keyboard.as_u8());
    }
}

extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    let should_switch = SCHEDULER.lock().tick(); // lock dropped immediately

    unsafe {
        PICS.lock().notify_end_of_interrupt(InterruptIndex::Timer.as_u8());
    }

    if should_switch {
        yield_now(); // safe now: PIC EOI already sent, lock already released
    }
}

extern "x86-interrupt" fn breakpoint_handler(
    stack_frame: InterruptStackFrame)
{
    println!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame, _error_code: u64) -> !
{
    panic!("EXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
}

impl InterruptIndex {
    fn as_u8(self) -> u8 {
        self as u8
    }

    fn as_usize(self) -> usize {
        usize::from(self.as_u8())
    }
}

