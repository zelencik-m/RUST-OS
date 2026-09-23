#include "syscalls.h"

static volatile u64 bss_value;

__attribute__((noreturn)) void _start(void) {
    if (bss_value != 0) {
        WRITE_LITERAL(2, "ELF loader test failed: .bss was not zero\n");
        exit_process(1);
    }

    WRITE_LITERAL(1, "ELF loader test passed\n");
    exit_process(0);
}
