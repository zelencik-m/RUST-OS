/*
 * The entry point of every Ulib program.
 *
 * The kernel starts the first thread with rsp pointing at argc, followed by
 * argv[0..argc], NULL, and an empty envp (see src/process/initial_stack.rs):
 *
 *   rsp + 8*(argc+2)   NULL      (envp terminator)
 *   rsp + 8*(argc+1)   NULL      (argv[argc])
 *   rsp + 8            argv[0]
 *   rsp                argc
 *
 * `_start` is not a C function: nothing called it, so there is no return
 * address on the stack, and it must never `ret`. It passes argc/argv to
 * main in the first two argument registers (System V: rdi, rsi), and hands
 * main's return value (eax) to exit().
 *
 * Stack alignment: the ABI wants rsp to be a multiple of 16 *before* a
 * `call`, so the callee starts with rsp = 8 (mod 16) after the return
 * address is pushed. The kernel already aligns rsp, but `and` makes that
 * independent of the kernel. Code compiled with SSE (not ours, yet) would
 * crash on `movaps` to a misaligned stack slot otherwise.
 */
__asm__(
    ".text\n"
    ".globl _start\n"
    ".type _start, @function\n"
    "_start:\n"
    "    xor %ebp, %ebp\n"       /* outermost frame: ends frame-pointer walks */
    "    mov (%rsp), %rdi\n"     /* argc */
    "    lea 8(%rsp), %rsi\n"    /* argv: the pointer array itself */
    "    and $-16, %rsp\n"
    "    call main\n"
    "    mov %eax, %edi\n"       /* main's return value is the exit code */
    "    call exit\n"
    "    ud2\n"                  /* exit doesn't return */
    ".size _start, . - _start\n");
