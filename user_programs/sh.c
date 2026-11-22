/*
 * sh: the rust-os shell. The kernel starts it as the first user process
 * (src/main.rs) and starts a new one whenever it exits.
 *
 *   main          read a line, tokenize it, run a builtin or a program
 *   edit_line     interactive: the raw-mode line editor with history
 *   read_line     script mode: stdin is a file, one command per line
 *   tokenize      split a line into argv (whitespace, "double quotes", #)
 *   builtins      cd pwd ls mkdir rmdir rm touch cat echo clear help exit
 *   run_program   spawn /bin/<name> (or a path with '/'), wait for it
 *
 * Mode: if stdin is the console, `tty_mode(0, RAW)` succeeds and the shell
 * edits lines itself. Otherwise it fails with ENOTTY, and the shell reads
 * commands from stdin with no prompt and no echo (tests use this).
 *
 * Interactive mode uses ANSI colors and box drawing (the console maps the
 * colors to its palette, src/theme.rs, and the UTF-8 box characters to the
 * VGA font); script mode prints plain text.
 *
 * There is no malloc: every buffer is on the stack or static.
 */
#include "ulib.h"

enum {
    LINE_CAP = 256,      /* a command line, NUL included */
    MAX_TOKENS = 32,     /* the kernel's MAX_ARGS (argv[0] included) */
    PATH_CAP = 1024,
    HISTORY_SIZE = 16,
    SCREEN_WIDTH = 80,
    PROMPT_PATH_MAX = 40, /* longer working directories are shortened to "...tail" */
};

/* Set by the `exit` builtin. */
static int exit_requested;
static int exit_status;

/* Status of the last command (builtin or program); colors the prompt. */
static int last_status;

/* ---- colors ------------------------------------------------------------ */

/* ANSI colors, named for what they look like with the console's palette
 * (src/theme.rs): mint ("green"), cyan accent ("blue"), amber ("yellow"). */
#define RESET "\x1b[0m"
#define RED "\x1b[1;31m"
#define GREEN "\x1b[1;32m"
#define AMBER "\x1b[1;33m"
#define TEAL "\x1b[1;34m"
#define PALE "\x1b[1;36m"
#define DIM "\x1b[90m"
#define MID "\x1b[32m"

/* The prompt's arrow (►) and a separator dot (·), as UTF-8. */
#define ARROW "\u25BA"
#define DOT "\u00B7"
#define RULE "\u2500" /* ─ */

/* Clear the screen and go to the top-left. */
#define CLEAR_SCREEN "\x1b[2J\x1b[H"

static int use_color; /* interactive mode only */

/* `code` in interactive mode, "" in script mode. */
static const char *paint(const char *code) {
    return use_color ? code : "";
}

/* Columns `text` takes on screen: escape sequences take none, and a UTF-8
 * character takes one. */
static size_t visible_width(const char *text) {
    size_t width = 0;
    for (const char *p = text; *p != '\0'; p++) {
        if (*p == '\x1b') {
            while (*p != '\0' && !(*p >= '@' && *p <= '~' && *p != '[')) {
                p++;
            }
            if (*p == '\0') {
                break;
            }
            continue;
        }
        if (((unsigned char)*p & 0xc0) != 0x80) {
            width++;
        }
    }
    return width;
}

/* `count` horizontal rules (─) into `out`, which must hold 3 * count + 1. */
static void rule(char *out, size_t count) {
    out[0] = '\0';
    for (size_t i = 0; i < count; i++) {
        strcat(out, RULE);
    }
}

/* `count` rules (─) whose color fades through `steps` (ANSI color codes,
 * bright first), in equal parts. Plain rules in script mode. `out` must
 * hold 3 * count + 8 * steps + 1 bytes. */
static void fading_rule(char *out, size_t count, const char *const *steps, size_t step_count) {
    out[0] = '\0';
    size_t current = step_count;
    for (size_t i = 0; i < count; i++) {
        size_t step = i * step_count / count;
        if (use_color && step != current) {
            strcat(out, steps[step]);
            current = step;
        }
        strcat(out, RULE);
    }
}

/* Fades for rules: down to the frame color (to meet a box corner), or on
 * into the background (the console's two darkest gradient shades). */
static const char *const FADE_TO_FRAME[] = {"\x1b[94m", "\x1b[36m", "\x1b[90m"};
static const char *const FADE_OUT[] = {"\x1b[94m", "\x1b[36m", "\x1b[90m", "\x1b[95m", "\x1b[35m"};

/* "RUST-OS" with one color per letter, white to cyan. */
static void gradient_title(char *out, size_t capacity) {
    static const char *const shades[] = {"\x1b[1;97m", "\x1b[1;96m", "\x1b[1;96m", "\x1b[1;94m",
                                         "\x1b[1;94m", "\x1b[36m",   "\x1b[36m"};
    const char *title = "RUST-OS";
    size_t n = 0;
    out[0] = '\0';
    for (size_t i = 0; title[i] != '\0' && n + 12 < capacity; i++) {
        n += (size_t)snprintf(out + n, capacity - n, "%s%c", paint(shades[i]), title[i]);
    }
}

/* A section heading: "─[ TITLE ]────..." `width` columns wide, the rule
 * fading out. */
static void print_section(const char *title, size_t width) {
    char line[3 * SCREEN_WIDTH + 64];
    size_t used = 1 + 2 + strlen(title) + 2;
    fading_rule(line, used < width ? width - used : 0, FADE_OUT, 5);
    printf("%s" RULE "[ %s%s%s ]%s%s\n", paint(DIM), paint(GREEN), title, paint(DIM), line,
           paint(RESET));
}

/* An error message on stderr (red in interactive mode), with a newline. */
__attribute__((format(printf, 1, 2))) static void shell_error(const char *format, ...) {
    char message[LINE_CAP];
    va_list args;
    va_start(args, format);
    vsnprintf(message, sizeof(message), format, args);
    va_end(args);
    dprintf(STDERR_FILENO, "%s%s%s\n", paint(RED), message, paint(RESET));
}

static void report(const char *command, const char *what, int error) {
    shell_error("%s: %s: %s", command, what, strerror(error));
}

/* ======================================================================
 * Reading keys (interactive mode)
 * ====================================================================== */

enum {
    KEY_NONE = 0, /* a key or escape sequence the editor ignores */
    KEY_EOF = -1,
    KEY_UP = 0x100,
    KEY_DOWN,
    KEY_RIGHT,
    KEY_LEFT,
};

/* Raw input that arrived but hasn't been used yet. One read() can return
 * several keys, even "...\n" plus the start of the next command, so this
 * must survive between edit_line calls. */
static unsigned char key_buffer[64];
static size_t key_length;
static size_t key_position;

static int next_byte(void) {
    if (key_position == key_length) {
        ssize_t n = read(STDIN_FILENO, key_buffer, sizeof(key_buffer));
        if (n <= 0) {
            return -1;
        }
        key_length = (size_t)n;
        key_position = 0;
    }
    return key_buffer[key_position++];
}

/* One key. Arrow keys arrive as ESC '[' 'A'..'D'. Reading byte by byte
 * (blocking for the next one) handles a sequence split across reads. */
static int next_key(void) {
    int c = next_byte();
    if (c != 0x1b) {
        return c;
    }
    c = next_byte();
    if (c != '[') {
        return c; /* a lone ESC is dropped; the key after it still counts */
    }
    switch (next_byte()) {
    case 'A': return KEY_UP;
    case 'B': return KEY_DOWN;
    case 'C': return KEY_RIGHT;
    case 'D': return KEY_LEFT;
    case -1: return KEY_EOF;
    default: return KEY_NONE;
    }
}

/* ======================================================================
 * History: a ring of the last HISTORY_SIZE lines
 * ====================================================================== */

static char history[HISTORY_SIZE][LINE_CAP];
static int history_total; /* lines ever added */

static int history_count(void) {
    return history_total < HISTORY_SIZE ? history_total : HISTORY_SIZE;
}

/* back = 1 is the most recent line, history_count() the oldest kept. */
static const char *history_entry(int back) {
    return history[(history_total - back) % HISTORY_SIZE];
}

static void history_add(const char *line) {
    if (line[0] == '\0') {
        return;
    }
    if (history_total > 0 && strcmp(history_entry(1), line) == 0) {
        return; /* like bash's ignoredups */
    }
    strcpy(history[history_total % HISTORY_SIZE], line);
    history_total++;
}

/* ======================================================================
 * The line editor
 * ====================================================================== */

struct editor {
    const char *prompt;   /* may contain color codes */
    size_t prompt_length; /* bytes */
    size_t prompt_width;  /* columns it takes on screen */
    char *buffer;
    size_t length; /* characters in buffer */
    size_t cursor; /* 0..length */
    size_t max;    /* longest line that fits on the screen row */
};

#define ERASE_TO_END "\x1b[K"

/* Redraw the whole row in one write:
 *   "\r" prompt line, erase the rest of the row (leftovers of a longer
 *   line), then "\b" back to the cursor.
 * \r and \b only move within the row, which is why the line has to stay on
 * one row (see `max`). */
static void redraw(struct editor *e) {
    char out[5 * SCREEN_WIDTH];
    size_t n = 0;

    out[n++] = '\r';
    memcpy(out + n, e->prompt, e->prompt_length);
    n += e->prompt_length;
    memcpy(out + n, e->buffer, e->length);
    n += e->length;
    memcpy(out + n, ERASE_TO_END, sizeof(ERASE_TO_END) - 1);
    n += sizeof(ERASE_TO_END) - 1;
    for (size_t i = 0; i < e->length - e->cursor; i++) {
        out[n++] = '\b';
    }
    write(STDOUT_FILENO, out, n);
}

static void load_line(struct editor *e, const char *text) {
    size_t n = strlen(text);
    if (n > e->max) {
        n = e->max; /* the prompt got longer since (cd); keep what fits */
    }
    memcpy(e->buffer, text, n);
    e->length = n;
    e->cursor = n;
}

/* Read one line from the console in raw mode. `prompt_width` is how many
 * columns the prompt takes (its color codes take none). Returns the line's
 * length (it is NUL-terminated in `buffer`), or -1 if input ended. */
static int edit_line(const char *prompt, size_t prompt_width, char *buffer, size_t capacity) {
    struct editor e;
    e.prompt = prompt;
    e.prompt_length = strlen(prompt);
    e.prompt_width = prompt_width;
    e.buffer = buffer;
    e.length = 0;
    e.cursor = 0;
    e.max = prompt_width + 1 < SCREEN_WIDTH ? SCREEN_WIDTH - 1 - prompt_width : 0;
    if (e.max > capacity - 1) {
        e.max = capacity - 1;
    }

    char draft[LINE_CAP]; /* the new line, kept while browsing history */
    int back = 0;         /* 0: editing the new line; n: history_entry(n) */

    write(STDOUT_FILENO, prompt, e.prompt_length);
    for (;;) {
        int key = next_key();
        if (key == KEY_EOF) {
            return -1;
        }

        if (key == '\n' || key == '\r') {
            buffer[e.length] = '\0';
            write(STDOUT_FILENO, "\n", 1);
            history_add(buffer);
            return (int)e.length;
        } else if (key == 0x03) { /* Ctrl-C: drop the line */
            write(STDOUT_FILENO, "^C\n", 3);
            buffer[0] = '\0';
            return 0;
        } else if (key == 0x0c) { /* Ctrl-L: clear the screen */
            write(STDOUT_FILENO, CLEAR_SCREEN, sizeof(CLEAR_SCREEN) - 1);
        } else if (key == 0x08 || key == 0x7f) { /* Backspace */
            if (e.cursor == 0) {
                continue;
            }
            memmove(buffer + e.cursor - 1, buffer + e.cursor, e.length - e.cursor);
            e.cursor--;
            e.length--;
        } else if (key >= 0x20 && key <= 0x7e) {
            if (e.length >= e.max) {
                continue;
            }
            memmove(buffer + e.cursor + 1, buffer + e.cursor, e.length - e.cursor);
            buffer[e.cursor++] = (char)key;
            e.length++;
        } else if (key == KEY_LEFT) {
            if (e.cursor == 0) {
                continue;
            }
            e.cursor--;
        } else if (key == KEY_RIGHT) {
            if (e.cursor == e.length) {
                continue;
            }
            e.cursor++;
        } else if (key == KEY_UP) {
            if (back == history_count()) {
                continue;
            }
            if (back == 0) {
                memcpy(draft, buffer, e.length);
                draft[e.length] = '\0';
            }
            back++;
            load_line(&e, history_entry(back));
        } else if (key == KEY_DOWN) {
            if (back == 0) {
                continue;
            }
            back--;
            load_line(&e, back == 0 ? draft : history_entry(back));
        } else {
            continue; /* Tab, Ctrl-D, unknown sequences, ... */
        }
        redraw(&e);
    }
}

/* "cwd ► " (cwd in teal, the arrow green, or red after a failed command),
 * with a long cwd shortened to "...<end of it>". Returns the prompt's width
 * in columns. */
static size_t make_prompt(char *prompt, size_t capacity) {
    char cwd[PATH_CAP];
    if (getcwd(cwd, sizeof(cwd)) < 0) {
        strcpy(cwd, "?");
    }
    size_t length = strlen(cwd);
    const char *shown = cwd;
    const char *ellipsis = "";
    if (length > PROMPT_PATH_MAX) {
        shown = cwd + length - (PROMPT_PATH_MAX - 3);
        ellipsis = "...";
    }
    snprintf(prompt, capacity, "%s%s%s%s %s" ARROW "%s ", paint(TEAL), ellipsis, shown,
             paint(RESET), paint(last_status == 0 ? GREEN : RED), paint(RESET));
    return strlen(ellipsis) + strlen(shown) + 3;
}

/* ======================================================================
 * Script mode: lines from a non-terminal stdin
 * ====================================================================== */

static char script_buffer[512];
static size_t script_length;
static size_t script_position;
static int script_eof;

/* Read the next line (without its '\n'). Returns its length, or -1 at the
 * end of input. The last line may lack a '\n'. A line that doesn't fit is
 * skipped with an error (running a cut-off command could do damage). */
static int read_line(char *line, size_t capacity) {
    size_t n = 0;
    int got_any = 0;
    int too_long = 0;
    for (;;) {
        if (script_position == script_length) {
            if (script_eof) {
                break;
            }
            ssize_t r = read(STDIN_FILENO, script_buffer, sizeof(script_buffer));
            if (r <= 0) {
                script_eof = 1;
                continue;
            }
            script_length = (size_t)r;
            script_position = 0;
        }
        char c = script_buffer[script_position++];
        got_any = 1;
        if (c == '\n') {
            break;
        }
        if (n + 1 < capacity) {
            line[n++] = c;
        } else {
            too_long = 1;
        }
    }
    if (!got_any) {
        return -1;
    }
    if (too_long) {
        shell_error("sh: line too long (at most %d characters)", (int)capacity - 1);
        n = 0;
    }
    line[n] = '\0';
    return (int)n;
}

/* ======================================================================
 * Tokenizer
 * ====================================================================== */

/* Split `line` in place into argv[0..n), argv[n] = NULL, and return n.
 *
 * Tokens are separated by spaces/tabs. "..." groups text, spaces included,
 * and may sit inside a token: a"b c"d is `ab cd`, and "" is an empty
 * argument. An unquoted '#' at the start of a token starts a comment.
 *
 * Removing quotes only shortens text, so the result is written over the
 * input: `w` (write) never passes `r` (read). Returns -1 (after printing
 * why) for an unterminated quote or too many tokens. */
static int tokenize(char *line, char **argv, int max) {
    char *r = line;
    char *w = line;
    int argc = 0;
    for (;;) {
        while (*r == ' ' || *r == '\t') {
            r++;
        }
        if (*r == '\0' || *r == '#') {
            break;
        }
        if (argc == max) {
            shell_error("sh: too many arguments (at most %d)", max);
            return -1;
        }
        argv[argc++] = w;
        while (*r != '\0' && *r != ' ' && *r != '\t') {
            if (*r == '"') {
                r++;
                while (*r != '"') {
                    if (*r == '\0') {
                        shell_error("sh: unterminated quote");
                        return -1;
                    }
                    *w++ = *r++;
                }
                r++; /* the closing quote */
            } else {
                *w++ = *r++;
            }
        }
        /* Step over the separator *before* writing the NUL: w may be
         * right at it. */
        if (*r != '\0') {
            r++;
        }
        *w++ = '\0';
    }
    argv[argc] = NULL;
    return argc;
}

/* ======================================================================
 * Builtins
 * ====================================================================== */

static int usage(const char *text) {
    shell_error("usage: %s", text);
    return 1;
}

static int cmd_cd(int argc, char **argv) {
    if (argc > 2) {
        return usage("cd [dir]");
    }
    const char *dir = argc == 2 ? argv[1] : "/";
    int r = chdir(dir);
    if (r < 0) {
        report("cd", dir, r);
        return 1;
    }
    return 0;
}

static int cmd_pwd(int argc, char **argv) {
    (void)argv;
    if (argc > 1) {
        return usage("pwd");
    }
    char cwd[PATH_CAP];
    long r = getcwd(cwd, sizeof(cwd));
    if (r < 0) {
        report("pwd", ".", (int)r);
        return 1;
    }
    puts(cwd);
    return 0;
}

/* One line on stdout: the entries of `path`, two spaces apart, with '/'
 * after directories. A file path prints just its name, like ls. */
static int cmd_ls(int argc, char **argv) {
    if (argc > 2) {
        return usage("ls [path]");
    }
    const char *path = argc == 2 ? argv[1] : ".";
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        report("ls", path, fd);
        return 1;
    }

    char entries[1024];
    int printed = 0;
    int status = 0;
    for (;;) {
        long n = getdents64(fd, entries, sizeof(entries));
        if (n == -ENOTDIR) {
            puts(path);
            break;
        }
        if (n < 0) {
            report("ls", path, (int)n);
            status = 1;
            break;
        }
        if (n == 0) {
            break;
        }
        for (long offset = 0; offset < n;) {
            struct linux_dirent64 *entry = (struct linux_dirent64 *)(entries + offset);
            offset += entry->d_reclen;
            if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0) {
                continue;
            }
            const char *color = entry->d_type == DT_DIR ? TEAL : entry->d_type == DT_CHR ? AMBER : "";
            printf("%s%s%s%s%s", printed ? "  " : "", paint(color), entry->d_name,
                   entry->d_type == DT_DIR ? "/" : "", paint(color[0] ? RESET : ""));
            printed = 1;
        }
    }
    if (printed) {
        putchar('\n');
    }
    close(fd);
    return status;
}

static int cmd_mkdir(int argc, char **argv) {
    if (argc < 2) {
        return usage("mkdir <dir>...");
    }
    int status = 0;
    for (int i = 1; i < argc; i++) {
        int r = mkdir(argv[i]);
        if (r < 0) {
            report("mkdir", argv[i], r);
            status = 1;
        }
    }
    return status;
}

static int cmd_rmdir(int argc, char **argv) {
    if (argc < 2) {
        return usage("rmdir <dir>...");
    }
    int status = 0;
    for (int i = 1; i < argc; i++) {
        int r = rmdir(argv[i]);
        if (r < 0) {
            report("rmdir", argv[i], r);
            status = 1;
        }
    }
    return status;
}

/* Create the file if it's missing. Unlike `open(O_TRUNC)`, an existing
 * file keeps its contents. */
static int cmd_touch(int argc, char **argv) {
    if (argc < 2) {
        return usage("touch <file>...");
    }
    int status = 0;
    for (int i = 1; i < argc; i++) {
        int fd = open(argv[i], O_WRONLY | O_CREAT);
        if (fd < 0) {
            report("touch", argv[i], fd);
            status = 1;
        } else {
            close(fd);
        }
    }
    return status;
}

static int cmd_cat(int argc, char **argv) {
    if (argc < 2) {
        return usage("cat <file>...");
    }
    int status = 0;
    char buffer[512];
    for (int i = 1; i < argc; i++) {
        int fd = open(argv[i], O_RDONLY);
        if (fd < 0) {
            report("cat", argv[i], fd);
            status = 1;
            continue;
        }
        for (;;) {
            ssize_t n = read(fd, buffer, sizeof(buffer));
            if (n < 0) {
                report("cat", argv[i], (int)n);
                status = 1;
                break;
            }
            if (n == 0) {
                break;
            }
            write(STDOUT_FILENO, buffer, (size_t)n);
        }
        close(fd);
    }
    return status;
}

/* The arguments joined by single spaces. Built into one buffer, so it's a
 * single write. The arguments came from one command line, so they fit. */
static int cmd_echo(int argc, char **argv) {
    char out[LINE_CAP + 1];
    size_t n = 0;
    for (int i = 1; i < argc; i++) {
        if (i > 1) {
            out[n++] = ' ';
        }
        size_t length = strlen(argv[i]);
        memcpy(out + n, argv[i], length);
        n += length;
    }
    out[n++] = '\n';
    write(STDOUT_FILENO, out, n);
    return 0;
}

static int cmd_clear(int argc, char **argv) {
    (void)argc;
    (void)argv;
    write(STDOUT_FILENO, CLEAR_SCREEN, sizeof(CLEAR_SCREEN) - 1);
    return 0;
}

static int cmd_exit(int argc, char **argv) {
    if (argc > 2) {
        return usage("exit [status]");
    }
    exit_requested = 1;
    exit_status = argc == 2 ? atoi(argv[1]) : 0;
    return 0;
}

/* --- rm ---------------------------------------------------------------- */

/* "/", ".", "..", "x/." and the like. `rm -r` on these would delete the
 * tree the shell is standing in (or everything). */
static int is_protected_path(const char *path) {
    size_t end = strlen(path);
    while (end > 0 && path[end - 1] == '/') {
        end--;
    }
    if (end == 0) {
        return 1; /* only slashes: the root */
    }
    size_t start = end;
    while (start > 0 && path[start - 1] != '/') {
        start--;
    }
    size_t length = end - start;
    return (length == 1 && path[start] == '.')
        || (length == 2 && path[start] == '.' && path[start + 1] == '.');
}

/* remove_tree works on one shared path buffer (append "/name", recurse,
 * cut it off again), so each level of recursion costs only a few bytes of
 * the 64 KiB user stack. */
static char tree_path[PATH_CAP];
static char tree_entries[1024];

/* Remove tree_path[0..length) and everything below it. Prints the first
 * error (with the path that failed) and returns it.
 *
 * Our getdents64 continues by entry *index*. Deleting entries while
 * reading would shift the later ones down and skip some. So each round
 * reopens the directory, takes the first real entry, and removes it,
 * until only "." and ".." are left. */
static int remove_tree(size_t length) {
    int r = unlink(tree_path);
    if (r != -EISDIR) {
        if (r < 0) {
            report("rm", tree_path, r);
        }
        return r;
    }

    for (;;) {
        int fd = open(tree_path, O_RDONLY);
        if (fd < 0) {
            report("rm", tree_path, fd);
            return fd;
        }
        long n = getdents64(fd, tree_entries, sizeof(tree_entries));
        close(fd);
        if (n < 0) {
            report("rm", tree_path, (int)n);
            return (int)n;
        }

        const char *child = NULL;
        for (long offset = 0; offset < n;) {
            struct linux_dirent64 *entry = (struct linux_dirent64 *)(tree_entries + offset);
            offset += entry->d_reclen;
            if (strcmp(entry->d_name, ".") != 0 && strcmp(entry->d_name, "..") != 0) {
                child = entry->d_name;
                break;
            }
        }
        if (child == NULL) {
            break; /* empty */
        }

        size_t child_length = strlen(child);
        if (length + 1 + child_length + 1 > sizeof(tree_path)) {
            report("rm", tree_path, -ENAMETOOLONG);
            return -ENAMETOOLONG;
        }
        /* Copied out before recursing: tree_entries gets reused. */
        tree_path[length] = '/';
        memcpy(tree_path + length + 1, child, child_length + 1);
        r = remove_tree(length + 1 + child_length);
        tree_path[length] = '\0';
        if (r < 0) {
            return r;
        }
    }

    r = rmdir(tree_path);
    if (r < 0) {
        report("rm", tree_path, r);
    }
    return r;
}

static int cmd_rm(int argc, char **argv) {
    int first = 1;
    int recursive = 0;
    if (argc > 1 && (strcmp(argv[1], "-r") == 0 || strcmp(argv[1], "-R") == 0)) {
        recursive = 1;
        first = 2;
    }
    if (first >= argc) {
        return usage("rm [-r] <path>...");
    }

    int status = 0;
    for (int i = first; i < argc; i++) {
        const char *path = argv[i];
        if (is_protected_path(path)) {
            shell_error("rm: %s: refusing to remove", path);
            status = 1;
            continue;
        }
        if (!recursive) {
            int r = unlink(path);
            if (r < 0) {
                report("rm", path, r);
                status = 1;
            }
            continue;
        }
        size_t length = strlen(path);
        while (length > 1 && path[length - 1] == '/') {
            length--; /* "d/" → "d", so child paths don't get "//" */
        }
        if (length + 1 > sizeof(tree_path)) {
            report("rm", path, -ENAMETOOLONG);
            status = 1;
            continue;
        }
        memcpy(tree_path, path, length);
        tree_path[length] = '\0';
        if (remove_tree(length) < 0) {
            status = 1;
        }
    }
    return status;
}

/* --- table --------------------------------------------------------------- */

static int cmd_help(int argc, char **argv);

struct builtin {
    const char *name;
    int (*run)(int argc, char **argv);
    const char *args;
    const char *description;
};

static const struct builtin builtins[] = {
    {"cd", cmd_cd, "[dir]", "change directory (default /)"},
    {"pwd", cmd_pwd, "", "print the working directory"},
    {"ls", cmd_ls, "[path]", "list a directory"},
    {"mkdir", cmd_mkdir, "<dir>...", "create directories"},
    {"rmdir", cmd_rmdir, "<dir>...", "remove empty directories"},
    {"rm", cmd_rm, "[-r] <path>...", "remove files (-r: directories too)"},
    {"touch", cmd_touch, "<file>...", "create empty files"},
    {"cat", cmd_cat, "<file>...", "print files"},
    {"echo", cmd_echo, "[text]...", "print the arguments"},
    {"clear", cmd_clear, "", "clear the screen (also Ctrl-L)"},
    {"help", cmd_help, "", "show this help"},
    {"exit", cmd_exit, "[status]", "leave the shell"},
};

enum { BUILTIN_COUNT = sizeof(builtins) / sizeof(builtins[0]) };

static int cmd_help(int argc, char **argv) {
    (void)argc;
    (void)argv;
    enum { WIDTH = 72 };
    print_section("COMMANDS", WIDTH);
    for (int i = 0; i < BUILTIN_COUNT; i++) {
        printf("  %s%-6s%s %-15s %s%s%s\n", paint(TEAL), builtins[i].name, paint(RESET),
               builtins[i].args, paint(MID), builtins[i].description, paint(RESET));
    }
    print_section("PROGRAMS", WIDTH);
    printf("  %sNAME%s runs /bin/NAME %s" DOT "%s a path with '/' runs that file %s" DOT
           "%s %sls /bin%s lists them\n",
           paint(TEAL), paint(RESET), paint(DIM), paint(RESET), paint(DIM), paint(RESET),
           paint(TEAL), paint(RESET));
    print_section("KEYS", WIDTH);
    printf("  %s\u2190 \u2192%s move   %s\u2191 \u2193%s history   %sCtrl-C%s cancel/stop   "
           "%sCtrl-L%s clear\n",
           paint(TEAL), paint(RESET), paint(TEAL), paint(RESET), paint(TEAL), paint(RESET),
           paint(TEAL), paint(RESET));
    return 0;
}

/* Returns 1 if argv[0] is a builtin (and runs it). */
static int run_builtin(int argc, char **argv) {
    for (int i = 0; i < BUILTIN_COUNT; i++) {
        if (strcmp(argv[0], builtins[i].name) == 0) {
            last_status = builtins[i].run(argc, argv);
            return 1;
        }
    }
    return 0;
}

/* ======================================================================
 * Running programs
 * ====================================================================== */

/* Run argv[0] as a program and wait for it.
 *
 * Interactive mode hands the terminal over for the duration:
 *  - cooked mode, so the child gets normal line input (with echo), and a
 *    ^C becomes a kill. In raw mode ^C is just byte 3 for the shell;
 *  - tty_foreground(child), so that ^C kills the child. wait4 clears it
 *    again once it has collected the child.
 * Then back to raw mode for the editor. A ^C typed between spawn and
 * tty_foreground kills nobody, which is harmless. */
static void run_program(char **argv, int interactive) {
    const char *name = argv[0];
    char path[PATH_CAP];
    int has_slash = strchr(name, '/') != NULL;
    int needed = has_slash ? snprintf(path, sizeof(path), "%s", name)
                           : snprintf(path, sizeof(path), "/bin/%s", name);
    if (needed >= (int)sizeof(path)) {
        report("sh", name, -ENAMETOOLONG);
        last_status = 1;
        return;
    }

    if (interactive) {
        tty_mode(STDIN_FILENO, TTY_COOKED);
    }
    int child = spawn(path, argv);
    int status = 0;
    int waited = 0;
    if (child > 0) {
        if (interactive) {
            tty_foreground(STDIN_FILENO, child);
        }
        waited = wait4(child, &status, 0, NULL);
    }
    if (interactive) {
        tty_mode(STDIN_FILENO, TTY_RAW);
    }

    if (child == -ENOENT && !has_slash) {
        shell_error("sh: %s: command not found", name);
        status = 127; /* like bash */
    } else if (child < 0) {
        report("sh", name, child);
        status = 126;
    } else if (waited < 0) {
        report("sh", "wait4", waited);
        status = 1;
    } else if (status != 0) {
        const char *meaning = status == EXIT_INTERRUPTED ? ": interrupted"
                            : status == EXIT_FAULT       ? ": crashed (CPU exception)"
                                                         : "";
        printf("%s[exit %d%s]%s\n", paint(AMBER), status, meaning, paint(RESET));
    }
    last_status = status;
}

/* ======================================================================
 * main
 * ====================================================================== */

/* One row of the banner box: "│ " text, padded to `inner` columns, " │". */
static void banner_row(const char *text, size_t inner) {
    size_t width = visible_width(text);
    printf("%s\u2502%s %s%*s %s\u2502%s\n", paint(DIM), paint(RESET), text,
           (int)(width < inner ? inner - width : 0), "", paint(DIM), paint(RESET));
}

/* The welcome box, in the style of the boot screen:
 *
 *   ┌─[ RUST-OS ]──────────────────────────────────────────────────┐
 *   │ SHELL    /BIN/SH · TTY0        TYPE 'help' FOR THE COMMANDS  │
 *   │ SESSION  INTERACTIVE           CTRL-C STOPS · CTRL-L CLEARS  │
 *   └──────────────────────────────────────────────────────────────┘
 */
static void print_banner(void) {
    enum { INNER = 64 };
    char line[3 * (INNER + 2) + 64];
    char row[LINE_CAP];
    char title[128];

    gradient_title(title, sizeof(title));
    fading_rule(line, INNER + 2 - 1 - 11, FADE_TO_FRAME, 3);
    printf("%s\u250C" RULE "[ %s%s ]%s%s\u2510%s\n", paint(DIM), title, paint(DIM), line,
           paint(DIM), paint(RESET));
    snprintf(row, sizeof(row), "%sSHELL%s    /BIN/SH %s" DOT "%s TTY0        %sTYPE %s'help'%s FOR THE COMMANDS%s",
             paint(DIM), paint(RESET), paint(DIM), paint(RESET), paint(MID), paint(TEAL),
             paint(MID), paint(RESET));
    banner_row(row, INNER);
    snprintf(row, sizeof(row), "%sSESSION%s  INTERACTIVE           %sCTRL-C STOPS " DOT " CTRL-L CLEARS%s",
             paint(DIM), paint(RESET), paint(MID), paint(RESET));
    banner_row(row, INNER);
    rule(line, INNER + 2);
    printf("%s\u2514%s\u2518%s\n\n", paint(DIM), line, paint(RESET));
}

int main(int argc, char **argv) {
    (void)argc;
    (void)argv;
    int interactive = tty_mode(STDIN_FILENO, TTY_RAW) >= 0;
    use_color = interactive;
    if (interactive) {
        print_banner();
    }

    char line[LINE_CAP];
    char *args[MAX_TOKENS + 1];
    char prompt[PROMPT_PATH_MAX + 64]; /* the path plus color codes */
    while (!exit_requested) {
        int n;
        if (interactive) {
            size_t width = make_prompt(prompt, sizeof(prompt));
            n = edit_line(prompt, width, line, sizeof(line));
        } else {
            n = read_line(line, sizeof(line));
        }
        if (n < 0) {
            break; /* end of input */
        }
        int count = tokenize(line, args, MAX_TOKENS);
        if (count <= 0) {
            continue;
        }
        if (!run_builtin(count, args)) {
            run_program(args, interactive);
        }
    }

    if (interactive) {
        tty_mode(STDIN_FILENO, TTY_COOKED);
    }
    return exit_status;
}
