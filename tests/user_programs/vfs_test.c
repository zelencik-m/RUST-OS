#include "syscalls.h"

enum {
    O_RDWR = 0x0002,
    O_CREAT = 0x0040,
    O_TRUNC = 0x0200,
    SEEK_SET = 0,
    EEXIST = 17,
};

static char read_buffer[64];
static const char file_message[] = "Hello from a user ELF!";

static int bytes_equal(const char *left, const char *right, usize length) {
    for (usize index = 0; index < length; ++index) {
        if (left[index] != right[index]) {
            return 0;
        }
    }
    return 1;
}

__attribute__((noreturn)) static void fail(void) {
    WRITE_LITERAL(2, "VFS test failed\n");
    exit_process(1);
}

__attribute__((noreturn)) void _start(void) {
    i64 mkdir_result = (i64)syscall2(SYS_MKDIR, (u64)"/tmp", 0);
    if (mkdir_result != 0 && mkdir_result != -EEXIST) {
        fail();
    }

    i64 fd = (i64)syscall3(
        SYS_OPEN,
        (u64)"/tmp/hello.txt",
        O_RDWR | O_CREAT | O_TRUNC,
        0
    );
    if (fd < 0) {
        fail();
    }

    usize message_length = sizeof(file_message) - 1;
    if (syscall3(SYS_WRITE, (u64)fd, (u64)file_message, message_length)
        != message_length) {
        fail();
    }

    if (syscall3(SYS_LSEEK, (u64)fd, 0, SEEK_SET) != 0) {
        fail();
    }

    if (syscall3(SYS_READ, (u64)fd, (u64)read_buffer, message_length)
        != message_length) {
        fail();
    }

    if (!bytes_equal(read_buffer, file_message, message_length)) {
        fail();
    }

    if ((i64)syscall1(SYS_CLOSE, (u64)fd) < 0) {
        fail();
    }

    WRITE_LITERAL(1, "VFS test passed\n");
    exit_process(0);
}
