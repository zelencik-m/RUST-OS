// src/syscalls/entry.rs

use core::arch::global_asm;

global_asm!(
    ".global syscall_int_stub",
    "syscall_int_stub:",

    // System V ABI requires DF = 0.
    "cld",

    // Save registers.
    //
    // IMPORTANT:
    // This push order must match SyscallFrame exactly.
    "push rax",
    "push rdi",
    "push rsi",
    "push rdx",
    "push r10",
    "push r8",
    "push r9",
    "push rcx",
    "push rbx",
    "push rbp",
    "push r11",
    "push r12",
    "push r13",
    "push r14",
    "push r15",

    // Pass &mut SyscallFrame to Rust.
    "mov rdi, rsp",

    "call syscall_int_handler",

    // Restore registers in reverse order.
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop r11",
    "pop rbp",
    "pop rbx",
    "pop rcx",
    "pop r9",
    "pop r8",
    "pop r10",
    "pop rdx",
    "pop rsi",
    "pop rdi",
    "pop rax",

    "iretq",
);

extern "C" {
    pub static syscall_int_stub: u8;
}