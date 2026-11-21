/*
 * Phase 3 child program, started by spawn_test.c and tests/process_lifecycle.rs.
 * Installed as /bin/proc_child in the tests' RamFs.
 *
 * It reads argc/argv from its initial stack (no crt0 yet), so it is also
 * the end-to-end test of the argv layout. argv[1] picks the mode:
 *
 *   echo ARGS...     print "argv:" + every argv entry joined by '|', exit 0
 *   cwd              print "cwd:" + the working directory, exit 0
 *   exit N           exit(N)
 *   nap TICKS N      sleep TICKS timer ticks, then exit(N)
 *   spin             loop forever without syscalls (killable only by the timer)
 *   sleep            sleep forever
 *   read             block in read(0) forever (only a kill ends it)
 *   fault            write to address 0 (killed with exit code 139)
 *   orphan MODE...   spawn "proc_child MODE..." and print "child=PID",
 *                    sleep 30 ticks, exit 0 without waiting for it
 *   waiter MODE...   spawn "proc_child MODE...", print "child=PID", wait4
 *                    for it and exit with its status
 *
 * argc == 0 (spawned with a NULL argv) exits 96 without output. Anything
 * unexpected is reported on stderr, which fails the test that started it.
 */
#include "syscalls.h"

/* Entry: rsp points at argc (see src/process/initial_stack.rs). Pass that
 * pointer to C before anything else touches the stack. */
__asm__(
    ".globl _start\n"
    "_start:\n"
    "    mov %rsp, %rdi\n"
    "    and $-16, %rsp\n"
    "    call proc_child_main\n"
    "    ud2\n");

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

static void print_number(u64 fd, u64 value) {
    char digits[24];
    usize position = sizeof(digits);
    digits[--position] = '\0';
    do {
        digits[--position] = (char)('0' + value % 10);
        value /= 10;
    } while (value != 0);
    print_to(fd, &digits[position]);
}

static u64 parse_number(const char *text) {
    u64 value = 0;
    for (usize i = 0; text[i] >= '0' && text[i] <= '9'; ++i) {
        value = value * 10 + (u64)(text[i] - '0');
    }
    return value;
}

__attribute__((noreturn)) static void fail(const char *message) {
    print_to(2, "proc_child: FAILED: ");
    print_to(2, message);
    print_to(2, "\n");
    exit_process(1);
}

__attribute__((noreturn)) void proc_child_main(u64 *stack) {
    /* The System V ABI requires a 16-byte aligned rsp at process entry. */
    if (((u64)stack & 15) != 0) {
        fail("initial rsp is not 16-byte aligned");
    }

    u64 argc = stack[0];
    char **argv = (char **)&stack[1];
    if (argc == 0) {
        exit_process(96);
    }
    if (argv[argc] != 0) {
        fail("argv[argc] is not NULL");
    }
    if (argv[argc + 1] != 0) {
        fail("envp is not empty");
    }
    if (argc < 2) {
        fail("no mode given");
    }

    const char *mode = argv[1];

    if (strings_equal(mode, "echo")) {
        print_to(1, "argv:");
        for (u64 i = 0; i < argc; ++i) {
            if (i > 0) {
                print_to(1, "|");
            }
            print_to(1, argv[i]);
        }
        print_to(1, "\n");
        exit_process(0);
    }

    if (strings_equal(mode, "cwd")) {
        char buffer[256];
        i64 n = (i64)syscall2(SYS_GETCWD, (u64)buffer, sizeof(buffer));
        if (n <= 0) {
            fail("getcwd failed");
        }
        print_to(1, "cwd:");
        print_to(1, buffer);
        print_to(1, "\n");
        exit_process(0);
    }

    if (strings_equal(mode, "exit") && argc == 3) {
        exit_process(parse_number(argv[2]));
    }

    if (strings_equal(mode, "nap") && argc == 4) {
        syscall1(SYS_SLEEP_TICKS, parse_number(argv[2]));
        exit_process(parse_number(argv[3]));
    }

    if (strings_equal(mode, "spin")) {
        for (;;) {
            __asm__ volatile("" ::: "memory");
        }
    }

    if (strings_equal(mode, "sleep")) {
        for (;;) {
            syscall1(SYS_SLEEP_TICKS, 1000000);
        }
    }

    if (strings_equal(mode, "read")) {
        char buffer[64];
        syscall3(SYS_READ, 0, (u64)buffer, sizeof(buffer));
        fail("read(0) returned, but only a kill should end it");
    }

    if (strings_equal(mode, "fault")) {
        *(volatile u64 *)0 = 1;
        fail("writing to address 0 did not fault");
    }

    int orphan = strings_equal(mode, "orphan");
    if ((orphan || strings_equal(mode, "waiter")) && argc >= 3) {
        /* argv[1..] is "orphan MODE ...": reuse it as the child's argv,
         * with argv[1] replaced by the program name. */
        argv[1] = "proc_child";
        i64 child = (i64)syscall2(SYS_SPAWN, (u64)"/bin/proc_child", (u64)&argv[1]);
        if (child <= 0) {
            fail("spawn failed");
        }
        print_to(1, "child=");
        print_number(1, (u64)child);
        print_to(1, "\n");
        if (orphan) {
            syscall1(SYS_SLEEP_TICKS, 30);
            exit_process(0);
        }
        int status = -1;
        i64 waited = (i64)syscall6(SYS_WAIT4, (u64)child, (u64)&status, 0, 0, 0, 0);
        if (waited != child) {
            /* A kill (Ctrl-C) makes wait4 fail with EINTR, but the process
             * exits before wait4 returns to user mode, so this never runs. */
            fail("wait4 failed");
        }
        exit_process((u64)status);
    }

    fail("unknown mode");
}
