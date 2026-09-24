#include "syscalls.h"

__attribute__((noreturn)) void _start(void) {
    for (;;) {
        WRITE_LITERAL(1, "executing ELF thread B\n");
        syscall1(SYS_SLEEP_TICKS, 3);
    }
}
