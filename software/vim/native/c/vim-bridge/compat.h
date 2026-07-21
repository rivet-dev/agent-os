/* compat.h — forced-include shim filling full-OS libc gaps for porting
 * Linux C programs (vim) to the agentos wasm runtime. */
#ifndef _AGENTOS_WASM_COMPAT_H
#define _AGENTOS_WASM_COMPAT_H
#include <sys/types.h>
#include <poll.h>
#include <sys/time.h>
#include <signal.h>
/* Force termios everywhere so TIOCGWINSZ + struct winsize are visible in EVERY
 * translation unit (os_unix.c's window-size query is gated on TIOCGWINSZ, which
 * is only defined where <termios.h> is included). */
#include <termios.h>

/* wasi-libc declares no sigprocmask (no signals), but our posix_stubs.c
 * implements it and configure detects it (HAVE_SIGPROCMASK) — declare it so
 * os_unix.c's calls compile instead of failing with implicit-declaration. */
int sigprocmask(int how, const sigset_t *set, sigset_t *oldset);

/* waitpid flags (no sys/wait.h with these on wasi) */
#ifndef WNOHANG
#define WNOHANG 1
#endif
#ifndef WUNTRACED
#define WUNTRACED 2
#endif
#ifndef WCONTINUED
#define WCONTINUED 8
#endif

/* interval timers — wasi-libc guards these out; we expose + stub them */
#ifndef ITIMER_REAL
#define ITIMER_REAL 0
#define ITIMER_VIRTUAL 1
#define ITIMER_PROF 2
struct itimerval { struct timeval it_interval; struct timeval it_value; };
int setitimer(int, const struct itimerval *, struct itimerval *);
int getitimer(int, struct itimerval *);
#endif

mode_t umask(mode_t);

#endif
