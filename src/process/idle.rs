// src/process/idle.rs

use core::arch::asm;

pub extern "C" fn idle_loop() -> ! {
    loop {
        // SAFETY: sti; hlt is a standard idle sequence. Interrupts are
        // enabled before halting so any pending IRQ can wake the CPU.
        unsafe {
            asm!("sti; hlt", options(nomem, nostack));
        }
    }
    }