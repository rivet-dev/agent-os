#include <errno.h>
#include <limits.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static int print_sysconf(int name, const char *variable) {
    errno = 0;
    long value = sysconf(name);
    if (value == -1) {
        if (errno == 0) {
            puts("undefined");
            return 0;
        }
        fprintf(stderr, "getconf: %s: %s\n", variable, strerror(errno));
        return 2;
    }
    printf("%ld\n", value);
    return 0;
}

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: getconf VARIABLE\n");
        return 2;
    }

    const char *variable = argv[1];
    if (!strcmp(variable, "PAGE_SIZE") || !strcmp(variable, "PAGESIZE"))
        return print_sysconf(_SC_PAGESIZE, variable);
#ifdef _SC_NPROCESSORS_CONF
    if (!strcmp(variable, "_NPROCESSORS_CONF"))
        return print_sysconf(_SC_NPROCESSORS_CONF, variable);
#endif
#ifdef _SC_NPROCESSORS_ONLN
    if (!strcmp(variable, "_NPROCESSORS_ONLN"))
        return print_sysconf(_SC_NPROCESSORS_ONLN, variable);
#endif
    if (!strcmp(variable, "LONG_BIT")) {
        printf("%zu\n", sizeof(long) * CHAR_BIT);
        return 0;
    }
    if (!strcmp(variable, "ULONG_MAX")) {
        printf("%lu\n", ULONG_MAX);
        return 0;
    }

    fprintf(stderr, "getconf: unrecognized configuration variable '%s'\n", variable);
    return 2;
}
