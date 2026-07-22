#include <errno.h>
#include <fcntl.h>
#include <netdb.h>
#include <netinet/in.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#ifdef __wasm__
#include <wasi/api.h>
#endif

int h_errno;

#ifdef __wasm__
__attribute__((import_module("host_fs"), import_name("path_mknod")))
uint32_t agentos_path_mknod(uint32_t fd, const char *path, uint32_t path_len,
                            uint32_t mode, uint64_t device);
#endif

#define AT_FDCWD_SENTINEL UINT32_MAX
#define AGENTOS_GETDENTS_MAX_BYTES (1024U * 1024U)
#define AGENTOS_LINUX_DIRENT_HEADER_BYTES 19U

#ifdef __wasm__
static unsigned char agentos_linux_dirent_type(__wasi_filetype_t type) {
    switch (type) {
    case __WASI_FILETYPE_BLOCK_DEVICE:
        return 6;
    case __WASI_FILETYPE_CHARACTER_DEVICE:
        return 2;
    case __WASI_FILETYPE_DIRECTORY:
        return 4;
    case __WASI_FILETYPE_REGULAR_FILE:
        return 8;
    case __WASI_FILETYPE_SOCKET_DGRAM:
    case __WASI_FILETYPE_SOCKET_STREAM:
        return 12;
    case __WASI_FILETYPE_SYMBOLIC_LINK:
        return 10;
    default:
        return 0;
    }
}
#endif

int agentos_getdents64(int fd, void *buffer, size_t length) {
#ifdef __wasm__
    uint8_t *wasi_buffer;
    __wasi_size_t wasi_used = 0;
    __wasi_dircookie_t cookie;
    __wasi_dircookie_t next_cookie = 0;
    size_t input_offset = 0;
    size_t output_offset = 0;
    off_t current_offset;
    __wasi_errno_t error;

    if (buffer == NULL || length < AGENTOS_LINUX_DIRENT_HEADER_BYTES + 1 ||
        length > AGENTOS_GETDENTS_MAX_BYTES) {
        errno = EINVAL;
        return -1;
    }
    current_offset = lseek(fd, 0, SEEK_CUR);
    if (current_offset < 0)
        return -1;
    cookie = (__wasi_dircookie_t)current_offset;
    wasi_buffer = malloc(length);
    if (wasi_buffer == NULL) {
        errno = ENOMEM;
        return -1;
    }
    error = __wasi_fd_readdir((__wasi_fd_t)fd, wasi_buffer,
                              (__wasi_size_t)length, cookie, &wasi_used);
    if (error != __WASI_ERRNO_SUCCESS) {
        free(wasi_buffer);
        errno = (int)error;
        return -1;
    }

    while (input_offset + sizeof(__wasi_dirent_t) <= wasi_used) {
        __wasi_dirent_t wasi_entry;
        size_t input_record_length;
        size_t linux_record_length;
        uint16_t linux_reclen;
        uint8_t *linux_entry;

        memcpy(&wasi_entry, wasi_buffer + input_offset, sizeof(wasi_entry));
        input_record_length = sizeof(wasi_entry) + (size_t)wasi_entry.d_namlen;
        if (input_record_length > (size_t)wasi_used - input_offset)
            break;
        if ((size_t)wasi_entry.d_namlen >
            SIZE_MAX - AGENTOS_LINUX_DIRENT_HEADER_BYTES - 8) {
            free(wasi_buffer);
            errno = EOVERFLOW;
            return -1;
        }
        linux_record_length =
            (AGENTOS_LINUX_DIRENT_HEADER_BYTES +
             (size_t)wasi_entry.d_namlen + 1 + 7) & ~(size_t)7;
        if (linux_record_length > UINT16_MAX ||
            linux_record_length > length - output_offset)
            break;

        linux_entry = (uint8_t *)buffer + output_offset;
        memset(linux_entry, 0, linux_record_length);
        memcpy(linux_entry, &wasi_entry.d_ino, sizeof(wasi_entry.d_ino));
        memcpy(linux_entry + 8, &wasi_entry.d_next, sizeof(wasi_entry.d_next));
        linux_reclen = (uint16_t)linux_record_length;
        memcpy(linux_entry + 16, &linux_reclen, sizeof(linux_reclen));
        linux_entry[18] = agentos_linux_dirent_type(wasi_entry.d_type);
        memcpy(linux_entry + AGENTOS_LINUX_DIRENT_HEADER_BYTES,
               wasi_buffer + input_offset + sizeof(wasi_entry),
               (size_t)wasi_entry.d_namlen);

        output_offset += linux_record_length;
        input_offset += input_record_length;
        next_cookie = wasi_entry.d_next;
    }
    free(wasi_buffer);

    if (output_offset == 0 && wasi_used == length) {
        errno = EINVAL;
        return -1;
    }
    if (output_offset != 0 &&
        (next_cookie > INT64_MAX ||
         lseek(fd, (off_t)next_cookie, SEEK_SET) < 0))
        return -1;
    return (int)output_offset;
#else
    (void)fd;
    (void)buffer;
    (void)length;
    errno = ENOSYS;
    return -1;
#endif
}

struct hostent *agentos_gethostbyname(const char *name) {
    static struct hostent host;
    static struct in_addr address;
    static char *addresses[] = { (char *)&address, NULL };
    static char *aliases[] = { NULL };
    struct addrinfo hints = {0};
    struct addrinfo *result = NULL;
    int error;

    hints.ai_family = AF_INET;
    hints.ai_socktype = SOCK_STREAM;
    error = getaddrinfo(name, NULL, &hints, &result);
    if (error != 0 || result == NULL) {
        h_errno = error;
        return NULL;
    }
    address = ((struct sockaddr_in *)result->ai_addr)->sin_addr;
    freeaddrinfo(result);
    host.h_name = (char *)name;
    host.h_aliases = aliases;
    host.h_addrtype = AF_INET;
    host.h_length = sizeof(address);
    host.h_addr_list = addresses;
    return &host;
}

char *agentos_strsignal(int signal_number) {
    static char description[32];

    switch (signal_number) {
    case SIGIO:
        return "I/O possible";
    case SIGTERM:
        return "Terminated";
    case SIGKILL:
        return "Killed";
    default:
        snprintf(description, sizeof(description), "signal %d", signal_number);
        return description;
    }
}

static int agentos_host_mknod(uint32_t dirfd, const char *path, mode_t mode,
                             dev_t device) {
#ifdef __wasm__
    uint32_t error = agentos_path_mknod(dirfd, path, (uint32_t)strlen(path),
                                        (uint32_t)mode, (uint64_t)device);
    if (error != 0) {
        errno = (int)error;
        return -1;
    }
    return 0;
#else
    (void)dirfd;
    (void)path;
    (void)mode;
    (void)device;
    errno = ENOSYS;
    return -1;
#endif
}

int agentos_mknod(const char *path, mode_t mode, dev_t device) {
    int cwd_fd = open(".", O_RDONLY | O_DIRECTORY);
    int result;
    int saved_errno;

    if (cwd_fd < 0)
        return -1;
    result = agentos_host_mknod((uint32_t)cwd_fd, path, mode, device);
    saved_errno = errno;
    close(cwd_fd);
    errno = saved_errno;
    return result;
}

int agentos_mknodat(int dirfd, const char *path, mode_t mode, dev_t device) {
    int fd;

    if ((mode & S_IFMT) != S_IFREG || device != 0) {
        if (dirfd == AT_FDCWD)
            return agentos_mknod(path, mode, device);
        return agentos_host_mknod((uint32_t)dirfd, path, mode, device);
    }
    fd = openat(dirfd, path, O_CREAT | O_EXCL | O_WRONLY, mode & 07777);
    if (fd < 0)
        return -1;
    return close(fd);
}

static void print_message(const char *format, va_list args, int include_errno) {
    if (format) {
        vfprintf(stderr, format, args);
        if (include_errno) fputs(": ", stderr);
    }
    if (include_errno) fputs(strerror(errno), stderr);
    fputc('\n', stderr);
}

__attribute__((weak)) void vwarn(const char *format, va_list args) {
    print_message(format, args, 1);
}
__attribute__((weak)) void vwarnx(const char *format, va_list args) {
    print_message(format, args, 0);
}

__attribute__((weak)) void warn(const char *format, ...) {
    va_list args;
    va_start(args, format);
    vwarn(format, args);
    va_end(args);
}

__attribute__((weak)) void warnx(const char *format, ...) {
    va_list args;
    va_start(args, format);
    vwarnx(format, args);
    va_end(args);
}

__attribute__((weak)) void verr(int status, const char *format, va_list args) {
    vwarn(format, args);
    exit(status);
}

__attribute__((weak)) void verrx(int status, const char *format, va_list args) {
    vwarnx(format, args);
    exit(status);
}

__attribute__((weak)) void err(int status, const char *format, ...) {
    va_list args;
    va_start(args, format);
    verr(status, format, args);
}

__attribute__((weak)) void errx(int status, const char *format, ...) {
    va_list args;
    va_start(args, format);
    verrx(status, format, args);
}
