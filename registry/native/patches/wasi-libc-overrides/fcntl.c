/**
 * Fix for wasi-libc's broken fcntl implementation.
 *
 * wasi-libc always returns FD_CLOEXEC(1) for F_GETFD and ignores F_SETFD
 * because WASI has no exec(). It also returns EINVAL for F_DUPFD and
 * F_DUPFD_CLOEXEC. This fix properly tracks per-fd cloexec flags,
 * delegates F_GETFL/F_SETFL to the original WASI fd_fdstat interface,
 * and routes F_DUPFD/F_DUPFD_CLOEXEC through the host_process bridge.
 *
 * Installed into the patched sysroot so ALL WASM programs get correct
 * fcntl behavior, not just test binaries.
 */

#include <stdarg.h>
#include <errno.h>
#include <fcntl.h>
#include <wasi/api.h>

/* WASI headers omit F_DUPFD and F_DUPFD_CLOEXEC — define with Linux values */
#ifndef F_DUPFD
#define F_DUPFD 0
#endif
#ifndef F_DUPFD_CLOEXEC
#define F_DUPFD_CLOEXEC 1030
#endif

/* Host import for dup with minimum fd (F_DUPFD semantics) */
__attribute__((import_module("host_process"), import_name("fd_dup_min")))
int __host_fd_dup_min(int fd, int min_fd, int *ret_new_fd);

/* Per-fd cloexec tracking (up to 256 FDs) */
#define MAX_FDS 256
static unsigned char _fd_cloexec[MAX_FDS];

int fcntl(int fd, int cmd, ...) {
    va_list ap;
    va_start(ap, cmd);

    int result;

    switch (cmd) {
    case F_DUPFD: {
        int min_fd = va_arg(ap, int);
        if (fd < 0 || fd >= MAX_FDS) {
            errno = EBADF;
            result = -1;
        } else if (min_fd < 0) {
            errno = EINVAL;
            result = -1;
        } else {
            int new_fd;
            int err = __host_fd_dup_min(fd, min_fd, &new_fd);
            if (err != 0) {
                errno = err;
                result = -1;
            } else {
                if (new_fd >= 0 && new_fd < MAX_FDS)
                    _fd_cloexec[new_fd] = 0;
                result = new_fd;
            }
        }
        break;
    }

    case F_DUPFD_CLOEXEC: {
        int min_fd = va_arg(ap, int);
        if (fd < 0 || fd >= MAX_FDS) {
            errno = EBADF;
            result = -1;
        } else if (min_fd < 0) {
            errno = EINVAL;
            result = -1;
        } else {
            int new_fd;
            int err = __host_fd_dup_min(fd, min_fd, &new_fd);
            if (err != 0) {
                errno = err;
                result = -1;
            } else {
                if (new_fd >= 0 && new_fd < MAX_FDS)
                    _fd_cloexec[new_fd] = 1;
                result = new_fd;
            }
        }
        break;
    }

    case F_GETFD:
        if (fd < 0 || fd >= MAX_FDS) {
            errno = EBADF;
            result = -1;
        } else {
            result = _fd_cloexec[fd] ? FD_CLOEXEC : 0;
        }
        break;

    case F_SETFD: {
        int arg = va_arg(ap, int);
        if (fd < 0 || fd >= MAX_FDS) {
            errno = EBADF;
            result = -1;
        } else {
            _fd_cloexec[fd] = (arg & FD_CLOEXEC) ? 1 : 0;
            result = 0;
        }
        break;
    }

    case F_GETFL: {
        __wasi_fdstat_t stat;
        __wasi_errno_t err = __wasi_fd_fdstat_get((__wasi_fd_t)fd, &stat);
        if (err != 0) {
            errno = err;
            result = -1;
        } else {
            int flags = stat.fs_flags;
            /* Derive read/write mode from rights */
            __wasi_rights_t r = stat.fs_rights_base;
            int can_read  = (r & __WASI_RIGHTS_FD_READ) != 0;
            int can_write = (r & __WASI_RIGHTS_FD_WRITE) != 0;
            if (can_read && can_write)
                flags |= O_RDWR;
            else if (can_read)
                flags |= O_RDONLY;
            else if (can_write)
                flags |= O_WRONLY;
            result = flags;
        }
        break;
    }

    case F_SETFL: {
        int arg = va_arg(ap, int);
        __wasi_errno_t err = __wasi_fd_fdstat_set_flags(
            (__wasi_fd_t)fd,
            (__wasi_fdflags_t)(arg & 0xfff));
        if (err != 0) {
            errno = err;
            result = -1;
        } else {
            result = 0;
        }
        break;
    }

    case F_GETLK: {
        struct flock *lock = va_arg(ap, struct flock *);
        if (!lock) {
            errno = EINVAL;
            result = -1;
        } else {
            lock->l_type = F_UNLCK;
            lock->l_pid = 0;
            result = 0;
        }
        break;
    }

    case F_SETLK:
    case F_SETLKW:
        // WASI has no kernel-level advisory locking. Treat locks as a
        // successful no-op so single-process workloads like DuckDB can open
        // writable database files on the VFS-backed filesystem.
        result = 0;
        break;

    default:
        errno = EINVAL;
        result = -1;
        break;
    }

    va_end(ap);
    return result;
}
