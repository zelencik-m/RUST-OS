/*
 * Phase 2 ring-3 test: SYS_TTY_MODE and a blocking read(0) on the console.
 *
 * In tests/userspace.rs, fd 1 and fd 2 are RamFs capture files and fd 0 is
 * the console. Once this program blocks in read(0), the runner types
 * "hello\n" and then Ctrl-D. Failures go to stderr, which fails the suite.
 */
#include "syscalls.h"

enum {
    EBADF = 9,
    EINVAL = 22,
    ENOTTY = 25,
    TTY_COOKED = 0,
    TTY_RAW = 1,
};

static usize string_length(const char *text) {
    usize length = 0;
    while (text[length] != '\0') {
        ++length;
    }
    return length;
}

static void print_to(u64 fd, const char *text) {
    write_bytes(fd, text, string_length(text));
}

static void print_number(u64 fd, i64 value) {
    char digits[24];
    usize position = sizeof(digits);
    int negative = value < 0;
    u64 magnitude = negative ? (u64)(-value) : (u64)value;
    digits[--position] = '\0';
    do {
        digits[--position] = (char)('0' + magnitude % 10);
        magnitude /= 10;
    } while (magnitude != 0);
    if (negative) {
        digits[--position] = '-';
    }
    print_to(fd, &digits[position]);
}

static void expect(const char *what, i64 got, i64 expected) {
    if (got != expected) {
        print_to(2, "tty_test: FAILED: ");
        print_to(2, what);
        print_to(2, " returned ");
        print_number(2, got);
        print_to(2, ", expected ");
        print_number(2, expected);
        print_to(2, "\n");
        exit_process(1);
    }
}

static i64 tty_mode(u64 fd, u64 mode) {
    return (i64)syscall2(SYS_TTY_MODE, fd, mode);
}

static i64 read_fd(u64 fd, char *buffer, usize length) {
    return (i64)syscall3(SYS_READ, fd, (u64)buffer, length);
}

__attribute__((noreturn)) void _start(void) {
    /* Only the console is a terminal. */
    expect("tty_mode(1 = RamFs file)", tty_mode(1, TTY_RAW), -ENOTTY);
    expect("tty_mode(99 = no such fd)", tty_mode(99, TTY_RAW), -EBADF);

    /* Each call returns the previous mode; bad modes change nothing. */
    expect("tty_mode(0, raw)", tty_mode(0, TTY_RAW), TTY_COOKED);
    expect("tty_mode(0, 7)", tty_mode(0, 7), -EINVAL);
    expect("tty_mode(0, cooked)", tty_mode(0, TTY_COOKED), TTY_RAW);

    /* Blocks until the runner types "hello\n" (then Ctrl-D). A 100-byte
     * buffer: a cooked read stops at the end of the line anyway. */
    char buffer[100];
    i64 n = read_fd(0, buffer, sizeof(buffer));
    expect("read(0) length", n, 6);
    const char *expected = "hello\n";
    for (int i = 0; i < 6; ++i) {
        if (buffer[i] != expected[i]) {
            print_to(2, "tty_test: FAILED: read(0) returned the wrong bytes\n");
            exit_process(1);
        }
    }

    /* Ctrl-D on an empty line: EOF. */
    expect("read(0) after Ctrl-D", read_fd(0, buffer, sizeof(buffer)), 0);

    print_to(1, "tty_test: passed\n");
    exit_process(0);
}
