#include "syscalls.h"

enum {
    O_RDONLY = 0x0000,
    O_WRONLY = 0x0001,
    O_CREAT = 0x0040,
    O_TRUNC = 0x0200,
    DT_DIR = 4,
    EEXIST = 17,
    SEEK_SET = 0,
    MAX_TREE_DEPTH = 64,
};

struct linux_dirent64 {
    u64 d_ino;
    i64 d_off;
    unsigned short d_reclen;
    unsigned char d_type;
    char d_name[];
} __attribute__((packed));

static usize string_length(const char *text) {
    usize length = 0;
    while (text[length] != '\0') {
        ++length;
    }
    return length;
}

static void print(const char *text) {
    write_bytes(1, text, string_length(text));
}

static void make_directory(const char *path) {
    i64 result = (i64)syscall2(SYS_MKDIR, (u64)path, 0);
    if (result < 0 && result != -EEXIST) {
        print("fs_tree_test: mkdir failed: ");
        print(path);
        print("\n");
        exit_process(1);
    }
}

static void make_file(const char *path, const char *contents) {
    i64 fd = (i64)syscall3(
        SYS_OPEN, (u64)path, O_WRONLY | O_CREAT | O_TRUNC, 0
    );
    if (fd < 0) {
        print("fs_tree_test: open failed: ");
        print(path);
        print("\n");
        exit_process(1);
    }

    usize length = string_length(contents);
    if ((i64)syscall3(SYS_WRITE, (u64)fd, (u64)contents, length) != (i64)length) {
        print("fs_tree_test: write failed: ");
        print(path);
        print("\n");
        exit_process(1);
    }
    syscall1(SYS_CLOSE, (u64)fd);
}

static int is_dot_entry(const char *name) {
    return name[0] == '.' &&
        (name[1] == '\0' || (name[1] == '.' && name[2] == '\0'));
}

static int join_path(char *output, usize capacity, const char *parent, const char *name) {
    usize position = 0;
    while (parent[position] != '\0') {
        if (position + 1 >= capacity) return 0;
        output[position] = parent[position];
        ++position;
    }
    if (position != 1 || output[0] != '/') {
        if (position + 1 >= capacity) return 0;
        output[position++] = '/';
    }
    for (usize index = 0; name[index] != '\0'; ++index) {
        if (position + 1 >= capacity) return 0;
        output[position++] = name[index];
    }
    output[position] = '\0';
    return 1;
}

static void print_tree_prefix(
    const unsigned char *continued,
    unsigned depth,
    int is_last
) {
    for (unsigned level = 0; level < depth; ++level) {
        print(continued[level] ? "|   " : "    ");
    }
    print(is_last ? "`-- " : "|-- ");
}

static usize count_directory_entries(i64 fd, int *valid) {
    usize total = 0;
    *valid = 1;

    for (;;) {
        char buffer[1024];
        i64 count = (i64)syscall3(
            SYS_GETDENTS64, (u64)fd, (u64)buffer, sizeof(buffer)
        );
        if (count < 0) {
            *valid = 0;
            break;
        }
        if (count == 0) break;

        usize offset = 0;
        while (offset < (usize)count) {
            struct linux_dirent64 *entry =
                (struct linux_dirent64 *)(buffer + offset);
            if (entry->d_reclen < 20 || offset + entry->d_reclen > (usize)count) {
                *valid = 0;
                return 0;
            }
            if (!is_dot_entry(entry->d_name)) ++total;
            offset += entry->d_reclen;
        }
    }

    if ((i64)syscall3(SYS_LSEEK, (u64)fd, 0, SEEK_SET) != 0) {
        *valid = 0;
    }
    return total;
}

static void print_directory(
    const char *path,
    unsigned depth,
    unsigned char *continued
) {
    i64 fd = (i64)syscall3(SYS_OPEN, (u64)path, O_RDONLY, 0);
    if (fd < 0) {
        print_tree_prefix(continued, depth, 1);
        print("[cannot open directory]\n");
        return;
    }

    int valid;
    usize total = count_directory_entries(fd, &valid);
    if (!valid) {
        print_tree_prefix(continued, depth, 1);
        print("[getdents64 failed]\n");
        syscall1(SYS_CLOSE, (u64)fd);
        return;
    }

    usize seen = 0;
    for (;;) {
        char buffer[1024];
        i64 count = (i64)syscall3(SYS_GETDENTS64, (u64)fd, (u64)buffer, sizeof(buffer));
        if (count < 0) {
            print_tree_prefix(continued, depth, 1);
            print("[getdents64 failed]\n");
            break;
        }
        if (count == 0) break;

        usize offset = 0;
        while (offset < (usize)count) {
            struct linux_dirent64 *entry =
                (struct linux_dirent64 *)(buffer + offset);
            if (entry->d_reclen < 20 || offset + entry->d_reclen > (usize)count) {
                print_tree_prefix(continued, depth, 1);
                print("[invalid directory record]\n");
                syscall1(SYS_CLOSE, (u64)fd);
                return;
            }

            if (!is_dot_entry(entry->d_name)) {
                ++seen;
                int is_last = seen == total;
                print_tree_prefix(continued, depth, is_last);
                print(entry->d_name);
                if (entry->d_type == DT_DIR) print("/");
                print("\n");

                if (entry->d_type == DT_DIR) {
                    char child_path[512];
                    if (depth + 1 >= MAX_TREE_DEPTH) {
                        continued[depth] = 0;
                        print_tree_prefix(continued, depth + 1, 1);
                        print("[maximum depth reached]\n");
                    } else if (join_path(child_path, sizeof(child_path), path, entry->d_name)) {
                        continued[depth] = !is_last;
                        print_directory(child_path, depth + 1, continued);
                    } else {
                        continued[depth] = 0;
                        print_tree_prefix(continued, depth + 1, 1);
                        print("[path too long]\n");
                    }
                }
            }
            offset += entry->d_reclen;
        }
    }
    syscall1(SYS_CLOSE, (u64)fd);
}

__attribute__((noreturn)) void _start(void) {
    make_directory("/fs_tree_test");
    make_directory("/fs_tree_test/docs");
    make_directory("/fs_tree_test/src");
    make_directory("/fs_tree_test/src/assets");
    make_file("/fs_tree_test/README.txt", "filesystem tree test\n");
    make_file("/fs_tree_test/docs/notes.txt", "directory listing works\n");
    make_file("/fs_tree_test/src/main.c", "int main(void) { return 0; }\n");
    make_file("/fs_tree_test/src/assets/logo.txt", "[rust-os]\n");

    print("\nFilesystem tree from /:\n/\n");
    unsigned char continued[MAX_TREE_DEPTH] = {0};
    print_directory("/", 0, continued);
    print("fs_tree_test: passed\n");
    exit_process(0);
}
