// src/process/usermode.rs
use core::arch::global_asm;

/// Helper to fetch the Ring 3 CS and SS selectors from the GDT.
/// Returns a u64 where the upper 32 bits are CS and lower 32 bits are SS.
#[no_mangle]
pub extern "C" fn get_user_selectors_helper() -> u64 {
    let selectors = crate::interrupts::gdt::get_selectors();
    let cs = selectors.user_code_selector.0 as u64 | 3;
    let ss = selectors.user_data_selector.0 as u64 | 3;
    (cs << 32) | ss
}

// Declare the global_asm symbol so naked_asm! can reference it
extern "C" {
    fn enter_usermode();
}

/// Pops the entry point and user stack pointer from the kernel stack,
/// fetches the user selectors, and jumps to the assembly enter_usermode routine.
#[unsafe(naked)]
pub unsafe extern "C" fn user_thread_trampoline() -> ! {
    core::arch::naked_asm!(
        // At this point, switch_context has executed `ret`, so RSP points to entry_point
        "pop rdi",        // rdi = entry_point
        "pop rsi",        // rsi = user_stack_top
        
        // Preserve arguments while calling the Rust helper
        "push rdi",
        "push rsi",
        "call {get_selectors}", // rax = (cs << 32) | ss
        "pop rsi",
        "pop rdi",
        
        "mov r8, rax",
        "shr r8, 32",     // r8 = cs
        "mov r9, rax",    // r9 = ss
        
        "jmp {enter_usermode}",
        get_selectors = sym get_user_selectors_helper,
        enter_usermode = sym enter_usermode,
    );
}

global_asm!(
    ".global enter_usermode",
    "enter_usermode:",
    // rdi = RIP (entry_point)
    // rsi = RSP (user_stack_top)
    // r8 = CS
    // r9 = SS
    
    // Set DS and ES to user data selector
    "mov eax, r9d",
    "mov ds, ax",
    "mov es, ax",
    
    // Push the iretq frame: SS, RSP, RFLAGS, CS, RIP
    "push r9",        // SS
    "push rsi",       // RSP
    "push 0x202",     // RFLAGS (Interrupts enabled)
    "push r8",        // CS
    "push rdi",       // RIP
    
    "iretq",
);