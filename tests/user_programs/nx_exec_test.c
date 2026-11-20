#include "syscalls.h"

__attribute__((noreturn)) void _start(void) {
    WRITE_LITERAL(1, "Attempting to execute non-executable stack memory\n");

    /* x86-64 machine code for a bare `ret`. Executing it must fault because
       the user stack is mapped NO_EXECUTE. */
    volatile unsigned char code[] = {0xC3};
    void (*fn)(void) = (void (*)(void))(unsigned long)code;
    fn();

    WRITE_LITERAL(2, "NX execution unexpectedly survived\n");
    exit_process(1);
}
