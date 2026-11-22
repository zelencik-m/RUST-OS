/*
 * Phase 3 ring-3 test: spawn, wait4, exit codes, argv, stdio and cwd
 * inheritance, tty_foreground and Ctrl-C.
 *
 * In tests/userspace.rs, fd 1 and fd 2 are RamFs capture files and fd 0 is
 * the console. /bin/proc_child (tests/user_programs/proc_child.c) is in the
 * RamFs. The children inherit fd 1, so their output lands in our stdout,
 * in order. The runner checks for:
 *
 *   argv:proc_child|echo|hi\ncwd:/bin\nspawn_test: passed\n
 *
 * The runner types Ctrl-C once this program is blocked and a foreground
 * process is set: that's the wait4 in step 1, after tty_foreground (it can
 * block earlier, on the filesystem lock inside spawn). This is why the
 * Ctrl-C test comes first. Failures go to stderr, which fails the suite.
 */
#include "syscalls.h"

enum {
    ESRCH = 3,
    E2BIG = 7,
    ENOEXEC = 8,
    ECHILD = 10,
    EACCES = 13,
    EFAULT = 14,
    EINVAL = 22,
    ENOENT = 2,
    ENOTTY = 25,

    O_WRONLY = 0x0001,
    O_CREAT = 0x0040,
};

#define CHILD "/bin/proc_child"

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

__attribute__((noreturn)) static void fail(const char *what, i64 got, i64 expected) {
    print_to(2, "spawn_test: FAILED: ");
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

static void expect_positive(const char *what, i64 got) {
    if (got <= 0) {
        fail(what, got, 1);
    }
}

static i64 spawn(const char *path, const char *const *argv) {
    return (i64)syscall2(SYS_SPAWN, (u64)path, (u64)argv);
}

static i64 wait4(i64 pid, int *status, u64 options) {
    return (i64)syscall6(SYS_WAIT4, (u64)pid, (u64)status, options, 0, 0, 0);
}

static i64 tty_foreground(u64 fd, i64 pid) {
    return (i64)syscall2(SYS_TTY_FOREGROUND, fd, (u64)pid);
}

/* Spawn CHILD with argv, wait for it, and check its exit status. */
static void run_and_expect(const char *what, const char *const *argv, int expected_status) {
    i64 child = spawn(CHILD, argv);
    expect_positive(what, child);
    int status = -1;
    expect("wait4", wait4(child, &status, 0), child);
    expect(what, status, expected_status);
}

static char long_arg[4001];

__attribute__((noreturn)) void _start(void) {
    i64 self = (i64)syscall1(SYS_GETPID, 0);
    int status;

    /* 1. Ctrl-C kills the foreground child. The runner types it once we
     *    block in wait4. The child is blocked in read(0) by then, or about
     *    to be; either way the kill wins. */
    const char *const read_argv[] = {"proc_child", "read", 0};
    i64 reader = spawn(CHILD, read_argv);
    expect_positive("spawn(read)", reader);
    expect("tty_foreground(1 = RamFs file)", tty_foreground(1, reader), -ENOTTY);
    expect("tty_foreground(not a child)", tty_foreground(0, 99999), -ESRCH);
    expect("tty_foreground(self)", tty_foreground(0, self), -ESRCH);
    expect("tty_foreground(0, reader)", tty_foreground(0, reader), 0);
    status = -1;
    expect("wait4(reader)", wait4(reader, &status, 0), reader);
    expect("status after Ctrl-C", status, 130);
    /* wait4 cleared the foreground, so there is no previous one. */
    expect("tty_foreground(0, 0) after wait4", tty_foreground(0, 0), 0);

    /* 2. argv reaches the child, and it writes into our (shared) stdout. */
    const char *const echo_argv[] = {"proc_child", "echo", "hi", 0};
    run_and_expect("echo child", echo_argv, 0);

    /* 3. The child inherits the cwd, and a relative path is resolved in it. */
    expect("chdir(/bin)", (i64)syscall1(SYS_CHDIR, (u64)"/bin"), 0);
    const char *const cwd_argv[] = {"proc_child", "cwd", 0};
    i64 cwd_child = spawn("proc_child", cwd_argv);
    expect_positive("spawn(relative path)", cwd_child);
    expect("wait4(cwd child)", wait4(cwd_child, &status, 0), cwd_child);
    expect("cwd child status", status, 0);
    expect("chdir(/)", (i64)syscall1(SYS_CHDIR, (u64)"/"), 0);

    /* 4. Exit codes, truncated to 8 bits like Linux. */
    const char *const exit42[] = {"proc_child", "exit", "42", 0};
    run_and_expect("exit(42)", exit42, 42);
    const char *const exit300[] = {"proc_child", "exit", "300", 0};
    run_and_expect("exit(300)", exit300, 300 & 0xff);
    const char *const fault_argv[] = {"proc_child", "fault", 0};
    run_and_expect("faulting child", fault_argv, 139);

    /* 5. A collected child can't be waited for again; neither can
     *    processes that aren't our children. */
    const char *const exit3[] = {"proc_child", "exit", "3", 0};
    i64 child = spawn(CHILD, exit3);
    expect_positive("spawn(exit 3)", child);
    /* A bad status pointer fails before anything is collected... */
    expect("wait4(bad status pointer)", wait4(child, (int *)0x10, 0), -EFAULT);
    /* ...so the child can still be collected, here with a NULL status. */
    expect("wait4(NULL status)", wait4(child, 0, 0), child);
    expect("wait4(collected child)", wait4(child, &status, 0), -ECHILD);
    expect("wait4(self)", wait4(self, &status, 0), -ECHILD);
    expect("wait4(no such pid)", wait4(99999, &status, 0), -ECHILD);
    expect("wait4(-1)", wait4(-1, &status, 0), -EINVAL);
    expect("wait4(options = 1)", wait4(1, &status, 1), -EINVAL);

    /* 6. A NULL argv starts the child with argc 0 (it exits 96). */
    i64 no_args = spawn(CHILD, 0);
    expect_positive("spawn(NULL argv)", no_args);
    expect("wait4(no-args child)", wait4(no_args, &status, 0), no_args);
    expect("argc-0 child status", status, 96);

    /* 7. spawn errors. */
    expect("spawn(missing file)", spawn("/bin/nope", exit3), -ENOENT);
    expect("spawn(directory)", spawn("/bin", exit3), -EACCES);
    expect("spawn(bad path pointer)", spawn((const char *)0x10, exit3), -EFAULT);
    expect("spawn(bad argv pointer)", spawn(CHILD, (const char *const *)0x10), -EFAULT);

    i64 fd = (i64)syscall3(SYS_OPEN, (u64)"/spawn_test.txt", O_WRONLY | O_CREAT, 0);
    expect_positive("open(/spawn_test.txt)", fd);
    write_bytes((u64)fd, "not an ELF\n", 11);
    syscall1(SYS_CLOSE, (u64)fd);
    expect("spawn(text file)", spawn("/spawn_test.txt", exit3), -ENOEXEC);
    expect("unlink(/spawn_test.txt)", (i64)syscall1(SYS_UNLINK, (u64)"/spawn_test.txt"), 0);

    const char *too_many[34];
    for (int i = 0; i < 33; ++i) {
        too_many[i] = "x";
    }
    too_many[33] = 0;
    expect("spawn(33 arguments)", spawn(CHILD, too_many), -E2BIG);

    /* Five 4000-byte arguments don't fit in the 16 KiB argv image. */
    for (int i = 0; i < 4000; ++i) {
        long_arg[i] = 'a';
    }
    long_arg[4000] = '\0';
    const char *const huge[] = {long_arg, long_arg, long_arg, long_arg, long_arg, 0};
    expect("spawn(20 KB of arguments)", spawn(CHILD, huge), -E2BIG);

    print_to(1, "spawn_test: passed\n");
    exit_process(0);
}
