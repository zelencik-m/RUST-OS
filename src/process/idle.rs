use core::arch::asm;

pub extern "C" fn idle_loop() -> ! {
    loop {
        // SAFETY: sti; hlt is a standard idle sequence. Interrupts are
        // enabled before halting so any pending IRQ can wake the CPU.
        unsafe {
            asm!("sti; hlt", options(nomem, nostack));
        }
        // An interrupt (e.g. a key press) may have woken a thread. Switch
        // to it now instead of idling until the timer's next time slice,
        // which would add up to ~110 ms of input latency.
        let work_queued = crate::scheduler::scheduler::SCHEDULER.lock().has_runnable();
        if work_queued {
            crate::scheduler::scheduler::yield_now();
        }
    }
}
