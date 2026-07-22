/**
 * Bounded file-backed mmap emulation for agentOS WASM guests.
 *
 * Linear memory cannot provide host page-fault mappings. This implementation
 * snapshots file bytes into malloc-backed memory and writes MAP_SHARED ranges
 * back on msync()/munmap(). MAP_PRIVATE mappings remain isolated.
 */
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/uio.h>
#include <unistd.h>
#include <wasi/api.h>

#define MAX_MAPPINGS 1024

struct mapping {
    void *address;
    size_t length;
    size_t allocation_length;
    int prot;
    int flags;
    int fd;
    off_t offset;
    dev_t device;
    ino_t inode;
    size_t page_size;
    unsigned char *private_dirty_pages;
};

static struct mapping mappings[MAX_MAPPINGS];
static size_t mapping_count;
static size_t file_mapping_count;
static size_t private_file_mapping_count;
static pthread_mutex_t mapping_mutex = PTHREAD_MUTEX_INITIALIZER;

static int lock_mappings(void) {
    int error = pthread_mutex_lock(&mapping_mutex);
    if (error != 0) {
        errno = error;
        return -1;
    }
    return 0;
}

static void unlock_mappings(void) {
    int saved_errno = errno;
    int error = pthread_mutex_unlock(&mapping_mutex);
    if (error != 0)
        fprintf(stderr, "agentos: failed to unlock mmap registry: %s\n",
                strerror(error));
    errno = saved_errno;
}

static int dirty_page(const struct mapping *mapping, size_t page) {
    return mapping->private_dirty_pages &&
        (mapping->private_dirty_pages[page / 8] & (1u << (page % 8))) != 0;
}

static void mark_page_dirty(struct mapping *mapping, size_t page) {
    mapping->private_dirty_pages[page / 8] |= (unsigned char)(1u << (page % 8));
}

static int same_file(const struct mapping *mapping, const struct stat *status) {
    return mapping->fd >= 0 && mapping->device == status->st_dev &&
        mapping->inode == status->st_ino;
}

static int set_direct_io(int fd, int enabled, int *original_flags) {
    *original_flags = fcntl(fd, F_GETFL);
    if (*original_flags < 0) return -1;
    if (((*original_flags & O_DIRECT) != 0) == enabled) return 0;
    int flags = enabled ? *original_flags | O_DIRECT : *original_flags & ~O_DIRECT;
    return fcntl(fd, F_SETFL, flags);
}

static int restore_status_flags(int fd, int original_flags, int result,
                                int operation_error) {
    if (fcntl(fd, F_SETFL, original_flags) != 0 && result == 0) return -1;
    if (result != 0) errno = operation_error;
    return result;
}

static ssize_t mapping_pread(struct mapping *mapping, void *buffer, size_t length,
                             off_t offset) {
    int original_flags;
    if (set_direct_io(mapping->fd, 0, &original_flags) != 0) return -1;

    unsigned char *cursor = buffer;
    size_t remaining = length;
    while (remaining) {
        ssize_t count = pread(mapping->fd, cursor, remaining, offset);
        if (count < 0 && errno == EINTR) continue;
        if (count < 0) {
            int error = errno;
            restore_status_flags(mapping->fd, original_flags, -1, error);
            return -1;
        }
        if (count == 0) break;
        cursor += count;
        remaining -= (size_t)count;
        offset += count;
    }
    memset(cursor, 0, remaining);
    if (restore_status_flags(mapping->fd, original_flags, 0, 0) != 0) return -1;
    return (ssize_t)(length - remaining);
}

static int write_range(off_t offset, size_t length, uint64_t *begin, uint64_t *end) {
    if (offset < 0) {
        errno = EINVAL;
        return -1;
    }
    *begin = (uint64_t)offset;
    if (__builtin_add_overflow(*begin, (uint64_t)length, end)) {
        errno = EOVERFLOW;
        return -1;
    }
    return 0;
}

/* Detect copy-on-write pages before the underlying file changes. Linear WASM
 * memory has no page faults, so a private mapping starts clean and becomes
 * permanently private when its bytes differ from the corresponding file page. */
static int classify_private_pages(int fd, off_t offset, size_t length) {
    if (!length || private_file_mapping_count == 0) return 0;
    struct stat status;
    if (fstat(fd, &status) != 0) return -1;

    uint64_t write_begin;
    uint64_t write_end;
    if (write_range(offset, length, &write_begin, &write_end) != 0) return -1;

    unsigned char *scratch = NULL;
    size_t scratch_length = 0;
    for (size_t i = 0; i < MAX_MAPPINGS; i++) {
        struct mapping *mapping = &mappings[i];
        if (!mapping->address || !mapping->private_dirty_pages ||
            !same_file(mapping, &status)) continue;

        uint64_t map_begin = (uint64_t)mapping->offset;
        uint64_t map_end;
        if (__builtin_add_overflow(map_begin, (uint64_t)mapping->length, &map_end))
            continue;
        if (write_begin >= map_end || write_end <= map_begin) continue;

        uint64_t overlap_begin = write_begin > map_begin ? write_begin : map_begin;
        uint64_t overlap_end = write_end < map_end ? write_end : map_end;
        size_t first_page = (size_t)((overlap_begin - map_begin) / mapping->page_size);
        size_t last_page = (size_t)((overlap_end - 1 - map_begin) / mapping->page_size);
        if (scratch_length < mapping->page_size) {
            unsigned char *resized = realloc(scratch, mapping->page_size);
            if (!resized) {
                free(scratch);
                errno = ENOMEM;
                return -1;
            }
            scratch = resized;
            scratch_length = mapping->page_size;
        }

        for (size_t page = first_page; page <= last_page; page++) {
            if (dirty_page(mapping, page)) continue;
            size_t relative = page * mapping->page_size;
            size_t page_length = mapping->length - relative;
            if (page_length > mapping->page_size) page_length = mapping->page_size;
            if (mapping_pread(mapping, scratch, page_length,
                              mapping->offset + (off_t)relative) < 0) {
                free(scratch);
                return -1;
            }
            if (memcmp((unsigned char *)mapping->address + relative, scratch,
                       page_length) != 0)
                mark_page_dirty(mapping, page);
        }
    }
    free(scratch);
    return 0;
}

/* Refresh file-write results into mappings. MAP_SHARED always observes the
 * write. MAP_PRIVATE observes it until the destination page has been modified
 * through the mapping, matching Linux copy-on-write behavior. */
static int refresh_mappings(int fd, off_t offset, size_t length) {
    if (!length || file_mapping_count == 0) return 0;
    struct stat status;
    if (fstat(fd, &status) != 0) return -1;

    uint64_t write_begin;
    uint64_t write_end;
    if (write_range(offset, length, &write_begin, &write_end) != 0) return -1;

    for (size_t i = 0; i < MAX_MAPPINGS; i++) {
        struct mapping *mapping = &mappings[i];
        if (!mapping->address || !same_file(mapping, &status)) continue;
        uint64_t map_begin = (uint64_t)mapping->offset;
        uint64_t map_end;
        if (__builtin_add_overflow(map_begin, (uint64_t)mapping->length, &map_end))
            continue;
        if (write_begin >= map_end || write_end <= map_begin) continue;

        uint64_t overlap_begin = write_begin > map_begin ? write_begin : map_begin;
        uint64_t overlap_end = write_end < map_end ? write_end : map_end;
        while (overlap_begin < overlap_end) {
            size_t relative = (size_t)(overlap_begin - map_begin);
            size_t page = relative / mapping->page_size;
            uint64_t page_end = map_begin + (uint64_t)(page + 1) * mapping->page_size;
            uint64_t chunk_end = overlap_end < page_end ? overlap_end : page_end;
            size_t chunk_length = (size_t)(chunk_end - overlap_begin);
            if ((mapping->flags & MAP_SHARED) != 0 || !dirty_page(mapping, page)) {
                if (mapping_pread(mapping,
                                  (unsigned char *)mapping->address + relative,
                                  chunk_length, (off_t)overlap_begin) < 0)
                    return -1;
            }
            overlap_begin = chunk_end;
        }
    }
    return 0;
}

static ssize_t raw_pwritev(int fd, const struct iovec *iov, int count,
                           off_t offset, int explain_capability) {
    if (count < 0 || offset < 0) {
        errno = EINVAL;
        return -1;
    }
    size_t written;
    __wasi_errno_t error = __wasi_fd_pwrite(
        fd, (const __wasi_ciovec_t *)iov, count, offset, &written);
    if (error != 0) {
        if (explain_capability && error == ENOTCAPABLE) {
            __wasi_fdstat_t descriptor;
            if (__wasi_fd_fdstat_get(fd, &descriptor) == 0) {
                error = (descriptor.fs_rights_base & __WASI_RIGHTS_FD_WRITE) == 0
                    ? EBADF
                    : ESPIPE;
            }
        }
        errno = error;
        return -1;
    }
    return (ssize_t)written;
}

static ssize_t pwrite_unlocked(int fd, const void *buffer, size_t length,
                               off_t offset) {
    if (classify_private_pages(fd, offset, length) != 0) return -1;
    struct iovec iov = {.iov_base = (void *)buffer, .iov_len = length};
    ssize_t written = raw_pwritev(fd, &iov, 1, offset, 1);
    if (written > 0 && refresh_mappings(fd, offset, (size_t)written) != 0)
        fprintf(stderr, "agentos: failed to refresh mmap after pwrite: %s\n",
                strerror(errno));
    return written;
}

static ssize_t pwritev_unlocked(int fd, const struct iovec *iov, int count,
                                off_t offset) {
    if (count < 0) {
        errno = EINVAL;
        return -1;
    }
    size_t length = 0;
    for (int i = 0; i < count; i++) {
        if (__builtin_add_overflow(length, iov[i].iov_len, &length)) {
            errno = EINVAL;
            return -1;
        }
    }
    if (classify_private_pages(fd, offset, length) != 0) return -1;
    ssize_t written = raw_pwritev(fd, iov, count, offset, 0);
    if (written > 0 && refresh_mappings(fd, offset, (size_t)written) != 0)
        fprintf(stderr, "agentos: failed to refresh mmap after pwritev: %s\n",
                strerror(errno));
    return written;
}

ssize_t pwrite(int fd, const void *buffer, size_t length, off_t offset) {
    if (lock_mappings() != 0) return -1;
    ssize_t result = pwrite_unlocked(fd, buffer, length, offset);
    unlock_mappings();
    return result;
}

ssize_t pwritev(int fd, const struct iovec *iov, int count, off_t offset) {
    if (lock_mappings() != 0) return -1;
    ssize_t result = pwritev_unlocked(fd, iov, count, offset);
    unlock_mappings();
    return result;
}

static int mapping_geometry(size_t length, size_t *allocation_length, size_t *alignment) {
    long page_size = sysconf(_SC_PAGESIZE);
    if (page_size <= 0) {
        errno = EINVAL;
        return -1;
    }
    size_t page = (size_t)page_size;
    size_t remainder = length % page;
    size_t padding = remainder ? page - remainder : 0;
    if (length > SIZE_MAX - padding) {
        errno = ENOMEM;
        return -1;
    }
    *allocation_length = length + padding;
    *alignment = page;
    return 0;
}

static void *allocate_mapping(size_t length, size_t alignment) {
    void *buffer = NULL;
    int error = posix_memalign(&buffer, alignment, length);
    if (error != 0) {
        errno = error;
        return NULL;
    }
    memset(buffer, 0, length);
    return buffer;
}

static struct mapping *find_mapping(void *address, size_t length) {
    uintptr_t begin = (uintptr_t)address;
    uintptr_t end;
    if (__builtin_add_overflow(begin, length, &end)) return NULL;
    for (size_t i = 0; i < MAX_MAPPINGS; i++) {
        uintptr_t map_begin = (uintptr_t)mappings[i].address;
        uintptr_t map_end;
        if (!map_begin || __builtin_add_overflow(map_begin, mappings[i].length, &map_end)) continue;
        if (begin >= map_begin && end <= map_end) return &mappings[i];
    }
    return NULL;
}

static int write_back(struct mapping *mapping, void *address, size_t length) {
    if (mapping->fd < 0 || (mapping->flags & MAP_SHARED) == 0 ||
        (mapping->prot & PROT_WRITE) == 0) return 0;

    /* Linux mmap writeback is not subject to the O_DIRECT alignment rules of
     * the descriptor used to create the mapping. Our emulation uses pwrite,
     * so temporarily clear the shared status flag and restore it on every
     * exit path. WASM guests are single-threaded while this code executes. */
    int original_flags = fcntl(mapping->fd, F_GETFL);
    if (original_flags < 0) return -1;
    int direct_disabled = (original_flags & O_DIRECT) != 0;
    if (direct_disabled && fcntl(mapping->fd, F_SETFL, original_flags & ~O_DIRECT) != 0)
        return -1;

    size_t relative = (uintptr_t)address - (uintptr_t)mapping->address;
    const unsigned char *cursor = address;
    size_t remaining = length;
    int result = 0;
    int write_error = 0;
    if (classify_private_pages(mapping->fd,
                               mapping->offset + (off_t)relative,
                               length) != 0) {
        int error = errno;
        if (direct_disabled) fcntl(mapping->fd, F_SETFL, original_flags);
        errno = error;
        return -1;
    }
    while (remaining) {
        struct iovec iov = {
            .iov_base = (void *)cursor,
            .iov_len = remaining,
        };
        ssize_t written = raw_pwritev(mapping->fd, &iov, 1,
                                      mapping->offset + (off_t)relative, 1);
        if (written < 0 && errno == EINTR) continue;
        if (written <= 0) {
            if (written == 0) errno = EIO;
            write_error = errno;
            result = -1;
            break;
        }
        cursor += written;
        remaining -= (size_t)written;
        relative += (size_t)written;
    }
    if (direct_disabled && fcntl(mapping->fd, F_SETFL, original_flags) != 0) {
        if (result == 0) return -1;
    }
    if (result != 0) errno = write_error;
    if (result == 0 &&
        refresh_mappings(mapping->fd,
                         mapping->offset +
                             (off_t)((uintptr_t)address -
                                     (uintptr_t)mapping->address),
                         length) != 0)
        fprintf(stderr, "agentos: failed to refresh mmap after writeback: %s\n",
                strerror(errno));
    return result;
}

static void *mmap_unlocked(void *address, size_t length, int prot, int flags,
                           int fd, off_t offset) {
    if (address || !length || offset < 0 ||
        ((flags & MAP_PRIVATE) == 0 && (flags & MAP_SHARED) == 0) ||
        ((flags & MAP_PRIVATE) != 0 && (flags & MAP_SHARED) != 0) ||
        (flags & MAP_FIXED) != 0 || (prot & PROT_EXEC) != 0 || prot == PROT_NONE) {
        errno = EINVAL;
        return MAP_FAILED;
    }

    size_t allocation_length;
    size_t alignment;
    if (mapping_geometry(length, &allocation_length, &alignment) != 0)
        return MAP_FAILED;

    struct mapping *slot = NULL;
    for (size_t i = 0; i < MAX_MAPPINGS; i++) {
        if (!mappings[i].address) {
            slot = &mappings[i];
            break;
        }
    }
    if (!slot) {
        errno = ENOMEM;
        return MAP_FAILED;
    }

    unsigned char *buffer = allocate_mapping(allocation_length, alignment);
    if (!buffer) {
        errno = ENOMEM;
        return MAP_FAILED;
    }

    int retained_fd = -1;
    struct stat retained_status = {0};
    if ((flags & MAP_ANONYMOUS) == 0) {
        retained_fd = dup(fd);
        if (retained_fd < 0) {
            free(buffer);
            return MAP_FAILED;
        }
        if (fstat(retained_fd, &retained_status) != 0) {
            close(retained_fd);
            free(buffer);
            return MAP_FAILED;
        }
        size_t remaining = allocation_length;
        unsigned char *cursor = buffer;
        off_t read_offset = offset;
        while (remaining) {
            ssize_t count = pread(retained_fd, cursor, remaining, read_offset);
            if (count < 0 && errno == EINTR) continue;
            if (count < 0) {
                close(retained_fd);
                free(buffer);
                return MAP_FAILED;
            }
            if (count == 0) break;
            cursor += count;
            remaining -= (size_t)count;
            read_offset += count;
        }
    }

    unsigned char *private_dirty_pages = NULL;
    if (retained_fd >= 0 && (flags & MAP_PRIVATE) != 0) {
        size_t page_count = allocation_length / alignment;
        private_dirty_pages = calloc((page_count + 7) / 8, 1);
        if (!private_dirty_pages) {
            close(retained_fd);
            free(buffer);
            errno = ENOMEM;
            return MAP_FAILED;
        }
    }

    *slot = (struct mapping){
        .address = buffer,
        .length = length,
        .allocation_length = allocation_length,
        .prot = prot,
        .flags = flags,
        .fd = retained_fd,
        .offset = offset,
        .device = retained_status.st_dev,
        .inode = retained_status.st_ino,
        .page_size = alignment,
        .private_dirty_pages = private_dirty_pages,
    };
    mapping_count++;
    if (retained_fd >= 0) {
        file_mapping_count++;
        if ((flags & MAP_PRIVATE) != 0) private_file_mapping_count++;
    }
    if (mapping_count == (MAX_MAPPINGS * 9) / 10) {
        fprintf(stderr,
                "agentos: mmap table is %zu/%d full; unmap ranges before the %d-entry limit\n",
                mapping_count, MAX_MAPPINGS, MAX_MAPPINGS);
    }
    return buffer;
}

void *mmap(void *address, size_t length, int prot, int flags, int fd,
           off_t offset) {
    if (lock_mappings() != 0) return MAP_FAILED;
    void *result = mmap_unlocked(address, length, prot, flags, fd, offset);
    unlock_mappings();
    return result;
}

static int msync_unlocked(void *address, size_t length, int flags) {
    if ((flags & ~(MS_ASYNC | MS_INVALIDATE | MS_SYNC)) != 0 ||
        ((flags & MS_ASYNC) != 0 && (flags & MS_SYNC) != 0)) {
        errno = EINVAL;
        return -1;
    }
    struct mapping *mapping = find_mapping(address, length);
    if (!mapping) {
        errno = ENOMEM;
        return -1;
    }
    return write_back(mapping, address, length);
}

int msync(void *address, size_t length, int flags) {
    if (lock_mappings() != 0) return -1;
    int result = msync_unlocked(address, length, flags);
    unlock_mappings();
    return result;
}

static int munmap_unlocked(void *address, size_t length) {
    struct mapping *mapping = find_mapping(address, length);
    if (!mapping || mapping->address != address || mapping->length != length) {
        errno = EINVAL;
        return -1;
    }
    if (write_back(mapping, address, length) != 0) return -1;
    if (mapping->fd >= 0 && close(mapping->fd) != 0) return -1;
    if (mapping->fd >= 0) {
        file_mapping_count--;
        if ((mapping->flags & MAP_PRIVATE) != 0) private_file_mapping_count--;
    }
    free(mapping->private_dirty_pages);
    free(mapping->address);
    memset(mapping, 0, sizeof(*mapping));
    mapping_count--;
    return 0;
}

int munmap(void *address, size_t length) {
    if (lock_mappings() != 0) return -1;
    int result = munmap_unlocked(address, length);
    unlock_mappings();
    return result;
}

static void *mremap_unlocked(void *old_address, size_t old_length,
                             size_t new_length, int flags) {
    if (!new_length || (flags & ~(MREMAP_MAYMOVE | MREMAP_FIXED)) != 0 ||
        (flags & MREMAP_FIXED) != 0) {
        errno = EINVAL;
        return MAP_FAILED;
    }
    struct mapping *mapping = find_mapping(old_address, old_length);
    if (!mapping || mapping->address != old_address || mapping->length != old_length) {
        errno = EFAULT;
        return MAP_FAILED;
    }

    size_t new_allocation_length;
    size_t alignment;
    if (mapping_geometry(new_length, &new_allocation_length, &alignment) != 0)
        return MAP_FAILED;

    /* Preserve dirty shared pages before realloc can discard a shrinking tail. */
    if (write_back(mapping, old_address, old_length) != 0) return MAP_FAILED;

    size_t old_allocation_length = mapping->allocation_length;
    unsigned char *resized = allocate_mapping(new_allocation_length, alignment);
    if (!resized) return MAP_FAILED;
    unsigned char *resized_dirty_pages = NULL;
    if (mapping->private_dirty_pages) {
        size_t old_page_count = old_allocation_length / mapping->page_size;
        size_t new_page_count = new_allocation_length / alignment;
        size_t new_dirty_bytes = (new_page_count + 7) / 8;
        resized_dirty_pages = calloc(new_dirty_bytes, 1);
        if (!resized_dirty_pages) {
            free(resized);
            errno = ENOMEM;
            return MAP_FAILED;
        }
        size_t old_dirty_bytes = (old_page_count + 7) / 8;
        if (old_dirty_bytes > new_dirty_bytes) old_dirty_bytes = new_dirty_bytes;
        memcpy(resized_dirty_pages, mapping->private_dirty_pages, old_dirty_bytes);
    }
    size_t preserved_length = old_allocation_length < new_allocation_length
        ? old_allocation_length
        : new_allocation_length;
    memcpy(resized, old_address, preserved_length);

    size_t tail_length = new_allocation_length > old_allocation_length
        ? new_allocation_length - old_allocation_length
        : 0;
    if (tail_length && mapping->fd >= 0) {
        unsigned char *tail = resized + old_allocation_length;
        unsigned char *cursor = tail;
        size_t remaining = tail_length;
        off_t read_offset = mapping->offset + (off_t)old_allocation_length;
        while (remaining) {
            ssize_t count = pread(mapping->fd, cursor, remaining, read_offset);
            if (count < 0 && errno == EINTR) continue;
            if (count < 0) {
                int error = errno;
                free(resized_dirty_pages);
                free(resized);
                errno = error;
                return MAP_FAILED;
            }
            if (count == 0) break;
            cursor += count;
            remaining -= (size_t)count;
            read_offset += count;
        }
    }
    free(mapping->private_dirty_pages);
    free(old_address);
    mapping->address = resized;
    mapping->length = new_length;
    mapping->allocation_length = new_allocation_length;
    mapping->page_size = alignment;
    mapping->private_dirty_pages = resized_dirty_pages;
    return resized;
}

void *mremap(void *old_address, size_t old_length, size_t new_length, int flags,
             ...) {
    if (lock_mappings() != 0) return MAP_FAILED;
    void *result = mremap_unlocked(old_address, old_length, new_length, flags);
    unlock_mappings();
    return result;
}

static int mprotect_unlocked(void *address, size_t length, int prot) {
    struct mapping *mapping = find_mapping(address, length);
    if (!mapping) {
        errno = ENOMEM;
        return -1;
    }
    if (prot != PROT_READ && prot != (PROT_READ | PROT_WRITE)) {
        errno = ENOTSUP;
        return -1;
    }
    mapping->prot = prot;
    return 0;
}

int mprotect(void *address, size_t length, int prot) {
    if (lock_mappings() != 0) return -1;
    int result = mprotect_unlocked(address, length, prot);
    unlock_mappings();
    return result;
}
