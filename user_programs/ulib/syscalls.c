/* Thin wrappers over the kernel's syscalls. Each returns the raw result:
 * negative values are -errno. */
#include "ulib.h"

ssize_t read(int fd, void *buffer, size_t count) {
    return (ssize_t)syscall3(SYS_READ, (u64)fd, (u64)buffer, count);
}

ssize_t write(int fd, const void *buffer, size_t count) {
    return (ssize_t)syscall3(SYS_WRITE, (u64)fd, (u64)buffer, count);
}

int open(const char *path, int flags) {
    return (int)syscall3(SYS_OPEN, (u64)path, (u64)flags, 0);
}

int close(int fd) {
    return (int)syscall1(SYS_CLOSE, (u64)fd);
}

long lseek(int fd, long offset, int whence) {
    return (long)syscall3(SYS_LSEEK, (u64)fd, (u64)offset, (u64)whence);
}

long getdents64(int fd, void *buffer, size_t count) {
    return (long)syscall3(SYS_GETDENTS64, (u64)fd, (u64)buffer, count);
}

int mkdir(const char *path) {
    return (int)syscall2(SYS_MKDIR, (u64)path, 0);
}

int rmdir(const char *path) {
    return (int)syscall1(SYS_RMDIR, (u64)path);
}

int unlink(const char *path) {
    return (int)syscall1(SYS_UNLINK, (u64)path);
}

int chdir(const char *path) {
    return (int)syscall1(SYS_CHDIR, (u64)path);
}

long getcwd(char *buffer, size_t size) {
    return (long)syscall2(SYS_GETCWD, (u64)buffer, size);
}

int spawn(const char *path, char *const argv[]) {
    return (int)syscall2(SYS_SPAWN, (u64)path, (u64)argv);
}

int wait4(int pid, int *status, int options, void *rusage) {
    return (int)syscall6(SYS_WAIT4, (u64)(i64)pid, (u64)status, (u64)options, (u64)rusage, 0, 0);
}

int tty_mode(int fd, int raw) {
    return (int)syscall2(SYS_TTY_MODE, (u64)fd, (u64)raw);
}

int tty_foreground(int fd, int pid) {
    return (int)syscall2(SYS_TTY_FOREGROUND, (u64)fd, (u64)pid);
}

int sleep_ticks(u64 ticks) {
    return (int)syscall1(SYS_SLEEP_TICKS, ticks);
}

int getpid(void) {
    return (int)syscall1(SYS_GETPID, 0);
}

void exit(int status) {
    syscall1(SYS_EXIT, (u64)status);
    for (;;) {
        __asm__ volatile("ud2");
    }
}
