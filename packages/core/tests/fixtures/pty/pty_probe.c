// pty_probe.c — argv-dispatched PTY/line-discipline probe for the agent-os
// terminal test matrix (packages/core/tests/pty-line-discipline.nightly.test.ts).
//
// Unlike agentos's single-sequence pty_probe.c, this probe runs exactly ONE
// case, selected by argv[1] (the caseId). The host harness drives the SAME case
// set through this WASM probe and a guest-Node twin (pty_probe.mjs) and asserts
// the SAME observable marker protocol against both runtimes.
//
// Build (vanilla wasi-sysroot — NO termios.h / sys/ioctl.h on the wasm path):
//   <wasi-sdk>/bin/clang --target=wasm32-wasip1 \
//     --sysroot=<wasi-sdk>/share/wasi-sysroot -O2 -o <dir>/pty_probe pty_probe.c
//
// The `#else` real-termios branch keeps the file natively debuggable:
//   clang -o /tmp/pty_probe pty_probe.c
//
// Marker protocol (every key=val lowercase; every line terminated with a LITERAL
// "\r\n" so @xterm/headless returns the cursor to column 0 even on the raw echo
// path that may not inject CR): see the test header for the full vocabulary.

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#if defined(__wasm__)
__attribute__((import_module("host_tty"), import_name("isatty"))) extern unsigned int
host_tty_isatty(unsigned int fd);
__attribute__((import_module("host_tty"), import_name("get_size"))) extern unsigned int
host_tty_get_size(unsigned int fd, unsigned short *cols, unsigned short *rows);
__attribute__((import_module("host_tty"), import_name("set_raw_mode"))) extern unsigned int
host_tty_set_raw_mode(unsigned int enabled);
#else
#include <sys/ioctl.h>
#include <termios.h>
static struct termios saved_termios;
static int saved_termios_valid = 0;

static unsigned int host_tty_isatty(unsigned int fd) {
    return isatty((int)fd) ? 1u : 0u;
}

static unsigned int host_tty_get_size(unsigned int fd, unsigned short *cols, unsigned short *rows) {
    struct winsize ws;
    memset(&ws, 0, sizeof(ws));
    if (ioctl((int)fd, TIOCGWINSZ, &ws) != 0) {
        return (unsigned int)errno;
    }
    *cols = ws.ws_col;
    *rows = ws.ws_row;
    return 0;
}

static unsigned int host_tty_set_raw_mode(unsigned int enabled) {
    if (enabled) {
        struct termios raw;
        if (tcgetattr(STDIN_FILENO, &saved_termios) != 0) {
            return (unsigned int)errno;
        }
        saved_termios_valid = 1;
        raw = saved_termios;
        cfmakeraw(&raw);
        if (tcsetattr(STDIN_FILENO, TCSANOW, &raw) != 0) {
            return (unsigned int)errno;
        }
        return 0;
    }
    if (saved_termios_valid && tcsetattr(STDIN_FILENO, TCSANOW, &saved_termios) != 0) {
        return (unsigned int)errno;
    }
    return 0;
}
#endif

// ---- shared encoders (byte-identical to agentos's pty_probe.c) ----------

static void print_hex(const unsigned char *bytes, int len) {
    for (int i = 0; i < len; i++) {
        if (i > 0) {
            fputc(' ', stdout);
        }
        printf("%02X", bytes[i]);
    }
}

static void print_text(const unsigned char *bytes, int len) {
    for (int i = 0; i < len; i++) {
        unsigned char c = bytes[i];
        if (c == '\r') {
            fputs("\\r", stdout);
        } else if (c == '\n') {
            fputs("\\n", stdout);
        } else if (c == '\t') {
            fputs("\\t", stdout);
        } else if (c == 0x1b) {
            fputs("\\e", stdout);
        } else if (c < 0x20 || c == 0x7f) {
            printf("\\x%02X", c);
        } else {
            fputc((int)c, stdout);
        }
    }
}

// Reads stdin one byte at a time, retrying on EINTR, until the terminator byte
// is seen / the buffer fills / EOF. Keeps the probe BLOCKED IN READ so the host
// can prove kernel echo while the program has not yet read the typed bytes.
static int read_until(unsigned char *buf, int cap, unsigned char terminator) {
    int len = 0;
    while (len < cap) {
        unsigned char c = 0;
        ssize_t n = read(STDIN_FILENO, &c, 1);
        if (n == 0) {
            return len; // EOF
        }
        if (n < 0) {
            if (errno == EINTR) {
                continue;
            }
            printf("READ_ERROR errno=%d\r\n", errno);
            return -1;
        }
        buf[len++] = c;
        if (c == terminator) {
            return len;
        }
    }
    return len;
}

static void emit_bytes(const char *tag, const unsigned char *buf, int n) {
    printf("#BYTES tag=%s n=%d hex=", tag, n);
    if (n > 0) {
        print_hex(buf, n);
    }
    printf(" text=");
    if (n > 0) {
        print_text(buf, n);
    }
    printf("\r\n");
}

// ---- cases ------------------------------------------------------------------

// cooked-echo: cooked-mode (ICANON+ECHO+ISIG) terminal echo. Block in read right
// after #READY so the host proves kernel echo independent of any readback.
static void case_cooked_echo(void) {
    unsigned char buf[128];
    unsigned int rc = host_tty_set_raw_mode(0);
    printf("#MODE want=cooked rc=%u\r\n", rc);
    printf("#READY tag=echo\r\n");
    int n = read_until(buf, (int)sizeof(buf), '\n');
    if (n < 0) {
        return;
    }
    if (n == 0) {
        printf("#EOF tag=echo n=0\r\n");
        return;
    }
    emit_bytes("echo", buf, n);
}

// control-char-echo: ECHOCTL — a non-signal control byte (^A 0x01) should echo
// as caret notation "^A"; the kernel echoes the raw byte instead (known broken).
static void case_control_char_echo(void) {
    unsigned char buf[128];
    unsigned int rc = host_tty_set_raw_mode(0);
    printf("#MODE want=cooked rc=%u\r\n", rc);
    printf("#READY tag=ctl\r\n");
    int n = read_until(buf, (int)sizeof(buf), '\n');
    if (n < 0) {
        return;
    }
    if (n == 0) {
        printf("#EOF tag=ctl n=0\r\n");
        return;
    }
    emit_bytes("ctl", buf, n);
}

// raw-no-echo: RAW mode (ICANON+ECHO+ISIG off) must NOT echo input.
static void case_raw_no_echo(void) {
    unsigned char buf[128];
    unsigned int rc = host_tty_set_raw_mode(1);
    printf("#MODE want=raw rc=%u\r\n", rc);
    printf("#READY tag=raw\r\n");
    int n = read_until(buf, (int)sizeof(buf), '!');
    if (n < 0) {
        return;
    }
    emit_bytes("raw", buf, n);
}

// backspace: cooked-mode VERASE (DEL 0x7f) erases one char via 08 20 08 echo.
static void case_backspace(void) {
    unsigned char buf[256];
    unsigned int rc = host_tty_set_raw_mode(0);
    printf("#MODE want=cooked rc=%u\r\n", rc);
    printf("#READY tag=erase\r\n");
    int n = read_until(buf, (int)sizeof(buf), '\n');
    if (n < 0) {
        return;
    }
    emit_bytes("erase", buf, n);
}

// kill-line: cooked-mode VKILL (^U 0x15) should discard the whole line; the
// kernel does neither (unimplemented) — captured honestly as broken.
static void case_kill_line(void) {
    unsigned char buf[256];
    unsigned int rc = host_tty_set_raw_mode(0);
    printf("#MODE want=cooked rc=%u\r\n", rc);
    printf("#READY tag=kill\r\n");
    int n = read_until(buf, (int)sizeof(buf), '\n');
    if (n < 0) {
        return;
    }
    emit_bytes("kill", buf, n);
}

// word-erase: cooked-mode VWERASE (^W 0x17) should erase the last word; the
// kernel echoes it raw and keeps it in the buffer (unimplemented — broken).
static void case_word_erase(void) {
    unsigned char buf[256];
    unsigned int rc = host_tty_set_raw_mode(0);
    printf("#MODE want=cooked rc=%u\r\n", rc);
    printf("#READY tag=werase\r\n");
    int n = read_until(buf, (int)sizeof(buf), '\n');
    if (n < 0) {
        return;
    }
    emit_bytes("werase", buf, n);
}

// line-buffering: ICANON holds the line until '\n', then delivers it whole.
static void case_line_buffering(void) {
    unsigned char buf[256];
    unsigned int rc = host_tty_set_raw_mode(0);
    printf("#MODE want=cooked rc=%u\r\n", rc);
    printf("#READY tag=canon\r\n");
    int n = read_until(buf, (int)sizeof(buf), '\n');
    if (n < 0) {
        return;
    }
    if (n == 0) {
        printf("#EOF tag=canon n=0\r\n");
        return;
    }
    emit_bytes("canon", buf, n);
}

// sigint: cooked ISIG — VINTR (^C 0x03) raises SIGINT to the foreground pgid.
// WASM cannot catch POSIX signals, so a correct kernel kills this process while
// it is blocked here (host observes via waitShell). The body runs only if the
// byte (wrongly) leaked through.
static void case_sigint(void) {
    unsigned char buf[128];
    unsigned int rc = host_tty_set_raw_mode(0);
    printf("#MODE want=cooked rc=%u\r\n", rc);
    printf("#READY tag=sigint\r\n");
    int n = read_until(buf, (int)sizeof(buf), '!');
    if (n <= 0) {
        printf("#EOF tag=sigint n=0\r\n");
        return;
    }
    emit_bytes("sigint", buf, n);
}

// sigquit: cooked ISIG — VQUIT (^\ 0x1C) raises SIGQUIT; correct behavior is
// process death observed by the host. cap=1 so a leaked byte resolves at once.
static void case_sigquit(void) {
    unsigned char buf[8];
    unsigned int rc = host_tty_set_raw_mode(0);
    printf("#MODE want=cooked rc=%u\r\n", rc);
    printf("#READY tag=sigquit\r\n");
    int n = read_until(buf, 1, '!');
    if (n <= 0) {
        printf("#EOF tag=sigquit n=0\r\n");
        return;
    }
    emit_bytes("sigquit", buf, n);
}

// vsusp: cooked ISIG — VSUSP (^Z 0x1a) raises SIGTSTP. The byte is consumed as a
// signal: neither delivered (no #BYTES) nor echoed. The foreground process is
// suspended (a STOP, not a kill), so a correct kernel never lets it reach #DONE.
static void case_vsusp(void) {
    unsigned char buf[8];
    unsigned int rc = host_tty_set_raw_mode(0);
    printf("#MODE want=cooked rc=%u\r\n", rc);
    printf("#READY tag=susp\r\n");
    int n = read_until(buf, 1, '!');
    if (n <= 0) {
        printf("#EOF tag=susp n=0\r\n");
        return;
    }
    emit_bytes("susp", buf, n);
}

// erase-ctrl-h: cooked-mode VERASE alias ^H (0x08) erases one char exactly like
// DEL (0x7f); the kernel maps both bytes to the erase op. Type "ab" + ^H + '\n'
// -> the delivered line drops the last char -> "a\n".
static void case_erase_ctrl_h(void) {
    unsigned char buf[256];
    unsigned int rc = host_tty_set_raw_mode(0);
    printf("#MODE want=cooked rc=%u\r\n", rc);
    printf("#READY tag=eraseh\r\n");
    int n = read_until(buf, (int)sizeof(buf), '\n');
    if (n < 0) {
        return;
    }
    emit_bytes("eraseh", buf, n);
}

// vintr-buffer: VINTR (^C) mid-line BOTH flushes the canonical input buffer AND
// raises SIGINT. A runtime that survives the signal proves the FLUSH: after
// "abc" + ^C + "de\n" the delivered line is "de\n" (the buffered "abc" was
// discarded), NOT "abcde\n". WASM cannot catch SIGINT, so this process is killed
// mid-read and only ever proves the kill; the buffer flush is proven by the
// surviving twin (guest-node).
static void case_vintr_buffer(void) {
    unsigned char buf[256];
    unsigned int rc = host_tty_set_raw_mode(0);
    printf("#MODE want=cooked rc=%u\r\n", rc);
    printf("#READY tag=vintrbuf\r\n");
    int n = read_until(buf, (int)sizeof(buf), '\n');
    if (n < 0) {
        return;
    }
    if (n == 0) {
        printf("#EOF tag=vintrbuf n=0\r\n");
        return;
    }
    emit_bytes("vintrbuf", buf, n);
}

// raw-ctrlc-byte: RAW mode (ISIG off) — VINTR (0x03) is ordinary input, not a
// signal. Read exactly one byte and report it; reaching #DONE proves no kill.
static void case_raw_ctrlc_byte(void) {
    unsigned int rc = host_tty_set_raw_mode(1);
    printf("#MODE want=raw rc=%u\r\n", rc);
    printf("#READY tag=rawc\r\n");

    unsigned char buf[1];
    int len = 0;
    while (len < 1) {
        unsigned char c = 0;
        ssize_t n = read(STDIN_FILENO, &c, 1);
        if (n == 0) {
            printf("#EOF tag=rawc n=0\r\n");
            return;
        }
        if (n < 0) {
            if (errno == EINTR) {
                continue;
            }
            printf("#ERR read errno=%d\r\n", errno);
            return;
        }
        buf[len++] = c;
    }
    emit_bytes("rawc", buf, len);
}

// onlcr: OPOST+ONLCR output expansion. Raw-write `a\nb`; the master must read
// back 0x61 0x0D 0x0A 0x62 (CR injected before the lone LF). No input consumed.
static void case_onlcr(void) {
    write(STDOUT_FILENO, "a\nb", 3);
}

// icrnl: cooked-mode ICRNL maps a typed CR (0x0D) to NL (0x0A), which both
// terminates the canonical line and is the byte delivered to the program.
static void case_icrnl(void) {
    unsigned char buf[128];
    unsigned int rc = host_tty_set_raw_mode(0);
    printf("#MODE want=cooked rc=%u\r\n", rc);
    printf("#READY tag=icrnl\r\n");
    int n = read_until(buf, (int)sizeof(buf), '\n');
    if (n < 0) {
        return;
    }
    emit_bytes("icrnl", buf, n);
}

// eof: cooked-mode VEOF (^D 0x04) on an empty line makes read() return 0 (EOF)
// and is NOT echoed.
static void case_eof(void) {
    unsigned int rc = host_tty_set_raw_mode(0);
    printf("#MODE want=cooked rc=%u\r\n", rc);
    printf("#READY tag=eof\r\n");

    unsigned char byte = 0;
    ssize_t n;
    do {
        n = read(STDIN_FILENO, &byte, 1);
    } while (n < 0 && errno == EINTR);

    if (n <= 0) {
        printf("#EOF tag=eof n=0\r\n");
    } else {
        emit_bytes("eof", &byte, 1);
    }
}

// resize-sigwinch: report size, block in a cooked read, then re-query the LIVE
// size after the host resizes + unblocks. WASM cannot catch SIGWINCH, so the
// new size from host_tty_get_size is the proof.
static void case_resize_sigwinch(void) {
    unsigned short cols0 = 0, rows0 = 0;
    unsigned int rc0 = host_tty_get_size(0, &cols0, &rows0);
    printf("#SIZE tag=before rc=%u cols=%u rows=%u\r\n",
           rc0, (unsigned int)cols0, (unsigned int)rows0);

    printf("#READY tag=resize\r\n");
    unsigned char buf[16];
    read_until(buf, (int)sizeof(buf), '!');

    unsigned short cols1 = 0, rows1 = 0;
    unsigned int rc1 = host_tty_get_size(0, &cols1, &rows1);
    printf("#SIZE tag=after rc=%u cols=%u rows=%u\r\n",
           rc1, (unsigned int)cols1, (unsigned int)rows1);
}

// cpr: DSR 6 -> CPR. Raw mode so the reply arrives verbatim; write ESC[6n and
// read_until 'R'. @xterm answers (the host wires term.onData -> writeShell).
static void case_cpr(void) {
    unsigned char buf[128];
    unsigned int rc = host_tty_set_raw_mode(1);
    printf("#MODE want=raw rc=%u\r\n", rc);
    write(STDOUT_FILENO, "\x1b[6n", 4);
    printf("#CPR sent=1\r\n");
    int n = read_until(buf, (int)sizeof(buf), 'R');
    if (n <= 0) {
        printf("#EOF tag=cpr n=0\r\n");
        return;
    }
    printf("#CPRREPLY n=%d hex=", n);
    print_hex(buf, n);
    printf(" text=");
    print_text(buf, n);
    printf("\r\n");
}

// isatty: report host_tty_isatty for fd 0/1/2.
static void case_isatty(void) {
    printf("#TTY in=%u out=%u err=%u\r\n",
           host_tty_isatty(0),
           host_tty_isatty(1),
           host_tty_isatty(2));
}

// winsize: report the kernel slave window size (host_tty_get_size on fd 0). A
// correct kernel returns the exact openShell({cols,rows}). No input read.
static void case_winsize(void) {
    unsigned short cols = 0, rows = 0;
    unsigned int rc = host_tty_get_size(STDIN_FILENO, &cols, &rows);
    printf("#SIZE tag=open rc=%u cols=%u rows=%u\r\n",
           rc, (unsigned int)cols, (unsigned int)rows);
}

int main(int argc, char **argv) {
    setvbuf(stdout, NULL, _IONBF, 0);
    setvbuf(stderr, NULL, _IONBF, 0);

    const char *id = (argc > 1) ? argv[1] : "";

    if (id[0] == '\0') {
        printf("#ERR unknown-case id=\r\n");
        return 2;
    }

    printf("#START id=%s\r\n", id);

    if (strcmp(id, "cooked-echo") == 0) {
        case_cooked_echo();
    } else if (strcmp(id, "control-char-echo") == 0) {
        case_control_char_echo();
    } else if (strcmp(id, "raw-no-echo") == 0) {
        case_raw_no_echo();
    } else if (strcmp(id, "backspace") == 0) {
        case_backspace();
    } else if (strcmp(id, "kill-line") == 0) {
        case_kill_line();
    } else if (strcmp(id, "word-erase") == 0) {
        case_word_erase();
    } else if (strcmp(id, "line-buffering") == 0) {
        case_line_buffering();
    } else if (strcmp(id, "sigint") == 0) {
        case_sigint();
    } else if (strcmp(id, "sigquit") == 0) {
        case_sigquit();
    } else if (strcmp(id, "vsusp") == 0) {
        case_vsusp();
    } else if (strcmp(id, "erase-ctrl-h") == 0) {
        case_erase_ctrl_h();
    } else if (strcmp(id, "vintr-buffer") == 0) {
        case_vintr_buffer();
    } else if (strcmp(id, "raw-ctrlc-byte") == 0) {
        case_raw_ctrlc_byte();
    } else if (strcmp(id, "onlcr") == 0) {
        case_onlcr();
    } else if (strcmp(id, "icrnl") == 0) {
        case_icrnl();
    } else if (strcmp(id, "eof") == 0) {
        case_eof();
    } else if (strcmp(id, "resize-sigwinch") == 0) {
        case_resize_sigwinch();
    } else if (strcmp(id, "cpr") == 0) {
        case_cpr();
    } else if (strcmp(id, "isatty") == 0) {
        case_isatty();
    } else if (strcmp(id, "winsize") == 0) {
        case_winsize();
    } else {
        printf("#ERR unknown-case id=%s\r\n", id);
        return 2;
    }

    printf("#DONE id=%s\r\n", id);
    return 0;
}
