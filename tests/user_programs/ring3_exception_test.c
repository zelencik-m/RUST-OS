#include "syscalls.h"

__attribute__((noreturn)) void _start(void) {
    WRITE_LITERAL(1, "Triggering a Ring-3 page fault\n");

    volatile u64 *unmapped = (volatile u64 *)0x20000000;
    *unmapped = 0x20000000;

    WRITE_LITERAL(2, "Ring-3 page fault test unexpectedly survived\n");
    exit_process(1);
}
