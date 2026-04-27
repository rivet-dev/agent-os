/**
 * Stub POSIX timezone globals + tzset() so ICU (and similar code that
 * still treats UNIX-y systems as having `timezone`/`tzname`) can link
 * against the patched WASI sysroot.
 *
 * The agent-OS VM is always UTC and has no /etc/localtime; running
 * with these stubs is equivalent to `TZ=UTC tzset()` on a normal libc.
 *
 * If a workload genuinely needs local-time conversion later, swap
 * these stubs for a host_time-backed implementation.
 */

#include <time.h>

long timezone = 0;
int daylight = 0;

static char *_aos_tzname[2] = {
    (char *)"UTC",
    (char *)"UTC",
};

char **__aos_tzname_ptr(void) { return _aos_tzname; }

/*
 * `tzname` on most systems is `extern char *tzname[2]`. wasi-libc
 * doesn't declare it. Provide it as a strong global so ICU's fallback
 * (`#define U_TZNAME tzname`) resolves.
 */
char *tzname[2] = {
    (char *)"UTC",
    (char *)"UTC",
};

void tzset(void) {
    /* no-op: VM is permanently UTC */
}
