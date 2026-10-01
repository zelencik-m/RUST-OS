#include "syscalls.h"

__attribute__((noreturn)) void _start(void) {
    WRITE_LITERAL(1, "Hello syscalls!\n");
    exit_process(0);
}
