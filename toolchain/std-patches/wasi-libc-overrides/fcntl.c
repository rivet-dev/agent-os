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
#include <stdint.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <unistd.h>
#include <wasi/api.h>

/* WASI headers omit F_DUPFD and F_DUPFD_CLOEXEC — define with Linux values */
#ifndef F_DUPFD
#define F_DUPFD 0
#endif
#ifndef F_DUPFD_CLOEXEC
#define F_DUPFD_CLOEXEC 1030
#endif

/* Spare Preview 1 fdflags bit reserved by the agentOS ABI. wasi-libc's
 * O_DIRECT value is an open(2)-only side-channel flag and does not fit in the
 * 16-bit WASI fdflags field, so translate it explicitly for F_GETFL/F_SETFL. */
#define AGENTOS_WASI_FDFLAG_DIRECT 0x20

/* Host import for dup with minimum fd (F_DUPFD semantics) */
__attribute__((import_module("host_process"), import_name("fd_dup_min")))
int __host_fd_dup_min(int fd, int min_fd, int *ret_new_fd);
__attribute__((import_module("host_process"), import_name("fd_getfd")))
int __host_fd_getfd(int fd, int *ret_flags);
__attribute__((import_module("host_process"), import_name("fd_setfd")))
int __host_fd_setfd(int fd, int flags);
__attribute__((import_module("host_process"), import_name("fd_record_lock")))
int __host_fd_record_lock(int fd, int command, int lock_type,
                          int64_t start, uint64_t length,
                          int *ret_type, uint32_t *ret_pid,
                          uint64_t *ret_start, uint64_t *ret_length);

static int _normalize_record_lock(int fd, const struct flock *lock,
                                  uint64_t *ret_start, uint64_t *ret_length) {
    int64_t base;
    switch (lock->l_whence) {
    case SEEK_SET:
        base = 0;
        break;
    case SEEK_CUR: {
        off_t offset = lseek(fd, 0, SEEK_CUR);
        if (offset == (off_t)-1) return -1;
        base = (int64_t)offset;
        break;
    }
    case SEEK_END: {
        struct stat stat;
        if (fstat(fd, &stat) != 0) return -1;
        base = (int64_t)stat.st_size;
        break;
    }
    default:
        errno = EINVAL;
        return -1;
    }

    int64_t start;
    if (__builtin_add_overflow(base, (int64_t)lock->l_start, &start) || start < 0) {
        errno = EINVAL;
        return -1;
    }
    int64_t signed_length = (int64_t)lock->l_len;
    if (signed_length < 0) {
        if (signed_length == INT64_MIN || start < -signed_length) {
            errno = EINVAL;
            return -1;
        }
        start += signed_length;
        signed_length = -signed_length;
    }
    *ret_start = (uint64_t)start;
    *ret_length = (uint64_t)signed_length;
    return 0;
}

/* Track active CLOEXEC descriptors sparsely: browser/runtime synthetic fd
 * numbers start high, so indexing by fd would waste memory. This collection
 * cannot outgrow the runtime's limits.resources.maxOpenFds bound because an
 * entry is accepted only for a live descriptor. */
static int *_cloexec_fds;
static uint32_t _cloexec_fd_count;
static uint32_t _cloexec_fd_capacity;

static int _fd_is_open(int fd) {
    __wasi_fdstat_t stat;
    return __wasi_fd_fdstat_get((__wasi_fd_t)fd, &stat) == 0;
}

static int _cloexec_index(int fd) {
    for (uint32_t i = 0; i < _cloexec_fd_count; i++)
        if (_cloexec_fds[i] == fd) return (int)i;
    return -1;
}

static int _set_cloexec_cache(int fd, int enabled) {
    int index = _cloexec_index(fd);
    if (!enabled) {
        if (index >= 0)
            _cloexec_fds[index] = _cloexec_fds[--_cloexec_fd_count];
        return 0;
    }
    if (index >= 0) return 0;
    if (_cloexec_fd_count == _cloexec_fd_capacity) {
        uint32_t capacity = _cloexec_fd_capacity ? _cloexec_fd_capacity * 2 : 16;
        if (capacity < _cloexec_fd_capacity ||
            capacity > UINT32_MAX / sizeof(*_cloexec_fds)) {
            errno = ENOMEM;
            return -1;
        }
        int *fds = realloc(_cloexec_fds, capacity * sizeof(*fds));
        if (!fds) {
            errno = ENOMEM;
            return -1;
        }
        _cloexec_fds = fds;
        _cloexec_fd_capacity = capacity;
    }
    _cloexec_fds[_cloexec_fd_count++] = fd;
    return 0;
}

static int _set_cloexec(int fd, int enabled) {
    int was_enabled = _cloexec_index(fd) >= 0;
    if (_set_cloexec_cache(fd, enabled) != 0) return -1;
    int err = __host_fd_setfd(fd, enabled ? FD_CLOEXEC : 0);
    if (err != 0) {
        /* Restoring a previously present entry cannot allocate: disabling it
         * retained the backing allocation, while a failed enable removes the
         * entry that was just appended. */
        _set_cloexec_cache(fd, was_enabled);
        errno = err;
        return -1;
    }
    return 0;
}

/* Shared by close/dup/pipe implementations in the patched libc. Newly opened
 * descriptor numbers must never inherit a stale flag from an earlier owner. */
int __agentos_set_cloexec_fd(int fd, int enabled) {
    return _set_cloexec(fd, enabled);
}

int close(int fd) {
    __wasi_errno_t err = __wasi_fd_close((__wasi_fd_t)fd);
    if (err != 0) {
        errno = err;
        return -1;
    }
    _set_cloexec_cache(fd, 0);
    return 0;
}

/* Used by execve() to send the close-on-exec set through host_process.proc_exec.
 * Closed descriptors are removed before serialization. */
uint32_t __agentos_copy_cloexec_fds(uint32_t *out, uint32_t capacity) {
    uint32_t index = 0;
    while (index < _cloexec_fd_count) {
        if (!_fd_is_open(_cloexec_fds[index])) {
            _cloexec_fds[index] = _cloexec_fds[--_cloexec_fd_count];
            continue;
        }
        index++;
    }
    uint32_t count = _cloexec_fd_count < capacity ? _cloexec_fd_count : capacity;
    for (uint32_t i = 0; i < count; i++) out[i] = (uint32_t)_cloexec_fds[i];
    return _cloexec_fd_count;
}

int fcntl(int fd, int cmd, ...) {
    va_list ap;
    va_start(ap, cmd);

    int result;

    switch (cmd) {
    case F_DUPFD: {
        int min_fd = va_arg(ap, int);
        /* No upper bound on the source fd: __host_fd_dup_min validates it and
         * returns EBADF for an invalid one. Only the cloexec cache write below
         * is bounded by MAX_FDS. */
        if (fd < 0) {
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
                _set_cloexec(new_fd, 0);
                result = new_fd;
            }
        }
        break;
    }

    case F_DUPFD_CLOEXEC: {
        int min_fd = va_arg(ap, int);
        /* See F_DUPFD: source fd validated by the host, not bounded here. */
        if (fd < 0) {
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
                if (_set_cloexec(new_fd, 1) != 0) {
                    int setfd_errno = errno;
                    if (close(new_fd) == 0)
                        errno = setfd_errno;
                    result = -1;
                } else {
                    result = new_fd;
                }
            }
        }
        break;
    }

    case F_GETFD:
        if (fd < 0) {
            errno = EBADF;
            result = -1;
        } else {
            int flags = 0;
            int err = __host_fd_getfd(fd, &flags);
            if (err != 0) {
                errno = err;
                result = -1;
            } else {
                result = flags & FD_CLOEXEC;
            }
        }
        break;

    case F_SETFD: {
        int arg = va_arg(ap, int);
        if (fd < 0) {
            errno = EBADF;
            result = -1;
        } else {
            result = _set_cloexec(fd, (arg & FD_CLOEXEC) != 0);
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
            if ((flags & AGENTOS_WASI_FDFLAG_DIRECT) != 0)
                flags = (flags & ~AGENTOS_WASI_FDFLAG_DIRECT) | O_DIRECT;
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
        __wasi_fdflags_t flags = (__wasi_fdflags_t)(arg & 0x1f);
        if ((arg & O_DIRECT) != 0)
            flags |= AGENTOS_WASI_FDFLAG_DIRECT;
        __wasi_errno_t err = __wasi_fd_fdstat_set_flags((__wasi_fd_t)fd, flags);
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
        } else if (lock->l_type != F_RDLCK && lock->l_type != F_WRLCK) {
            errno = EINVAL;
            result = -1;
        } else {
            uint64_t start, length, found_start, found_length;
            int found_type;
            uint32_t found_pid;
            if (_normalize_record_lock(fd, lock, &start, &length) != 0) {
                result = -1;
                break;
            }
            int err = __host_fd_record_lock(
                fd, cmd, lock->l_type, (int64_t)start, length,
                &found_type, &found_pid, &found_start, &found_length);
            if (err != 0) {
                errno = err;
                result = -1;
            } else {
                lock->l_type = (short)found_type;
                lock->l_pid = (pid_t)found_pid;
                if (found_type != F_UNLCK) {
                    lock->l_whence = SEEK_SET;
                    lock->l_start = (off_t)found_start;
                    lock->l_len = (off_t)found_length;
                }
                result = 0;
            }
        }
        break;
    }

    case F_SETLK:
    case F_SETLKW: {
        struct flock *lock = va_arg(ap, struct flock *);
        if (!lock) {
            errno = EINVAL;
            result = -1;
        } else if (lock->l_type != F_RDLCK && lock->l_type != F_WRLCK &&
                   lock->l_type != F_UNLCK) {
            errno = EINVAL;
            result = -1;
        } else {
            uint64_t start, length, ignored_start, ignored_length;
            int ignored_type;
            uint32_t ignored_pid;
            if (_normalize_record_lock(fd, lock, &start, &length) != 0) {
                result = -1;
                break;
            }
            int err = __host_fd_record_lock(
                fd, cmd, lock->l_type, (int64_t)start, length,
                &ignored_type, &ignored_pid, &ignored_start, &ignored_length);
            if (err != 0) {
                errno = err;
                result = -1;
            } else {
                result = 0;
            }
        }
        break;
    }

    default:
        errno = EINVAL;
        result = -1;
        break;
    }

    va_end(ap);
    return result;
}
