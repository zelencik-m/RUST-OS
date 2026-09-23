#ifndef RUST_OS_USER_SYSCALLS_H
#define RUST_OS_USER_SYSCALLS_H

typedef unsigned long u64;
typedef signed long i64;
typedef unsigned long usize;

enum {
    SYS_READ = 0,
    SYS_WRITE = 1,
    SYS_OPEN = 2,
    SYS_CLOSE = 3,
    SYS_LSEEK = 8,
    SYS_EXIT = 60,
    SYS_MKDIR = 83,
    SYS_SLEEP_TICKS = 1000,
};

static inline u64 syscall6(
    u64 number,
    u64 arg1,
    u64 arg2,
    u64 arg3,
    u64 arg4,
    u64 arg5,
    u64 arg6
) {
    register u64 rax __asm__("rax") = number;
    register u64 r10 __asm__("r10") = arg4;
    register u64 r8 __asm__("r8") = arg5;
    register u64 r9 __asm__("r9") = arg6;

    __asm__ volatile(
        "int $0x80"
        : "+a"(rax)
        : "D"(arg1), "S"(arg2), "d"(arg3), "r"(r10), "r"(r8), "r"(r9)
        : "memory", "cc"
    );
    return rax;
}

static inline u64 syscall3(u64 number, u64 arg1, u64 arg2, u64 arg3) {
    return syscall6(number, arg1, arg2, arg3, 0, 0, 0);
}

static inline u64 syscall2(u64 number, u64 arg1, u64 arg2) {
    return syscall6(number, arg1, arg2, 0, 0, 0, 0);
}

static inline u64 syscall1(u64 number, u64 arg1) {
    return syscall6(number, arg1, 0, 0, 0, 0, 0);
}

static inline void write_bytes(u64 fd, const char *bytes, usize length) {
    syscall3(SYS_WRITE, fd, (u64)bytes, length);
}

#define WRITE_LITERAL(fd, text) write_bytes((fd), (text), sizeof(text) - 1)

__attribute__((noreturn)) static inline void exit_process(u64 status) {
    syscall1(SYS_EXIT, status);
    for (;;) {
        __asm__ volatile("pause");
    }
}

#endif
