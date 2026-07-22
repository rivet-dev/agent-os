#ifndef AGENTOS_XFSTESTS_HELPER_COMPAT_H
#define AGENTOS_XFSTESTS_HELPER_COMPAT_H

#define HAVE_RENAMEAT2 1

#include <errno.h>
#include <fcntl.h>
#include <getopt.h>
#include <netdb.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/ioctl.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>

#include <posix_spawn_compat.h>

extern char **environ;

#ifdef __wasi__
pid_t fork(void);
int agentos_mknod(const char *path, mode_t mode, dev_t device);
int agentos_mknodat(int dirfd, const char *path, mode_t mode, dev_t device);
int agentos_getdents64(int fd, void *buffer, size_t length);
struct hostent *agentos_gethostbyname(const char *name);
char *agentos_strsignal(int signal_number);

#define gethostbyname agentos_gethostbyname
#define mknod agentos_mknod
#define mknodat agentos_mknodat
#define strsignal agentos_strsignal

/* Wasm EH provides setjmp/longjmp through libsetjmp. The pinned fsstress
 * source only uses the signal-mask variants to recover from SIGBUS, and the
 * agentOS guest signal model does not expose a distinct process mask here. */
#define sigsetjmp(buffer, save_mask) setjmp(buffer)
#define siglongjmp(buffer, value) longjmp(buffer, value)

#ifndef F_SETLEASE
#define F_SETLEASE 1024
#endif
#ifndef F_GETLEASE
#define F_GETLEASE 1025
#endif
#ifndef F_SETSIG
#define F_SETSIG 10
#endif
#ifndef SO_LINGER
#define SO_LINGER 13
#endif
#ifndef SA_SIGINFO
typedef struct {
    int si_fd;
} siginfo_t;
#define SA_SIGINFO 0x00000004
#endif

static inline void *agentos_memalign(size_t alignment, size_t size) {
    void *allocation = NULL;
    int error = posix_memalign(&allocation, alignment, size);

    if (error != 0) {
        errno = error;
        return NULL;
    }
    return allocation;
}

#define memalign agentos_memalign

/* looptest combines this with O_RDWR, which makes WASI reject the unsupported
 * direct-I/O request as an invalid access mode instead of silently ignoring it. */
#ifndef O_DIRECT
#define O_DIRECT O_SEARCH
#endif

typedef off_t off64_t;
typedef ino_t ino64_t;
#define ftruncate64 ftruncate
#define fstat64 fstat
#define lstat64 lstat
#define lseek64 lseek
#define stat64 stat

static inline int agentos_system(const char *command) {
    pid_t child;
    int status;
    char *argv[] = { "sh", "-c", (char *)command, NULL };
    int error;

    if (command == NULL)
        return 1;
    error = posix_spawnp(&child, "sh", NULL, NULL, argv, environ);
    if (error != 0) {
        errno = error;
        return -1;
    }
    if (waitpid(child, &status, 0) < 0)
        return -1;
    return status;
}

#define system agentos_system
#endif

#endif
