/*
 * printf and friends.
 *
 * One formatter (`format`) writes into a `struct out`, which either fills a
 * caller's buffer (vsnprintf) or collects bytes in a small stack buffer and
 * writes them to an fd (vdprintf). So a typical printf is a single `write`:
 * faster than one syscall per character, and a line printed by one process
 * isn't split by another process's output in the middle.
 */
#include "ulib.h"

enum { PENDING_SIZE = 256 };

struct out {
    int fd;         /* -1: format into `buffer` instead */
    char *buffer;   /* buffer mode: the destination ... */
    size_t size;    /* ... and its capacity, including the final NUL */
    size_t count;   /* bytes produced so far (also those that didn't fit) */
    int error;      /* fd mode: the first write error (-errno), or 0 */
    size_t used;    /* fd mode: bytes waiting in `pending` */
    char pending[PENDING_SIZE];
};

static void flush(struct out *o) {
    size_t done = 0;
    while (done < o->used && o->error == 0) {
        ssize_t n = write(o->fd, o->pending + done, o->used - done);
        if (n <= 0) {
            o->error = n < 0 ? (int)n : -EIO;
            break;
        }
        done += (size_t)n;
    }
    o->used = 0;
}

static void emit(struct out *o, char c) {
    if (o->fd >= 0) {
        o->pending[o->used++] = c;
        if (o->used == PENDING_SIZE) {
            flush(o);
        }
    } else if (o->count + 1 < o->size) {
        o->buffer[o->count] = c;
    }
    o->count++;
}

static void repeat(struct out *o, char c, int times) {
    for (int i = 0; i < times; i++) {
        emit(o, c);
    }
}

static void emit_text(struct out *o, const char *text, size_t length, int width, int left) {
    int padding = width > (int)length ? width - (int)length : 0;
    if (!left) {
        repeat(o, ' ', padding);
    }
    for (size_t i = 0; i < length; i++) {
        emit(o, text[i]);
    }
    if (left) {
        repeat(o, ' ', padding);
    }
}

/* `magnitude` is the absolute value; `negative` adds the '-'. With zero
 * padding the zeros go between the sign and the digits: "-0042". */
static void emit_number(
    struct out *o,
    unsigned long magnitude,
    int negative,
    unsigned base,
    int upper,
    int width,
    int left,
    int zero
) {
    const char *digit_set = upper ? "0123456789ABCDEF" : "0123456789abcdef";
    char digits[24]; /* 2^64 has 20 decimal digits */
    int n = 0;
    do {
        digits[n++] = digit_set[magnitude % base];
        magnitude /= base;
    } while (magnitude != 0);

    int length = n + (negative ? 1 : 0);
    int padding = width > length ? width - length : 0;
    if (!left && !zero) {
        repeat(o, ' ', padding);
    }
    if (negative) {
        emit(o, '-');
    }
    if (!left && zero) {
        repeat(o, '0', padding);
    }
    while (n > 0) {
        emit(o, digits[--n]);
    }
    if (left) {
        repeat(o, ' ', padding);
    }
}

static void format(struct out *o, const char *f, va_list args) {
    for (; *f != '\0'; f++) {
        if (*f != '%') {
            emit(o, *f);
            continue;
        }
        f++;

        int left = 0;
        int zero = 0;
        for (;; f++) {
            if (*f == '-') {
                left = 1;
            } else if (*f == '0') {
                zero = 1;
            } else {
                break;
            }
        }
        int width = 0;
        while (*f >= '0' && *f <= '9') {
            width = width * 10 + (*f - '0');
            f++;
        }
        int is_long = 0;
        while (*f == 'l' || *f == 'z') {
            is_long = 1;
            f++;
        }

        switch (*f) {
        case 'd':
        case 'i': {
            long value = is_long ? va_arg(args, long) : (long)va_arg(args, int);
            /* 0 - (unsigned)value is right for LONG_MIN too, whose
             * negation doesn't fit in a long. */
            unsigned long magnitude = value < 0 ? 0UL - (unsigned long)value : (unsigned long)value;
            emit_number(o, magnitude, value < 0, 10, 0, width, left, zero);
            break;
        }
        case 'u':
        case 'x':
        case 'X': {
            unsigned long value = is_long ? va_arg(args, unsigned long)
                                          : (unsigned long)va_arg(args, unsigned int);
            emit_number(o, value, 0, *f == 'u' ? 10 : 16, *f == 'X', width, left, zero);
            break;
        }
        case 'p': {
            unsigned long value = (unsigned long)va_arg(args, void *);
            emit(o, '0');
            emit(o, 'x');
            emit_number(o, value, 0, 16, 0, 0, 0, 0);
            break;
        }
        case 'c': {
            char c = (char)va_arg(args, int);
            emit_text(o, &c, 1, width, left);
            break;
        }
        case 's': {
            const char *s = va_arg(args, const char *);
            if (s == NULL) {
                s = "(null)";
            }
            emit_text(o, s, strlen(s), width, left);
            break;
        }
        case '%':
            emit(o, '%');
            break;
        case '\0':
            /* A lone '%' at the very end: print it and stop. */
            emit(o, '%');
            return;
        default:
            /* Unknown conversion: print it as written. */
            emit(o, '%');
            emit(o, *f);
            break;
        }
    }
}

/* Like C's: writes at most size-1 characters plus a NUL (nothing if size is
 * 0), and returns the length the whole output *would* have had. */
int vsnprintf(char *buffer, size_t size, const char *f, va_list args) {
    struct out o;
    o.fd = -1;
    o.buffer = buffer;
    o.size = size;
    o.count = 0;
    o.error = 0;
    o.used = 0;
    format(&o, f, args);
    if (size > 0) {
        buffer[o.count < size ? o.count : size - 1] = '\0';
    }
    return (int)o.count;
}

int snprintf(char *buffer, size_t size, const char *f, ...) {
    va_list args;
    va_start(args, f);
    int n = vsnprintf(buffer, size, f, args);
    va_end(args);
    return n;
}

int vdprintf(int fd, const char *f, va_list args) {
    struct out o;
    o.fd = fd;
    o.buffer = NULL;
    o.size = 0;
    o.count = 0;
    o.error = 0;
    o.used = 0;
    format(&o, f, args);
    flush(&o);
    return o.error != 0 ? o.error : (int)o.count;
}

int dprintf(int fd, const char *f, ...) {
    va_list args;
    va_start(args, f);
    int n = vdprintf(fd, f, args);
    va_end(args);
    return n;
}

int printf(const char *f, ...) {
    va_list args;
    va_start(args, f);
    int n = vdprintf(STDOUT_FILENO, f, args);
    va_end(args);
    return n;
}

int puts(const char *s) {
    return dprintf(STDOUT_FILENO, "%s\n", s);
}

int putchar(int c) {
    char ch = (char)c;
    ssize_t n = write(STDOUT_FILENO, &ch, 1);
    return n < 0 ? (int)n : (unsigned char)ch;
}
