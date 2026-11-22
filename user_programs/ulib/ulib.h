/*
 * ulib: the small C library rust-os programs link against.
 *
 * A "Ulib" program (see PROGRAMS in build.rs) defines `int main(int argc,
 * char **argv)` and is linked with crt0.o, syscalls.o, string.o and stdio.o.
 * crt0's `_start` reads argc/argv from the initial stack and turns main's
 * return value into the process exit code.
 *
 * Error convention: the syscall wrappers return the kernel's raw result, so a
 * failure is a negative errno (e.g. -ENOENT). There is no global `errno`.
 * `strerror(-ENOENT)` gives the message.
 */
#ifndef RUST_OS_ULIB_H
#define RUST_OS_ULIB_H

#include <stdarg.h>
#include <stddef.h>

/* u64/i64/usize, syscall6..syscall1 and the SYS_* numbers. */
#include "syscalls.h"

typedef long ssize_t;

/* ---- constants (Linux values) ------------------------------------------ */

enum {
    O_RDONLY = 0x0000,
    O_WRONLY = 0x0001,
    O_RDWR = 0x0002,
    O_CREAT = 0x0040,
    O_TRUNC = 0x0200,
    O_APPEND = 0x0400,

    SEEK_SET = 0,
    SEEK_CUR = 1,
    SEEK_END = 2,

    STDIN_FILENO = 0,
    STDOUT_FILENO = 1,
    STDERR_FILENO = 2,

    /* linux_dirent64.d_type */
    DT_CHR = 2,
    DT_DIR = 4,
    DT_REG = 8,

    /* tty_mode */
    TTY_COOKED = 0,
    TTY_RAW = 1,
};

/* errno values the kernel returns (negated). */
enum {
    ESRCH = 3,
    EINTR = 4,
    EIO = 5,
    E2BIG = 7,
    ENOEXEC = 8,
    EBADF = 9,
    ECHILD = 10,
    ENOMEM = 12,
    EACCES = 13,
    EFAULT = 14,
    EBUSY = 16,
    EEXIST = 17,
    ENOTDIR = 20,
    EISDIR = 21,
    EINVAL = 22,
    EMFILE = 24,
    ENOTTY = 25,
    ERANGE = 34,
    ENAMETOOLONG = 36,
    ENOSYS = 38,
    ENOTEMPTY = 39,
    ENOENT = 2,
};

/* Exit codes the kernel uses for killed processes. */
enum {
    EXIT_INTERRUPTED = 130, /* Ctrl-C */
    EXIT_FAULT = 139,       /* CPU exception in user mode */
};

/* One getdents64 record. Records are d_reclen bytes long (8-aligned). */
struct linux_dirent64 {
    u64 d_ino;
    i64 d_off;
    unsigned short d_reclen;
    unsigned char d_type;
    char d_name[];
} __attribute__((packed));

/* ---- syscalls.c ------------------------------------------------------- */

ssize_t read(int fd, void *buffer, size_t count);
ssize_t write(int fd, const void *buffer, size_t count);
int open(const char *path, int flags);
int close(int fd);
long lseek(int fd, long offset, int whence);
long getdents64(int fd, void *buffer, size_t count);
int mkdir(const char *path);
int rmdir(const char *path);
int unlink(const char *path);
int chdir(const char *path);
/* Copies the cwd (NUL-terminated) into buffer; returns its length including
 * the NUL, or -ERANGE if it doesn't fit. */
long getcwd(char *buffer, size_t size);
/* Start `path` as a child with this NULL-terminated argv; returns its pid. */
int spawn(const char *path, char *const argv[]);
/* Wait for child `pid`; *status gets its plain exit code (0-255). Returns
 * `pid`. Only pid > 0 and options == 0 are supported. */
int wait4(int pid, int *status, int options, void *rusage);
/* 1 = raw, 0 = cooked; returns the previous mode. */
int tty_mode(int fd, int raw);
/* The process Ctrl-C kills (0 = none); returns the previous one. */
int tty_foreground(int fd, int pid);
int sleep_ticks(u64 ticks);
int getpid(void);
__attribute__((noreturn)) void exit(int status);

/* ---- string.c --------------------------------------------------------- */

size_t strlen(const char *s);
int strcmp(const char *a, const char *b);
int strncmp(const char *a, const char *b, size_t n);
char *strcpy(char *dst, const char *src);
char *strncpy(char *dst, const char *src, size_t n);
char *strcat(char *dst, const char *src);
char *strchr(const char *s, int c);
char *strrchr(const char *s, int c);
/* clang emits calls to these four on its own (struct copies, array
 * initializers), so every Ulib program needs them. */
void *memcpy(void *dst, const void *src, size_t n);
void *memmove(void *dst, const void *src, size_t n);
void *memset(void *dst, int c, size_t n);
int memcmp(const void *a, const void *b, size_t n);
/* Decimal, optional leading '-'; stops at the first non-digit. */
long atol(const char *s);
int atoi(const char *s);
/* Message for an errno; accepts ENOENT or -ENOENT. */
const char *strerror(int error);

/* ---- stdio.c ---------------------------------------------------------- */

/* Formats: %d %i %u %x %X %c %s %p %%, the length modifiers l/ll/z (64-bit),
 * a field width (digits, or '*' to take it from the arguments), '-'
 * (left-justify) and '0' (zero-pad). No floating point.
 * printf/dprintf return the number of bytes written, or a negative errno. */
int vsnprintf(char *buffer, size_t size, const char *format, va_list args);
int snprintf(char *buffer, size_t size, const char *format, ...)
    __attribute__((format(printf, 3, 4)));
int vdprintf(int fd, const char *format, va_list args);
int dprintf(int fd, const char *format, ...) __attribute__((format(printf, 2, 3)));
int printf(const char *format, ...) __attribute__((format(printf, 1, 2)));
/* Writes s and a newline. */
int puts(const char *s);
int putchar(int c);

#endif
