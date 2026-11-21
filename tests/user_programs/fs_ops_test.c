
#include "syscalls.h"

enum {
    O_RDONLY = 0x0000,
    O_WRONLY = 0x0001,
    O_CREAT = 0x0040,

    ENOENT = 2,
    EFAULT = 14,
    EBUSY = 16,
    ENOTDIR = 20,
    EISDIR = 21,
    EINVAL = 22,
    ERANGE = 34,
    ENOTEMPTY = 39,
};

static usize string_length(const char *text) {
    usize length = 0;
    while (text[length] != '\0') {
        ++length;
    }
    return length;
}

static int strings_equal(const char *a, const char *b) {
    usize i = 0;
    while (a[i] != '\0' && a[i] == b[i]) {
        ++i;
    }
    return a[i] == b[i];
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

__attribute__((noreturn)) static void fail(const char *what, i64 got, i64 expected) {
    print_to(2, "fs_ops_test: FAILED: ");
    print_to(2, what);
    print_to(2, " returned ");
    print_number(2, got);
    print_to(2, ", expected ");
    print_number(2, expected);
    print_to(2, "\n");
    exit_process(1);
}

static void expect(const char *what, i64 got, i64 expected) {
    if (got != expected) {
        fail(what, got, expected);
    }
}

static i64 sys_getcwd(char *buffer, usize size) {
    return (i64)syscall2(SYS_GETCWD, (u64)buffer, size);
}

static i64 sys_chdir(const char *path) {
    return (i64)syscall1(SYS_CHDIR, (u64)path);
}

static i64 sys_mkdir(const char *path) {
    return (i64)syscall2(SYS_MKDIR, (u64)path, 0755);
}

static i64 sys_rmdir(const char *path) {
    return (i64)syscall1(SYS_RMDIR, (u64)path);
}

static i64 sys_unlink(const char *path) {
    return (i64)syscall1(SYS_UNLINK, (u64)path);
}

static i64 sys_open(const char *path, u64 flags) {
    return (i64)syscall3(SYS_OPEN, (u64)path, flags, 0644);
}

static void create_file(const char *path) {
    i64 fd = sys_open(path, O_WRONLY | O_CREAT);
    if (fd < 0) {
        fail(path, fd, 0);
    }
    syscall1(SYS_CLOSE, (u64)fd);
}

static int exists(const char *path) {
    i64 fd = sys_open(path, O_RDONLY);
    if (fd < 0) {
        return 0;
    }
    syscall1(SYS_CLOSE, (u64)fd);
    return 1;
}

static void expect_cwd(const char *expected) {
    char buffer[128];
    i64 length = sys_getcwd(buffer, sizeof(buffer));
    /* Linux getcwd returns the byte count including the NUL terminator. */
    expect("getcwd length", length, (i64)string_length(expected) + 1);
    if (!strings_equal(buffer, expected)) {
        print_to(2, "fs_ops_test: FAILED: getcwd gave \"");
        print_to(2, buffer);
        print_to(2, "\", expected \"");
        print_to(2, expected);
        print_to(2, "\"\n");
        exit_process(1);
    }
}

static void test_getcwd(void) {
    expect_cwd("/");

    char exact[2];
    expect("getcwd(exact fit)", sys_getcwd(exact, sizeof(exact)), 2);
    char tiny[1];
    expect("getcwd(size 1)", sys_getcwd(tiny, sizeof(tiny)), -ERANGE);
    expect("getcwd(size 0)", sys_getcwd(tiny, 0), -ERANGE);
    expect("getcwd(NULL)", sys_getcwd((char *)0, 64), -EFAULT);
}

static void test_unlink_and_rmdir(void) {
    expect("mkdir /fs_ops", sys_mkdir("/fs_ops"), 0);
    expect("mkdir /fs_ops/dir", sys_mkdir("/fs_ops/dir"), 0);
    create_file("/fs_ops/file");
    create_file("/fs_ops/dir/inner");

    expect("unlink(dir)", sys_unlink("/fs_ops/dir"), -EISDIR);
    expect("rmdir(file)", sys_rmdir("/fs_ops/file"), -ENOTDIR);
    expect("rmdir(non-empty)", sys_rmdir("/fs_ops/dir"), -ENOTEMPTY);
    expect("rmdir(/)", sys_rmdir("/"), -EBUSY);
    expect("unlink(missing)", sys_unlink("/fs_ops/missing"), -ENOENT);
    expect("rmdir(missing)", sys_rmdir("/fs_ops/missing"), -ENOENT);
    expect("unlink(file/x)", sys_unlink("/fs_ops/file/x"), -ENOTDIR);
    expect("unlink(\"\")", sys_unlink(""), -EINVAL);
    expect("unlink(NULL)", sys_unlink((const char *)0), -EFAULT);
    expect("rmdir(NULL)", sys_rmdir((const char *)0), -EFAULT);

    expect("unlink(inner)", sys_unlink("/fs_ops/dir/inner"), 0);
    expect("inner gone", exists("/fs_ops/dir/inner"), 0);
    expect("unlink(inner) again", sys_unlink("/fs_ops/dir/inner"), -ENOENT);
    expect("rmdir(dir)", sys_rmdir("/fs_ops/dir"), 0);
    expect("dir gone", exists("/fs_ops/dir"), 0);
    expect("unlink(file)", sys_unlink("/fs_ops/file"), 0);
    expect("file gone", exists("/fs_ops/file"), 0);
}

static void test_chdir(void) {
    /* /fs_ops still exists and is empty after test_unlink_and_rmdir. */
    create_file("/fs_ops/note");

    expect("chdir(/fs_ops)", sys_chdir("/fs_ops"), 0);
    expect_cwd("/fs_ops");

    /* Relative paths now resolve against the new cwd. */
    expect("exists(note) relative", exists("note"), 1);
    expect("mkdir(sub) relative", sys_mkdir("sub"), 0);
    expect("exists(/fs_ops/sub)", exists("/fs_ops/sub"), 1);

    expect("chdir(sub)", sys_chdir("sub"), 0);
    expect_cwd("/fs_ops/sub");
    char small[8]; /* "/fs_ops/sub" needs 12 bytes */
    expect("getcwd(too small)", sys_getcwd(small, sizeof(small)), -ERANGE);

    expect("chdir(..)", sys_chdir(".."), 0);
    expect_cwd("/fs_ops");

    /* Failed chdir calls must leave the cwd unchanged. */
    expect("chdir(file)", sys_chdir("note"), -ENOTDIR);
    expect("chdir(missing)", sys_chdir("missing"), -ENOENT);
    expect("chdir(\"\")", sys_chdir(""), -EINVAL);
    expect("chdir(NULL)", sys_chdir((const char *)0), -EFAULT);
    expect_cwd("/fs_ops");

    /* unlink/rmdir also resolve relative to the cwd. */
    expect("unlink(note) relative", sys_unlink("note"), 0);
    expect("rmdir(sub) relative", sys_rmdir("./sub"), 0);

    expect("chdir(/)", sys_chdir("/"), 0);
    expect_cwd("/");
    expect("rmdir(fs_ops)", sys_rmdir("fs_ops"), 0);
    expect("fs_ops gone", exists("/fs_ops"), 0);
}

__attribute__((noreturn)) void _start(void) {
    test_getcwd();
    test_unlink_and_rmdir();
    test_chdir();
    print_to(1, "fs_ops_test: passed\n");
    exit_process(0);
}
