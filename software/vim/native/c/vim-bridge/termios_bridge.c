/* termios_bridge.c — termios for the agentos full-OS libc.
 *
 * The kernel owns the PTY line discipline (canonical/echo/signals) and exposes
 * a raw-mode toggle + window size to guests via the `host_tty` WASM imports.
 * We bridge POSIX termios onto that: tcsetattr maps the requested mode to
 * host_tty.set_raw_mode (raw when ICANON|ECHO are cleared, cooked otherwise),
 * and we keep a shadow `struct termios` so tcgetattr/tcsetattr round-trip the
 * way applications (vim) expect. Window size comes from host_tty.get_size.
 */
#include <termios.h>
#include <sys/ioctl.h>
#include <errno.h>
#include <string.h>
#include <stdarg.h>
#include <unistd.h>

__attribute__((import_module("host_tty"), import_name("isatty"))) extern unsigned int
__host_tty_isatty(unsigned int fd);
__attribute__((import_module("host_tty"), import_name("get_size"))) extern unsigned int
__host_tty_get_size(unsigned int fd, unsigned short *cols, unsigned short *rows);
__attribute__((import_module("host_tty"), import_name("set_raw_mode"))) extern unsigned int
__host_tty_set_raw_mode(unsigned int enabled);

static struct termios g_shadow;
static int g_shadow_valid = 0;

static void cooked_defaults(struct termios *t) {
	memset(t, 0, sizeof(*t));
	t->c_iflag = ICRNL | IXON | IMAXBEL | IUTF8;
	t->c_oflag = OPOST | ONLCR;
	t->c_cflag = CS8 | CREAD | CLOCAL | B38400;
	t->c_lflag = ISIG | ICANON | ECHO | ECHOE | ECHOK | ECHOCTL | ECHOKE | IEXTEN;
	t->c_cc[VINTR] = 003;   /* ^C */
	t->c_cc[VQUIT] = 034;   /* ^\ */
	t->c_cc[VERASE] = 0177; /* DEL */
	t->c_cc[VKILL] = 025;   /* ^U */
	t->c_cc[VEOF] = 004;    /* ^D */
	t->c_cc[VTIME] = 0;
	t->c_cc[VMIN] = 1;
	t->c_cc[VSTART] = 021;  /* ^Q */
	t->c_cc[VSTOP] = 023;   /* ^S */
	t->c_cc[VSUSP] = 032;   /* ^Z */
	t->c_cc[VREPRINT] = 022;/* ^R */
	t->c_cc[VWERASE] = 027; /* ^W */
	t->c_cc[VLNEXT] = 026;  /* ^V */
	t->c_cc[VDISCARD] = 017;/* ^O */
	t->__c_ispeed = B38400;
	t->__c_ospeed = B38400;
}

static void ensure_shadow(void) {
	if (!g_shadow_valid) {
		cooked_defaults(&g_shadow);
		g_shadow_valid = 1;
	}
}

/* isatty: delegate to the kernel's host_tty view. wasi-libc's isatty inspects
 * WASI fdstat (must be CHARACTER_DEVICE with NO seek/tell rights); a PTY fd
 * that carries seek/tell rights is wrongly reported as not-a-tty, so terminal
 * apps (vim) print "Output is not to a terminal" and refuse full-screen mode.
 * host_tty.isatty is the authoritative PTY check the kernel exposes (a strong
 * symbol overriding the weak libc alias). */
static int bridge_isatty(int fd) {
	return __host_tty_isatty((unsigned int)fd) ? 1 : 0;
}

int __isatty(int fd) {
	return bridge_isatty(fd);
}

int isatty(int fd) {
	return bridge_isatty(fd);
}

int tcgetattr(int fd, struct termios *t) {
	(void)fd;
	if (!t) { errno = EFAULT; return -1; }
	if (!__host_tty_isatty((unsigned int)fd)) { errno = ENOTTY; return -1; }
	ensure_shadow();
	*t = g_shadow;
	return 0;
}

int tcsetattr(int fd, int act, const struct termios *t) {
	(void)act;
	if (!t) { errno = EFAULT; return -1; }
	if (!__host_tty_isatty((unsigned int)fd)) { errno = ENOTTY; return -1; }
	g_shadow = *t;
	g_shadow_valid = 1;
	/* Raw when the line editor is disabled (no canonical input, no echo). */
	unsigned int raw = ((t->c_lflag & ICANON) && (t->c_lflag & ECHO)) ? 0u : 1u;
	unsigned int rc = __host_tty_set_raw_mode(raw);
	if (rc != 0) { errno = (int)rc; return -1; }
	return 0;
}

void cfmakeraw(struct termios *t) {
	if (!t) return;
	t->c_iflag &= ~(IGNBRK | BRKINT | PARMRK | ISTRIP | INLCR | IGNCR | ICRNL | IXON);
	t->c_oflag &= ~OPOST;
	t->c_lflag &= ~(ECHO | ECHONL | ICANON | ISIG | IEXTEN);
	t->c_cflag &= ~(CSIZE | PARENB);
	t->c_cflag |= CS8;
	t->c_cc[VMIN] = 1;
	t->c_cc[VTIME] = 0;
}

int tcflush(int fd, int q) { (void)fd; (void)q; return 0; }
int tcdrain(int fd) { (void)fd; return 0; }
int tcsendbreak(int fd, int d) { (void)fd; (void)d; return 0; }
int tcflow(int fd, int a) { (void)fd; (void)a; return 0; }

speed_t cfgetospeed(const struct termios *t) { return t ? t->__c_ospeed : 0; }
speed_t cfgetispeed(const struct termios *t) { return t ? t->__c_ispeed : 0; }
int cfsetospeed(struct termios *t, speed_t s) { if (t) t->__c_ospeed = s; return 0; }
int cfsetispeed(struct termios *t, speed_t s) { if (t) t->__c_ispeed = s; return 0; }
int cfsetspeed(struct termios *t, speed_t s) { if (t) { t->__c_ispeed = s; t->__c_ospeed = s; } return 0; }

pid_t tcgetpgrp(int fd) { (void)fd; return 1; }
int tcsetpgrp(int fd, pid_t p) { (void)fd; (void)p; return 0; }
pid_t tcgetsid(int fd) { (void)fd; return 1; }

/* Provide a TIOCGWINSZ-capable ioctl bridging to host_tty.get_size. The base
 * libc's ioctl is weak/stubbed for TTY ops on wasm, so we override it. Any
 * non-TTY request falls through to ENOTTY (sufficient for terminal apps). */
int ioctl(int fd, int request, ...) {
	va_list ap;
	va_start(ap, request);
	void *arg = va_arg(ap, void *);
	va_end(ap);
	if (request == TIOCGWINSZ) {
		struct winsize *ws = (struct winsize *)arg;
		if (!ws) { errno = EFAULT; return -1; }
		unsigned short cols = 0, rows = 0;
		unsigned int rc = __host_tty_get_size((unsigned int)fd, &cols, &rows);
		if (rc != 0) { errno = (int)rc; return -1; }
		ws->ws_col = cols;
		ws->ws_row = rows;
		ws->ws_xpixel = 0;
		ws->ws_ypixel = 0;
		return 0;
	}
	errno = ENOTTY;
	return -1;
}
