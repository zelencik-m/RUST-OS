
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

        // 3. Update IDT to point to the naked wrapper
        unsafe {
            idt[InterruptIndex::Timer.as_usize()]
                .set_handler_fn(core::mem::transmute::<*const (), extern "x86-interrupt" fn(InterruptStackFrame)>(
                    timer_interrupt_wrapper as *const ()
                ));
        }

        idt[InterruptIndex::Keyboard.as_usize()]
            .set_handler_fn(keyboard_interrupt_handler);

        idt.page_fault.set_handler_fn(page_fault_handler);

        idt.general_protection_fault.set_handler_fn(general_protection_fault_handler);


        idt
    };
}

pub fn init_idt() {
    IDT.load();
}

// src/interrupts/mod.rs
extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;
    use x86_64::structures::paging::{Page, Size4KiB};

    let fault_addr = Cr2::read();
    let page_addr  = Page::<Size4KiB>::containing_address(fault_addr).start_address();

    // 1. Who faulted?
    let tid = match SCHEDULER.lock().get_current() {
        Some(t) => t,
        None => panic!(
            "PAGE FAULT in kernel, no current thread\n  addr: {:?}\n  err:  {:?}\n{:#?}",
            fault_addr, error_code, stack_frame
        ),
    };

    // 2. Ask ProcessManager for the full VMA
    let (owner, vma) = {
        let pm = PROCESS_MANAGER.lock();
        let owner = pm.thread(tid).get_owner();
        let vma = pm.get_vma(owner, fault_addr);
        (owner, vma)
    }; // pm lock is dropped here to prevent deadlocks when demanding pages

    // 3. Act on the answer
    match vma {
        Some(vma) => {
            // Catch stack overflows hitting the guard page
            if vma.kind == crate::memory::vma::VmaKind::Guard {
                println!("PAGE FAULT: stack overflow (guard page) PID({}), TID({:?})", owner.0, tid);
                crate::process::process_manager::kill_current_thread_and_schedule(tid);
                return;
            }

            match crate::process::process_manager::demand_map_page(owner, page_addr, vma.flags) {
                Ok(()) => {
                    // return → iretq retries the faulting instruction
                }
                Err(_) => {
                    println!("PAGE FAULT: OOM while mapping PID({}), TID({:?})", owner.0, tid);
                    println!("  faulting addr: {:?}", fault_addr);
                    crate::process::process_manager::kill_current_thread_and_schedule(tid);
                }
            }
        }
        None => {
            println!("PAGE FAULT: killing PID({}), TID({:?})", owner.0, tid);
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

// Add this import at the top of the file if not already present
use core::arch::global_asm;

// 1. Naked Wrapper
global_asm!(
    ".global timer_interrupt_wrapper",
    "timer_interrupt_wrapper:",
    "cld",              // Ensure direction flag is clear for System V ABI
    "sub rsp, 128",     // Skip the 128-byte red zone to avoid corrupting leaf-function data
    // Push all caller-saved registers to preserve them across the context switch
    "push rax",
    "push rcx",
    "push rdx",
    "push rsi",
    "push rdi",
    "push r8",
    "push r9",
    "push r10",
    "push r11",
    
    // Call the Rust handler
    "call timer_interrupt_rust_handler",
    
    // Pop caller-saved registers (restores the new thread's state)
    "pop r11",
    "pop r10",
    "pop r9",
    "pop r8",
    "pop rdi",
    "pop rsi",
    "pop rdx",
    "pop rcx",
    "pop rax",
    "add rsp, 128",     // Restore red zone skip
    "iretq"             // Return from interrupt (consumes the CPU-pushed frame)
);

extern "C" {
    fn timer_interrupt_wrapper();
}

// 2. Rust Handler
#[no_mangle]
extern "C" fn timer_interrupt_rust_handler() {
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

extern "x86-interrupt" fn general_protection_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    // Check the Code Segment (CS) register to see what Ring we were in.
    // The bottom 2 bits of CS represent the Requested Privilege Level (RPL).
    let cs = stack_frame.code_segment;
    
    if cs & 0x3 == 3 {
        // We are in Ring 3 (User Mode)! The user program did something illegal.
        let tid = SCHEDULER.lock().get_current().expect("No current thread during #GP");
        let owner = PROCESS_MANAGER.lock().thread(tid).get_owner();
        
        println!("\n!!! EXCEPTION: GENERAL PROTECTION FAULT (#GP) in Ring 3 !!!");
        println!("  Killing PID({}), TID({:?})", owner.0, tid);
        println!("  Error Code: {:#x}", error_code);
        println!("  Instruction Pointer: {:?}", stack_frame.instruction_pointer);
        
        // Kill the thread and seamlessly switch to the next one
        crate::process::process_manager::kill_current_thread_and_schedule(tid);
    } else {
        // We are in Ring 0 (Kernel Mode)! The kernel did something wrong. This is fatal.
        panic!(
            "\n!!! KERNEL EXCEPTION: GENERAL PROTECTION FAULT (#GP) !!!\n  Error Code: {:#x}\n  Stack Frame:\n{:#?}", 
            error_code, stack_frame
        );
    }
}