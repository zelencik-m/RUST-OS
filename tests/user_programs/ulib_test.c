/*
 * Phase 4: checks ulib from ring 3 (crt0, printf, string functions, syscall
 * wrappers). Started by tests/userspace.rs with argv ["ulib_test"], and
 * installed there as /bin/ulib_test so it can spawn itself:
 *
 *   ulib_test            run every check, print "ulib_test: passed"
 *   ulib_test ret N      return N from main (crt0 makes it the exit code)
 *
 * Failures go to stderr, which fails the test.
 */
#include <limits.h>
#include "ulib.h"

static int failures;

static void fail(const char *what, const char *got, const char *want) {
    dprintf(STDERR_FILENO, "ulib_test: FAILED: %s: got \"%s\", want \"%s\"\n", what, got, want);
    failures++;
}

static void check(int ok, const char *what) {
    if (!ok) {
        dprintf(STDERR_FILENO, "ulib_test: FAILED: %s\n", what);
        failures++;
    }
}

/* snprintf into a buffer and compare with `want`, and check the return
 * value is the full length. */
#define EXPECT_FORMAT(want, ...)                                          \
    do {                                                                  \
        char got_[128];                                                   \
        int n_ = snprintf(got_, sizeof(got_), __VA_ARGS__);               \
        if (strcmp(got_, (want)) != 0) {                                  \
            fail(#__VA_ARGS__, got_, (want));                             \
        } else if (n_ != (int)strlen(want)) {                             \
            fail(#__VA_ARGS__ " (return value)", got_, (want));           \
        }                                                                 \
    } while (0)

static void test_format(void) {
    EXPECT_FORMAT("plain", "plain");
    EXPECT_FORMAT("42", "%d", 42);
    EXPECT_FORMAT("-42", "%d", -42);
    EXPECT_FORMAT("0", "%d", 0);
    EXPECT_FORMAT("-2147483648", "%d", INT_MIN);
    EXPECT_FORMAT("2147483647", "%i", INT_MAX);
    EXPECT_FORMAT("4294967295", "%u", UINT_MAX);
    EXPECT_FORMAT("-9223372036854775808", "%ld", LONG_MIN);
    EXPECT_FORMAT("18446744073709551615", "%lu", ULONG_MAX);
    EXPECT_FORMAT("0", "%x", 0u);
    EXPECT_FORMAT("ff", "%x", 255u);
    EXPECT_FORMAT("DEADBEEF", "%X", 0xdeadbeefu);
    EXPECT_FORMAT("7f8000100000", "%lx", 0x7f8000100000ul);
    EXPECT_FORMAT("0x1000", "%p", (void *)0x1000);
    EXPECT_FORMAT("c", "%c", 'c');
    EXPECT_FORMAT("hi there", "%s %s", "hi", "there");
    EXPECT_FORMAT("(null)", "%s", (char *)NULL);
    EXPECT_FORMAT("100%", "%d%%", 100);
    EXPECT_FORMAT("[   7]", "[%4d]", 7);
    EXPECT_FORMAT("[7   ]", "[%-4d]", 7);
    EXPECT_FORMAT("[0007]", "[%04d]", 7);
    EXPECT_FORMAT("[-007]", "[%04d]", -7);
    EXPECT_FORMAT("[  ab]", "[%4s]", "ab");
    EXPECT_FORMAT("[ab  ]", "[%-4s]", "ab");
    EXPECT_FORMAT("[toolong]", "[%3s]", "toolong");
    EXPECT_FORMAT("[   ab]", "[%*s]", 5, "ab");
    EXPECT_FORMAT("[ab   ]", "[%*s]", -5, "ab");
    EXPECT_FORMAT("[007]", "[%0*d]", 3, 7);
    EXPECT_FORMAT("[]", "[%*s]", 0, "");
    EXPECT_FORMAT("pid=12 status=0", "pid=%d status=%d", 12, 0);
    /* Malformed formats on purpose: printed as written. */
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wformat"
    EXPECT_FORMAT("%q", "%q");
    EXPECT_FORMAT("end%", "end%");
#pragma clang diagnostic pop

    /* Truncation: at most size-1 characters plus a NUL; the return value
     * is the length the output would have had. */
    char small[5];
    memset(small, 'x', sizeof(small));
    int n = snprintf(small, sizeof(small), "%s", "abcdefgh");
    check(n == 8, "snprintf returns the untruncated length");
    check(memcmp(small, "abcd", 5) == 0, "snprintf truncates and NUL-terminates");

    char untouched = 'u';
    check(snprintf(&untouched, 0, "abc") == 3, "snprintf with size 0 returns the length");
    check(untouched == 'u', "snprintf with size 0 writes nothing");
}

static void test_strings(void) {
    check(strlen("") == 0 && strlen("abc") == 3, "strlen");
    check(strcmp("abc", "abc") == 0, "strcmp equal");
    check(strcmp("abc", "abd") < 0 && strcmp("abd", "abc") > 0, "strcmp order");
    check(strcmp("ab", "abc") < 0 && strcmp("abc", "ab") > 0, "strcmp prefix");
    check(strcmp("\xff", "a") > 0, "strcmp compares unsigned");
    check(strncmp("abcx", "abcy", 3) == 0 && strncmp("abcx", "abcy", 4) < 0, "strncmp");
    check(strncmp("ab", "ab", 10) == 0, "strncmp stops at NUL");

    char buffer[32];
    check(strcpy(buffer, "hello") == buffer && strcmp(buffer, "hello") == 0, "strcpy");
    strcat(buffer, " world");
    check(strcmp(buffer, "hello world") == 0, "strcat");
    check(strchr(buffer, 'o') == buffer + 4, "strchr");
    check(strrchr(buffer, 'o') == buffer + 7, "strrchr");
    check(strchr(buffer, 'z') == NULL, "strchr missing");
    check(strchr(buffer, '\0') == buffer + 11, "strchr finds the NUL");

    char padded[6];
    strncpy(padded, "ab", sizeof(padded));
    check(memcmp(padded, "ab\0\0\0\0", 6) == 0, "strncpy pads with NULs");

    check(atoi("123") == 123 && atoi("-45") == -45 && atoi("7x") == 7 && atoi("") == 0, "atoi");
    check(strcmp(strerror(-ENOENT), "No such file or directory") == 0, "strerror(-ENOENT)");
    check(strcmp(strerror(EEXIST), "File exists") == 0, "strerror(EEXIST)");

    unsigned char bytes[16];
    check(memset(bytes, 0xab, sizeof(bytes)) == bytes, "memset returns dst");
    int all_set = 1;
    for (int i = 0; i < 16; i++) {
        all_set &= bytes[i] == 0xab;
    }
    check(all_set, "memset fills");

    unsigned char copy[16];
    memcpy(copy, "0123456789abcdef", 16);
    check(memcmp(copy, "0123456789abcdef", 16) == 0, "memcpy");
    check(memcmp("abc", "abd", 3) < 0 && memcmp("abd", "abc", 3) > 0, "memcmp order");
    check(memcmp("a\x80", "a\x01", 2) > 0, "memcmp compares unsigned");

    /* Overlapping moves in both directions. */
    char overlap[16];
    memcpy(overlap, "abcdefgh", 9);
    memmove(overlap + 2, overlap, 6);
    check(memcmp(overlap, "ababcdef", 8) == 0, "memmove forward overlap");
    memcpy(overlap, "abcdefgh", 9);
    memmove(overlap, overlap + 2, 6);
    check(memcmp(overlap, "cdefghgh", 8) == 0, "memmove backward overlap");

    /* Struct copies and array initializers: clang compiles these to memcpy
     * and memset calls, which must link and work. */
    struct big {
        char text[64];
        long numbers[8];
    } a = {"struct copy", {1, 2, 3, 4, 5, 6, 7, 8}};
    struct big b = a;
    check(strcmp(b.text, "struct copy") == 0 && b.numbers[7] == 8, "struct copy");
    char zeros[100] = {0};
    int all_zero = 1;
    for (int i = 0; i < 100; i++) {
        all_zero &= zeros[i] == 0;
    }
    check(all_zero, "zero initializer");
}

/* printf to an fd: more than its 256-byte buffer must arrive in full and in
 * order. A file stands in for the fd so the bytes can be read back. */
static void test_dprintf(void) {
    const char *path = "/ulib_test.out";
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC);
    if (fd < 0) {
        fail("open", strerror(fd), "a file descriptor");
        return;
    }

    char line[601];
    for (int i = 0; i < 600; i++) {
        line[i] = (char)('a' + i % 26);
    }
    line[600] = '\0';
    int n = dprintf(fd, "<%s|%d>", line, 600);
    check(n == 606, "dprintf returns the number of bytes written");

    char back[700];
    check(lseek(fd, 0, SEEK_SET) == 0, "lseek");
    long got = read(fd, back, sizeof(back));
    check(got == 606, "dprintf wrote every byte");
    check(got == 606 && back[0] == '<' && memcmp(back + 1, line, 600) == 0
              && memcmp(back + 601, "|600>", 5) == 0,
          "dprintf output is complete and in order");

    check(close(fd) == 0, "close");
    check(unlink(path) == 0, "unlink");
    check(open(path, O_RDONLY) == -ENOENT, "the wrappers return -errno");
    check(dprintf(99, "x") == -EBADF, "dprintf to a closed fd returns -EBADF");
}

/* main's return value reaches wait4 through crt0's exit(). */
static void test_exit_code(char *self) {
    struct {
        const char *arg;
        int want;
    } cases[] = {{"0", 0}, {"7", 7}, {"300", 300 & 0xff}};
    for (int i = 0; i < 3; i++) {
        char *argv[] = {self, "ret", (char *)cases[i].arg, NULL};
        int child = spawn("/bin/ulib_test", argv);
        if (child <= 0) {
            fail("spawn", strerror(child), "a pid");
            return;
        }
        int status = -1;
        check(wait4(child, &status, 0, NULL) == child, "wait4 returns the child");
        if (status != cases[i].want) {
            char got[16];
            char want[16];
            snprintf(got, sizeof(got), "%d", status);
            snprintf(want, sizeof(want), "%d", cases[i].want);
            fail("exit code of `ulib_test ret N`", got, want);
        }
    }
}

int main(int argc, char **argv) {
    if (argc == 3 && strcmp(argv[1], "ret") == 0) {
        return atoi(argv[2]);
    }

    check(argc == 1, "argc");
    check(argv[0] != NULL && strcmp(argv[0], "ulib_test") == 0, "argv[0]");
    check(argv[argc] == NULL, "argv[argc] is NULL");
    check(getpid() > 0, "getpid");

    test_format();
    test_strings();
    test_dprintf();
    test_exit_code(argv[0]);

    if (failures != 0) {
        return 1;
    }
    puts("ulib_test: passed");
    return 0;
}
