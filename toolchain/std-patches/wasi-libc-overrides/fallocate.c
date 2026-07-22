#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <sys/stat.h>
#include <unistd.h>

#ifndef FALLOC_FL_KEEP_SIZE
#define FALLOC_FL_KEEP_SIZE 0x01
#define FALLOC_FL_PUNCH_HOLE 0x02
#define FALLOC_FL_NO_HIDE_STALE 0x04
#define FALLOC_FL_COLLAPSE_RANGE 0x08
#define FALLOC_FL_ZERO_RANGE 0x10
#define FALLOC_FL_INSERT_RANGE 0x20
#define FALLOC_FL_UNSHARE_RANGE 0x40
#define FALLOC_FL_WRITE_ZEROES 0x80
#endif

uint32_t __agentos_host_fd_punch_hole(uint32_t fd, uint64_t offset,
                                      uint64_t length) __attribute__((
    __import_module__("host_fs"), __import_name__("fd_punch_hole")));
uint32_t __agentos_host_fd_zero_range(uint32_t fd, uint64_t offset,
                                     uint64_t length,
                                     uint32_t keep_size) __attribute__((
    __import_module__("host_fs"), __import_name__("fd_zero_range")));
uint32_t __agentos_host_fd_insert_range(uint32_t fd, uint64_t offset,
                                       uint64_t length) __attribute__((
    __import_module__("host_fs"), __import_name__("fd_insert_range")));
uint32_t __agentos_host_fd_collapse_range(uint32_t fd, uint64_t offset,
                                         uint64_t length) __attribute__((
    __import_module__("host_fs"), __import_name__("fd_collapse_range")));

static int host_result(uint32_t error) {
    if (error == 0) return 0;
    errno = (int)error;
    return -1;
}

static int allocate_range(int fd, off_t offset, off_t length, int keep_size) {
    struct stat before;
    if (fstat(fd, &before) != 0) return -1;

    off_t end = offset + length;
    if (!keep_size || end <= before.st_size) {
        int error = posix_fallocate(fd, offset, length);
        if (error != 0) {
            errno = error;
            return -1;
        }
        return 0;
    }

    /* Allocate the visible prefix through Preview1 without extending it. */
    if (offset < before.st_size) {
        int error = posix_fallocate(fd, offset, before.st_size - offset);
        if (error != 0) {
            errno = error;
            return -1;
        }
    }

    /* Beyond EOF there are no existing bytes for ZERO_RANGE to alter. The
     * agentOS range import can therefore retain the allocation metadata while
     * preserving i_size, unlike allocate-then-truncate which discards it. */
    off_t beyond_eof = offset > before.st_size ? offset : before.st_size;
    return host_result(__agentos_host_fd_zero_range(
        (uint32_t)fd, (uint64_t)beyond_eof, (uint64_t)(end - beyond_eof), 1));
}

int fallocate(int fd, int mode, off_t offset, off_t length) {
    if (offset < 0 || length <= 0 || offset > INT64_MAX - length) {
        errno = EINVAL;
        return -1;
    }

    switch (mode) {
    case 0:
        return allocate_range(fd, offset, length, 0);
    case FALLOC_FL_KEEP_SIZE:
        return allocate_range(fd, offset, length, 1);
    case FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE:
        return host_result(__agentos_host_fd_punch_hole(
            (uint32_t)fd, (uint64_t)offset, (uint64_t)length));
    case FALLOC_FL_ZERO_RANGE:
    case FALLOC_FL_ZERO_RANGE | FALLOC_FL_KEEP_SIZE:
    case FALLOC_FL_WRITE_ZEROES:
    case FALLOC_FL_WRITE_ZEROES | FALLOC_FL_KEEP_SIZE:
        return host_result(__agentos_host_fd_zero_range(
            (uint32_t)fd, (uint64_t)offset, (uint64_t)length,
            (uint32_t)((mode & FALLOC_FL_KEEP_SIZE) != 0)));
    case FALLOC_FL_INSERT_RANGE:
        return host_result(__agentos_host_fd_insert_range(
            (uint32_t)fd, (uint64_t)offset, (uint64_t)length));
    case FALLOC_FL_COLLAPSE_RANGE:
        return host_result(__agentos_host_fd_collapse_range(
            (uint32_t)fd, (uint64_t)offset, (uint64_t)length));
    case FALLOC_FL_UNSHARE_RANGE:
    case FALLOC_FL_UNSHARE_RANGE | FALLOC_FL_KEEP_SIZE: {
        struct stat file;
        if (fstat(fd, &file) != 0) return -1;
        /* agentOS files have no guest-visible reflinked extents, so every
         * valid range is already unshared. */
        return 0;
    }
    default:
        errno = EOPNOTSUPP;
        return -1;
    }
}
