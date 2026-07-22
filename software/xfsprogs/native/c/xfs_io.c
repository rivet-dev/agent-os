#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#define MAX_COMMANDS 128
#define IO_CHUNK (1024 * 1024)
#define DIRECT_IO_ALIGNMENT 512

#ifdef __wasm__
__attribute__((import_module("host_fs"), import_name("fd_fiemap")))
uint32_t agentos_fd_fiemap(uint32_t fd, uint32_t index, uint64_t *start, uint64_t *end,
                          uint32_t *flags);
#endif

struct options {
    const char *commands[MAX_COMMANDS];
    size_t command_count;
    const char *path;
    int create;
    int read_only;
    int direct;
    int append;
    int truncate;
    int sync;
    int tmpfile;
    mode_t mode;
};

struct mapping_state {
    unsigned char *address;
    uint64_t offset;
    uint64_t length;
};

static int parse_size(const char *text, uint64_t *value) {
    if (!text || !*text || *text == '-') return -1;
    char *end = NULL;
    errno = 0;
    unsigned long long number = strtoull(text, &end, 0);
    if (errno || end == text) return -1;
    uint64_t multiplier = 1;
    if (*end) {
        if (!strcasecmp(end, "b")) multiplier = 512;
        else if (!strcasecmp(end, "k") || !strcasecmp(end, "kb") || !strcasecmp(end, "kib")) multiplier = 1024;
        else if (!strcasecmp(end, "m") || !strcasecmp(end, "mb") || !strcasecmp(end, "mib")) multiplier = 1024 * 1024;
        else if (!strcasecmp(end, "g") || !strcasecmp(end, "gb") || !strcasecmp(end, "gib")) multiplier = 1024ull * 1024 * 1024;
        else return -1;
    }
    if (number > UINT64_MAX / multiplier) return -1;
    *value = (uint64_t)number * multiplier;
    return 0;
}

static int split_words(char *command, char **words, int capacity) {
    int count = 0;
    char *cursor = command;
    while (*cursor) {
        while (*cursor == ' ' || *cursor == '\t') cursor++;
        if (!*cursor) break;
        if (count == capacity) return -1;
        words[count++] = cursor;
        while (*cursor && *cursor != ' ' && *cursor != '\t') cursor++;
        if (*cursor) *cursor++ = 0;
    }
    return count;
}

static int parse_pattern(const char *text, unsigned char *pattern) {
    char *end = NULL;
    errno = 0;
    unsigned long value = strtoul(text, &end, 0);
    if (!text || !*text || !end || *end || errno == ERANGE) return -1;
    *pattern = (unsigned char)value;
    return 0;
}

static int command_pwrite(int fd, int argc, char **argv, int direct) {
    unsigned char pattern = 0xcd;
    const char *input_path = NULL;
    uint64_t block_size = IO_CHUNK;
    int quiet = 0;
    int index = 1;
    while (index < argc && argv[index][0] == '-') {
        if ((!strcmp(argv[index], "-S") || !strcmp(argv[index], "-b")) && index + 1 < argc) {
            if (!strcmp(argv[index], "-S") && parse_pattern(argv[index + 1], &pattern) != 0) {
                fprintf(stderr, "pwrite: invalid pattern\n");
                return 1;
            }
            if (!strcmp(argv[index], "-b") &&
                (parse_size(argv[index + 1], &block_size) != 0 || block_size == 0 ||
                 block_size > SIZE_MAX)) {
                fprintf(stderr, "pwrite: invalid block size\n");
                return 1;
            }
            index += 2;
        } else if (!strcmp(argv[index], "-i") && index + 1 < argc) {
            input_path = argv[index + 1];
            index += 2;
        } else if (!strcmp(argv[index], "-q")) {
            quiet = 1;
            index++;
        } else if ((!strcmp(argv[index], "-V") || !strcmp(argv[index], "-F")) && index + 1 < argc) {
            index += 2;
        } else if (!strcmp(argv[index], "-W")) {
            index++;
        } else {
            fprintf(stderr, "pwrite: Operation not supported\n");
            return 1;
        }
    }
    if (index + 2 != argc && index + 3 != argc) {
        fprintf(stderr, "pwrite: bad argument count\n");
        return 1;
    }
    uint64_t offset, length;
    if (parse_size(argv[index], &offset) != 0 || parse_size(argv[index + 1], &length) != 0 ||
        offset > INT64_MAX || length > INT64_MAX) {
        fprintf(stderr, "pwrite: Invalid argument\n");
        return 1;
    }
    if (index + 3 == argc &&
        (parse_size(argv[index + 2], &block_size) != 0 || block_size == 0 ||
         block_size > SIZE_MAX)) {
        fprintf(stderr, "pwrite: invalid block size\n");
        return 1;
    }
    size_t capacity = length < block_size ? (size_t)length : (size_t)block_size;
    if (capacity == 0) capacity = 1;
    unsigned char *buffer = NULL;
    if (direct) {
        if (posix_memalign((void **)&buffer, DIRECT_IO_ALIGNMENT, capacity) != 0)
            buffer = NULL;
    } else {
        buffer = malloc(capacity);
    }
    if (!buffer) { perror("pwrite"); return 1; }
    int input_fd = -1;
    if (input_path) {
        input_fd = open(input_path, O_RDONLY);
        if (input_fd < 0) {
            perror(input_path);
            free(buffer);
            return 1;
        }
    } else {
        memset(buffer, pattern, capacity);
    }
    uint64_t written = 0;
    while (written < length) {
        size_t chunk = (size_t)((length - written) < capacity ? (length - written) : capacity);
        if (input_fd >= 0) {
            ssize_t read_result = pread(input_fd, buffer, chunk, (off_t)written);
            if (read_result < 0) {
                perror("pwrite input");
                close(input_fd);
                free(buffer);
                return 1;
            }
            if (read_result == 0) break;
            chunk = (size_t)read_result;
        }
        ssize_t result = pwrite(fd, buffer, chunk, (off_t)(offset + written));
        if (result <= 0) {
            perror("pwrite");
            if (input_fd >= 0) close(input_fd);
            free(buffer);
            return 1;
        }
        written += (uint64_t)result;
    }
    if (input_fd >= 0 && close(input_fd) != 0) {
        perror("pwrite input close");
        free(buffer);
        return 1;
    }
    free(buffer);
    if (!quiet) {
        printf("wrote %" PRIu64 "/%" PRIu64 " bytes at offset %" PRIu64 "\n", written, length, offset);
        printf("%" PRIu64 " bytes, 1 ops; 0.0000 sec (%" PRIu64 " bytes/sec and 1 ops/sec)\n",
               written, written);
    }
    return 0;
}

static void print_dump_row(uint64_t address, const unsigned char *row, size_t length) {
    printf("%08" PRIx64 ": ", address);
    for (size_t column = 0; column < length; column++) printf(" %02x", row[column]);
    printf("  ");
    for (size_t column = 0; column < length; column++) {
        unsigned char byte = row[column];
        putchar(byte > 0x20 && byte <= 0x7e ? byte : '.');
    }
    putchar('\n');
}

static int command_pread(int fd, int argc, char **argv, int direct) {
    int quiet = 0;
    int verbose = 0;
    int index = 1;
    while (index < argc && argv[index][0] == '-') {
        if (!strcmp(argv[index], "-q")) quiet = 1;
        else if (!strcmp(argv[index], "-v")) verbose = 1;
        else if ((!strcmp(argv[index], "-b") || !strcmp(argv[index], "-V")) && index + 1 < argc) index++;
        else if (strcmp(argv[index], "-F")) {
            fprintf(stderr, "pread: Operation not supported\n");
            return 1;
        }
        index++;
    }
    if (index + 2 != argc) { fprintf(stderr, "pread: bad argument count\n"); return 1; }
    uint64_t offset, length;
    if (parse_size(argv[index], &offset) != 0 || parse_size(argv[index + 1], &length) != 0) {
        fprintf(stderr, "pread: Invalid argument\n");
        return 1;
    }
    unsigned char *buffer = NULL;
    if (posix_memalign((void **)&buffer, DIRECT_IO_ALIGNMENT, 4096) != 0) {
        perror("pread");
        return 1;
    }
    uint64_t read_bytes = 0;
    unsigned char previous_row[16];
    size_t previous_length = 0;
    int have_previous = 0;
    int repeated = 0;
    while (read_bytes < length) {
        size_t chunk = (size_t)((length - read_bytes) < 4096 ? (length - read_bytes) : 4096);
        ssize_t result = pread(fd, buffer, chunk, (off_t)(offset + read_bytes));
        if (result < 0) { perror("pread"); free(buffer); return 1; }
        if (result == 0) break;
        if (verbose) {
            for (size_t row = 0; row < (size_t)result; row += 16) {
                size_t row_length = (size_t)result - row;
                if (row_length > 16) row_length = 16;
                if (have_previous && previous_length == row_length &&
                    memcmp(previous_row, buffer + row, row_length) == 0) {
                    if (!repeated) {
                        puts("*");
                        repeated = 1;
                    }
                } else {
                    print_dump_row(offset + read_bytes + row, buffer + row, row_length);
                    memcpy(previous_row, buffer + row, row_length);
                    previous_length = row_length;
                    have_previous = 1;
                    repeated = 0;
                }
            }
        }
        read_bytes += (uint64_t)result;
    }
    free(buffer);
    if (!quiet) {
        printf("read %" PRIu64 "/%" PRIu64 " bytes at offset %" PRIu64 "\n", read_bytes, length, offset);
        printf("%" PRIu64 " bytes, 1 ops; 0.0000 sec (%" PRIu64 " bytes/sec and 1 ops/sec)\n",
               read_bytes, read_bytes);
    }
    return 0;
}

static int command_sendfile(int output_fd, int argc, char **argv) {
    const char *input_path = NULL;
    int quiet = 0;
    int index = 1;
    while (index < argc && argv[index][0] == '-') {
        if (!strcmp(argv[index], "-i") && index + 1 < argc) {
            input_path = argv[index + 1];
            index += 2;
        } else if (!strcmp(argv[index], "-q")) {
            quiet = 1;
            index++;
        } else {
            fprintf(stderr, "sendfile: Operation not supported\n");
            return 1;
        }
    }
    uint64_t input_offset, length;
    if (!input_path || index + 2 != argc ||
        parse_size(argv[index], &input_offset) != 0 ||
        parse_size(argv[index + 1], &length) != 0 ||
        input_offset > INT64_MAX || length > INT64_MAX) {
        fprintf(stderr, "sendfile: Invalid argument\n");
        return 1;
    }

    int input_fd = open(input_path, O_RDONLY);
    if (input_fd < 0) {
        perror("sendfile");
        return 1;
    }
    size_t capacity = length < IO_CHUNK ? (size_t)length : IO_CHUNK;
    if (capacity == 0) capacity = 1;
    unsigned char *buffer = malloc(capacity);
    if (!buffer) {
        perror("sendfile");
        close(input_fd);
        return 1;
    }

    uint64_t copied = 0;
    while (copied < length) {
        size_t requested = (size_t)((length - copied) < capacity ?
                                    (length - copied) : capacity);
        ssize_t read_result;
        do {
            read_result = pread(input_fd, buffer, requested,
                                (off_t)(input_offset + copied));
        } while (read_result < 0 && errno == EINTR);
        if (read_result < 0) {
            perror("sendfile");
            free(buffer);
            close(input_fd);
            return 1;
        }
        if (read_result == 0) break;

        size_t written = 0;
        while (written < (size_t)read_result) {
            ssize_t write_result;
            do {
                write_result = write(output_fd, buffer + written,
                                     (size_t)read_result - written);
            } while (write_result < 0 && errno == EINTR);
            if (write_result <= 0) {
                if (write_result == 0) errno = EIO;
                perror("sendfile");
                free(buffer);
                close(input_fd);
                return 1;
            }
            written += (size_t)write_result;
        }
        copied += (uint64_t)read_result;
    }

    free(buffer);
    if (close(input_fd) != 0) {
        perror("sendfile");
        return 1;
    }
    if (!quiet) {
        printf("sent %" PRIu64 "/%" PRIu64 " bytes at offset %" PRIu64 "\n",
               copied, length, input_offset);
    }
    return 0;
}

static int command_truncate(int fd, int argc, char **argv, struct mapping_state *mapping) {
    if (argc != 2) { fprintf(stderr, "truncate: bad argument count\n"); return 1; }
    uint64_t length;
    if (parse_size(argv[1], &length) != 0 || length > INT64_MAX) {
        fprintf(stderr, "truncate: Invalid argument\n");
        return 1;
    }
    struct stat before;
    if (fstat(fd, &before) != 0) { perror("fstat"); return 1; }
    if (ftruncate(fd, (off_t)length) != 0) { perror("ftruncate"); return 1; }
    if (mapping->address && length < (uint64_t)before.st_size &&
        length < mapping->offset + mapping->length) {
        uint64_t zero_from = length > mapping->offset ? length - mapping->offset : 0;
        memset(mapping->address + (size_t)zero_from, 0,
               (size_t)(mapping->length - zero_from));
    }
    return 0;
}

static int parse_range_command(const char *command, int argc, char **argv,
                               uint64_t *offset, uint64_t *length);

static int command_fpunch(int fd, int argc, char **argv) {
    if (argc != 3) { fprintf(stderr, "fpunch: bad argument count\n"); return 1; }
    uint64_t offset, length;
    if (parse_size(argv[1], &offset) != 0 || parse_size(argv[2], &length) != 0 ||
        offset > INT64_MAX || length > INT64_MAX || offset > UINT64_MAX - length) {
        fprintf(stderr, "fpunch: Invalid argument\n");
        return 1;
    }
    uint32_t error = fallocate(fd, 0x01 | 0x02, (off_t)offset, (off_t)length) == 0
        ? 0
        : (uint32_t)errno;
    if (error != 0) {
        errno = (int)error;
        perror("fpunch");
        return 1;
    }
    return 0;
}

static int command_fzero(int fd, int argc, char **argv) {
    int keep_size = 0;
    int index = 1;
    if (index < argc && !strcmp(argv[index], "-k")) {
        keep_size = 1;
        index++;
    }
    if (index + 2 != argc) {
        fprintf(stderr, "fzero: Invalid argument\n");
        return 1;
    }
    uint64_t offset, length;
    char *range[] = {argv[0], argv[index], argv[index + 1]};
    if (parse_range_command("fzero", 3, range, &offset, &length) != 0) return 1;
    int mode = 0x10 | (keep_size ? 0x01 : 0);
    uint32_t error = fallocate(fd, mode, (off_t)offset, (off_t)length) == 0
        ? 0
        : (uint32_t)errno;
    if (error != 0) {
        errno = (int)error;
        perror("fzero");
        return 1;
    }
    return 0;
}

static int parse_range_command(const char *command, int argc, char **argv,
                               uint64_t *offset, uint64_t *length) {
    if (argc != 3 || parse_size(argv[1], offset) != 0 ||
        parse_size(argv[2], length) != 0 || *offset > INT64_MAX ||
        *length > INT64_MAX || *offset > UINT64_MAX - *length) {
        fprintf(stderr, "%s: Invalid argument\n", command);
        return -1;
    }
    return 0;
}

static int command_falloc(int fd, int argc, char **argv) {
    int index = 1;
    int keep_size = 0;
    if (index < argc && !strcmp(argv[index], "-k")) {
        keep_size = 1;
        index++;
    }
    if (index + 2 != argc) {
        fprintf(stderr, "falloc: Invalid argument\n");
        return 1;
    }
    uint64_t offset, length;
    char *range[] = {argv[0], argv[index], argv[index + 1]};
    if (parse_range_command("falloc", 3, range, &offset, &length) != 0) return 1;
    int mode = keep_size ? 0x01 : 0;
    if (fallocate(fd, mode, (off_t)offset, (off_t)length) != 0) {
        perror("fallocate");
        return 1;
    }
    return 0;
}

static int command_shift_range(int fd, int argc, char **argv, int insert) {
    const char *command = insert ? "finsert" : "fcollapse";
    uint64_t offset, length;
    if (parse_range_command(command, argc, argv, &offset, &length) != 0) return 1;
    int mode = insert ? 0x20 : 0x08;
    uint32_t error = fallocate(fd, mode, (off_t)offset, (off_t)length) == 0
        ? 0
        : (uint32_t)errno;
    if (error != 0) {
        errno = (int)error;
        // Upstream xfs_io reports the underlying syscall name for both
        // finsert and fcollapse failures.
        perror("fallocate");
        return 1;
    }
    return 0;
}

static void print_fiemap_extent(uint32_t *output_index, uint64_t start, uint64_t end,
                                int hole, uint32_t flags) {
    if (start >= end) return;
    uint64_t first_sector = start / 512;
    uint64_t last_sector = (end - 1) / 512;
    if (hole) {
        printf("%u: [%" PRIu64 "..%" PRIu64 "]: hole\n",
               (*output_index)++, first_sector, last_sector);
    } else {
        printf("%u: [%" PRIu64 "..%" PRIu64 "]: %" PRIu64 "..%" PRIu64
               " %" PRIu64 " 0x%03" PRIx32 "\n",
               (*output_index)++, first_sector, last_sector, first_sector, last_sector,
               last_sector - first_sector + 1, flags);
    }
}

static int command_fiemap(int fd, int argc, char **argv) {
    if (argc != 1 && (argc != 2 || strcmp(argv[1], "-v"))) {
        fprintf(stderr, "fiemap: Operation not supported\n");
        return 1;
    }
    struct stat statbuf;
    if (fstat(fd, &statbuf) != 0) { perror("fiemap"); return 1; }
    uint64_t size = (uint64_t)statbuf.st_size;
    uint64_t cursor = 0;
    uint32_t output_index = 0;
#ifdef __wasm__
    for (uint32_t index = 0;; index++) {
        uint64_t start = 0, end = 0;
        uint32_t flags = 0;
        uint32_t error = agentos_fd_fiemap((uint32_t)fd, index, &start, &end, &flags);
        if (error == ENODATA) break;
        if (error != 0) { errno = (int)error; perror("fiemap"); return 1; }
        if (start > cursor) print_fiemap_extent(&output_index, cursor, start, 1, 0);
        print_fiemap_extent(&output_index, start, end, 0, flags);
        if (end > cursor) cursor = end;
    }
#else
    while (cursor < size) {
        off_t data = lseek(fd, (off_t)cursor, SEEK_DATA);
        if (data < 0 && errno == ENXIO) break;
        if (data < 0) { perror("fiemap"); return 1; }
        off_t hole = lseek(fd, data, SEEK_HOLE);
        if (hole < 0) { perror("fiemap"); return 1; }
        if ((uint64_t)data > cursor)
            print_fiemap_extent(&output_index, cursor, (uint64_t)data, 1, 0);
        print_fiemap_extent(&output_index, (uint64_t)data, (uint64_t)hole, 0, 0);
        cursor = (uint64_t)hole;
    }
#endif
    if (output_index > 0 && cursor < size)
        print_fiemap_extent(&output_index, cursor, size, 1, 0);
    return 0;
}

static int command_fadvise(int fd, int argc, char **argv) {
    if ((argc != 2 && argc != 4) || strlen(argv[1]) != 2 || argv[1][0] != '-') {
        fprintf(stderr, "fadvise: Invalid argument\n");
        return 1;
    }

    int advice;
    switch (argv[1][1]) {
        case 'd': advice = POSIX_FADV_DONTNEED; break;
        case 'n': advice = POSIX_FADV_NOREUSE; break;
        case 'r': advice = POSIX_FADV_RANDOM; break;
        case 's': advice = POSIX_FADV_SEQUENTIAL; break;
        case 'w': advice = POSIX_FADV_WILLNEED; break;
        default:
            fprintf(stderr, "fadvise: Invalid argument\n");
            return 1;
    }

    uint64_t offset = 0;
    uint64_t length = 0;
    if (argc == 4 &&
        (parse_size(argv[2], &offset) != 0 || parse_size(argv[3], &length) != 0)) {
        fprintf(stderr, "fadvise: Invalid argument\n");
        return 1;
    }
#ifdef __wasm__
    // agentOS VFS reads are authoritative and do not retain a guest-visible page cache.
    (void)fd;
    (void)offset;
    (void)length;
    (void)advice;
    return 0;
#else
    int error = posix_fadvise(fd, (off_t)offset, (off_t)length, advice);
    if (error != 0) {
        errno = error;
        perror("fadvise");
        return 1;
    }
    return 0;
#endif
}

static int command_utimes(const char *path, int argc, char **argv) {
    if (argc != 5) { fprintf(stderr, "utimes: bad argument count\n"); return 1; }
    uint64_t atime_sec, atime_nsec, mtime_sec, mtime_nsec;
    if (parse_size(argv[1], &atime_sec) || parse_size(argv[2], &atime_nsec) ||
        parse_size(argv[3], &mtime_sec) || parse_size(argv[4], &mtime_nsec) ||
        atime_nsec >= 1000000000 || mtime_nsec >= 1000000000) {
        fprintf(stderr, "utimes: Invalid argument\n");
        return 1;
    }
    struct timespec times[2] = {
        {(time_t)atime_sec, (long)atime_nsec},
        {(time_t)mtime_sec, (long)mtime_nsec},
    };
    if (utimensat(AT_FDCWD, path, times, 0) != 0) { perror("utimes"); return 1; }
    return 0;
}

static int command_flink(int fd, int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "flink: bad argument count\n");
        return 1;
    }
    if (linkat(fd, "", AT_FDCWD, argv[1], AT_EMPTY_PATH) != 0) {
        perror("flink");
        return 1;
    }
    return 0;
}

static int command_stat(const char *path) {
    struct stat st;
    if (stat(path, &st) != 0) { perror("stat"); return 1; }
    printf("fd.path = \"%s\"\n", path);
    printf("stat.ino = %" PRIu64 "\n", (uint64_t)st.st_ino);
    printf("stat.size = %" PRIu64 "\n", (uint64_t)st.st_size);
    printf("stat.blocks = %" PRIu64 "\n", (uint64_t)st.st_blocks);
    return 0;
}

static int command_mmap(int fd, int argc, char **argv, struct mapping_state *mapping) {
    int prot = 0;
    int index = 1;
    while (index < argc && argv[index][0] == '-') {
        for (const char *flag = argv[index] + 1; *flag; flag++) {
            if (*flag == 'r') prot |= PROT_READ;
            else if (*flag == 'w') prot |= PROT_WRITE;
            else {
                fprintf(stderr, "mmap: Operation not supported\n");
                return 1;
            }
        }
        index++;
    }
    if (index + 2 != argc) {
        fprintf(stderr, "mmap: bad argument count\n");
        return 1;
    }
    uint64_t offset, length;
    if (parse_size(argv[index], &offset) != 0 ||
        parse_size(argv[index + 1], &length) != 0 || length == 0 ||
        offset > INT64_MAX || length > SIZE_MAX || offset > UINT64_MAX - length) {
        fprintf(stderr, "mmap: Invalid argument\n");
        return 1;
    }
    if (!prot) prot = PROT_READ | PROT_WRITE;
    if (mapping->address && munmap(mapping->address, (size_t)mapping->length) != 0) {
        perror("munmap");
        return 1;
    }
    void *address = mmap(NULL, (size_t)length, prot, MAP_SHARED, fd, (off_t)offset);
    if (address == MAP_FAILED) {
        mapping->address = NULL;
        mapping->length = 0;
        perror("mmap");
        return 1;
    }
    mapping->address = address;
    mapping->offset = offset;
    mapping->length = length;
    return 0;
}

static int command_mread(int argc, char **argv, const struct mapping_state *mapping) {
    int reverse = 0;
    int index = 1;
    while (index < argc && argv[index][0] == '-') {
        if (!strcmp(argv[index], "-r")) reverse = 1;
        else {
            fprintf(stderr, "mread: Operation not supported\n");
            return 1;
        }
        index++;
    }
    if (!mapping->address) {
        fprintf(stderr, "mread: no mapped regions\n");
        return 1;
    }
    uint64_t offset = mapping->offset;
    uint64_t length = mapping->length;
    if (index != argc) {
        if (index + 2 != argc || parse_size(argv[index], &offset) != 0 ||
            parse_size(argv[index + 1], &length) != 0) {
            fprintf(stderr, "mread: Invalid argument\n");
            return 1;
        }
    }
    if (offset < mapping->offset || length > mapping->length ||
        offset - mapping->offset > mapping->length - length) {
        fprintf(stderr, "mread: range is not within the current mapping\n");
        return 1;
    }
    size_t start = (size_t)(offset - mapping->offset);
    volatile unsigned char sink = 0;
    if (reverse) {
        for (size_t i = (size_t)length; i > 0; i--) sink ^= mapping->address[start + i - 1];
    } else {
        for (size_t i = 0; i < (size_t)length; i++) sink ^= mapping->address[start + i];
    }
    (void)sink;
    return 0;
}

static int mapped_range(const char *command, const struct mapping_state *mapping,
                        uint64_t offset, uint64_t length, size_t *start) {
    if (!mapping->address) {
        fprintf(stderr, "%s: no mapped regions\n", command);
        return -1;
    }
    if (offset < mapping->offset || length > mapping->length ||
        offset - mapping->offset > mapping->length - length) {
        fprintf(stderr, "%s: range is not within the current mapping\n", command);
        return -1;
    }
    *start = (size_t)(offset - mapping->offset);
    return 0;
}

static int command_mwrite(int argc, char **argv, const struct mapping_state *mapping) {
    unsigned char pattern = 0xcd;
    int index = 1;
    while (index < argc && argv[index][0] == '-') {
        if (!strcmp(argv[index], "-S") && index + 1 < argc) {
            if (parse_pattern(argv[index + 1], &pattern) != 0) {
                fprintf(stderr, "mwrite: invalid pattern\n");
                return 1;
            }
            index += 2;
        } else {
            fprintf(stderr, "mwrite: Operation not supported\n");
            return 1;
        }
    }
    uint64_t offset, length;
    size_t start;
    if (index + 2 != argc || parse_size(argv[index], &offset) != 0 ||
        parse_size(argv[index + 1], &length) != 0 || length > SIZE_MAX ||
        mapped_range("mwrite", mapping, offset, length, &start) != 0)
        return 1;
    memset(mapping->address + start, pattern, (size_t)length);
    return 0;
}

static int command_mremap(int argc, char **argv, struct mapping_state *mapping) {
    int flags = 0;
    int index = 1;
    if (index < argc && !strcmp(argv[index], "-m")) {
        flags = MREMAP_MAYMOVE;
        index++;
    }
    uint64_t length;
    if (index + 1 != argc || parse_size(argv[index], &length) != 0 ||
        length == 0 || length > SIZE_MAX) {
        fprintf(stderr, "mremap: Invalid argument\n");
        return 1;
    }
    if (!mapping->address) {
        fprintf(stderr, "mremap: no mapped regions\n");
        return 1;
    }
    void *address = mremap(mapping->address, (size_t)mapping->length,
                           (size_t)length, flags);
    if (address == MAP_FAILED) { perror("mremap"); return 1; }
    mapping->address = address;
    mapping->length = length;
    return 0;
}

static int command_msync(int argc, char **argv, const struct mapping_state *mapping) {
    int flags = MS_ASYNC;
    int index = 1;
    if (index < argc && !strcmp(argv[index], "-s")) {
        flags = MS_SYNC;
        index++;
    }
    uint64_t offset = mapping->offset;
    uint64_t length = mapping->length;
    size_t start;
    if (index != argc &&
        (index + 2 != argc || parse_size(argv[index], &offset) != 0 ||
         parse_size(argv[index + 1], &length) != 0)) {
        fprintf(stderr, "msync: Invalid argument\n");
        return 1;
    }
    if (mapped_range("msync", mapping, offset, length, &start) != 0) return 1;
    if (msync(mapping->address + start, (size_t)length, flags) != 0) {
        perror("msync");
        return 1;
    }
    return 0;
}

static int command_munmap(int argc, struct mapping_state *mapping) {
    if (argc != 1) { fprintf(stderr, "munmap: bad argument count\n"); return 1; }
    if (!mapping->address) {
        fprintf(stderr, "munmap: no mapped regions\n");
        return 1;
    }
    if (munmap(mapping->address, (size_t)mapping->length) != 0) {
        perror("munmap");
        return 1;
    }
    memset(mapping, 0, sizeof(*mapping));
    return 0;
}

static int print_help(const char *command) {
    if (!strcmp(command, "pwrite")) puts(" pwrite [-q] [-S pattern] [-i infile] offset len -- writes a range");
    else if (!strcmp(command, "pread")) puts(" pread [-q] offset len -- reads a range");
    else if (!strcmp(command, "sendfile")) puts(" sendfile -i infile [-q] offset len -- copies a file range");
    else if (!strcmp(command, "truncate")) puts(" truncate size -- changes file size");
    else if (!strcmp(command, "falloc")) puts(" falloc [-k] offset len -- allocates a range");
    else if (!strcmp(command, "fpunch")) puts(" fpunch offset len -- deallocates a range");
    else if (!strcmp(command, "fzero")) puts(" fzero [-k] offset len -- zeroes and allocates a range");
    else if (!strcmp(command, "finsert")) puts(" finsert offset len -- inserts a range");
    else if (!strcmp(command, "fcollapse")) puts(" fcollapse offset len -- collapses a range");
    else if (!strcmp(command, "fiemap")) puts(" fiemap [-v] -- prints file extents");
    else if (!strcmp(command, "fadvise")) puts(" fadvise [-d|-n|-r|-s|-w] [offset len] -- advises file access");
    else if (!strcmp(command, "flink")) puts(" flink path -- links the open file descriptor");
    else if (!strcmp(command, "utimes")) puts(" utimes atime_sec atime_nsec mtime_sec mtime_nsec -- changes timestamps");
    else if (!strcmp(command, "fsync") || !strcmp(command, "s")) puts(" fsync -- flushes file data and metadata");
    else if (!strcmp(command, "fdatasync")) puts(" fdatasync -- flushes file data");
    else if (!strcmp(command, "syncfs")) puts(" syncfs -- flushes the containing filesystem");
    else if (!strcmp(command, "stat")) puts(" stat -- prints file metadata");
    else if (!strcmp(command, "mmap")) puts(" mmap [-rw] offset len -- maps a file range");
    else if (!strcmp(command, "mread")) puts(" mread [-r] [offset len] -- reads a mapped range");
    else if (!strcmp(command, "mwrite")) puts(" mwrite [-S pattern] offset len -- writes a mapped range");
    else if (!strcmp(command, "mremap")) puts(" mremap [-m] len -- resizes the current mapping");
    else if (!strcmp(command, "msync")) puts(" msync [-s] [offset len] -- flushes a mapped range");
    else if (!strcmp(command, "munmap")) puts(" munmap -- unmaps the current mapping");
    else if (!strcmp(command, "close")) puts(" close -- closes the current file");
    else { fprintf(stderr, "%s: command not found\n", command); return 1; }
    return 0;
}

static int execute_command(int *fd, const char *path, const char *text, int direct,
                           struct mapping_state *mapping) {
    char *copy = strdup(text);
    if (!copy) return 1;
    char *words[32];
    int count = split_words(copy, words, 32);
    if (count <= 0) { free(copy); return count < 0; }
    int status;
    if (!strcmp(words[0], "close")) {
        if (count != 1) status = 1;
        else if (*fd < 0) status = 0;
        else if (close(*fd) != 0) status = (perror("close"), 1);
        else { *fd = -1; status = 0; }
    }
    else if (!strcmp(words[0], "help")) status = count == 2 ? print_help(words[1]) : 1;
    else if (*fd < 0) { fprintf(stderr, "%s: file is closed\n", words[0]); status = 1; }
    else if (!strcmp(words[0], "pwrite")) status = command_pwrite(*fd, count, words, direct);
    else if (!strcmp(words[0], "pread")) status = command_pread(*fd, count, words, direct);
    else if (!strcmp(words[0], "sendfile")) status = command_sendfile(*fd, count, words);
    else if (!strcmp(words[0], "truncate"))
        status = command_truncate(*fd, count, words, mapping);
    else if (!strcmp(words[0], "falloc")) status = command_falloc(*fd, count, words);
    else if (!strcmp(words[0], "fpunch")) status = command_fpunch(*fd, count, words);
    else if (!strcmp(words[0], "fzero")) status = command_fzero(*fd, count, words);
    else if (!strcmp(words[0], "finsert"))
        status = command_shift_range(*fd, count, words, 1);
    else if (!strcmp(words[0], "fcollapse"))
        status = command_shift_range(*fd, count, words, 0);
    else if (!strcmp(words[0], "fiemap")) status = command_fiemap(*fd, count, words);
    else if (!strcmp(words[0], "fadvise")) status = command_fadvise(*fd, count, words);
    else if (!strcmp(words[0], "flink")) status = command_flink(*fd, count, words);
    else if (!strcmp(words[0], "fsync") || !strcmp(words[0], "s")) status = fsync(*fd) != 0 ? (perror("fsync"), 1) : 0;
    else if (!strcmp(words[0], "fdatasync")) status = fdatasync(*fd) != 0 ? (perror("fdatasync"), 1) : 0;
    else if (!strcmp(words[0], "syncfs")) status = syncfs(*fd) != 0 ? (perror("syncfs"), 1) : 0;
    else if (!strcmp(words[0], "utimes")) status = command_utimes(path, count, words);
    else if (!strcmp(words[0], "stat") || !strcmp(words[0], "statx")) status = command_stat(path);
    else if (!strcmp(words[0], "mmap") || !strcmp(words[0], "mm"))
        status = command_mmap(*fd, count, words, mapping);
    else if (!strcmp(words[0], "mread") || !strcmp(words[0], "mr"))
        status = command_mread(count, words, mapping);
    else if (!strcmp(words[0], "mwrite") || !strcmp(words[0], "mw"))
        status = command_mwrite(count, words, mapping);
    else if (!strcmp(words[0], "mremap")) status = command_mremap(count, words, mapping);
    else if (!strcmp(words[0], "msync") || !strcmp(words[0], "ms"))
        status = command_msync(count, words, mapping);
    else if (!strcmp(words[0], "munmap") || !strcmp(words[0], "mu"))
        status = command_munmap(count, mapping);
    else if (!strcmp(words[0], "quit")) status = 0;
    else { fprintf(stderr, "%s: command not found\n", words[0]); status = 1; }
    free(copy);
    return status;
}

static int parse_options(int argc, char **argv, struct options *options) {
    for (int i = 1; i < argc; i++) {
        if (argv[i][0] != '-' || argv[i][1] == '\0') {
            options->path = argv[i];
            continue;
        }

        const char *option = argv[i];
        for (size_t flag_index = 1; option[flag_index] != '\0'; flag_index++) {
            switch (option[flag_index]) {
            case 'c':
            case 'C': {
                if (options->command_count == MAX_COMMANDS) return -1;
                const char *command = option[flag_index + 1] != '\0'
                    ? &option[flag_index + 1]
                    : (i + 1 < argc ? argv[++i] : NULL);
                if (!command) return -1;
                options->commands[options->command_count++] = command;
                flag_index = strlen(option) - 1;
                break;
            }
            case 'f':
            case 'F': options->create = 1; break;
            case 'r': options->read_only = 1; break;
            case 'd': options->direct = 1; break;
            case 'a': options->append = 1; break;
            case 't': options->truncate = 1; break;
            case 's': options->sync = 1; break;
            case 'T': options->tmpfile = 1; break;
            case 'm': {
                const char *mode_text = option[flag_index + 1] != '\0'
                    ? &option[flag_index + 1]
                    : (i + 1 < argc ? argv[++i] : NULL);
                if (!mode_text || !*mode_text) return -1;
                char *end = NULL;
                errno = 0;
                unsigned long mode = strtoul(mode_text, &end, 8);
                if (errno || end == mode_text || *end || mode > 07777) return -1;
                options->mode = (mode_t)mode;
                flag_index = strlen(option) - 1;
                break;
            }
            default: break;
            }
        }
    }
    if (!options->command_count) return -1;
    if (options->path) return 0;

    /* Upstream xfs_io permits global help queries without opening a file. */
    for (size_t i = 0; i < options->command_count; i++) {
        const char *command = options->commands[i];
        while (*command == ' ' || *command == '\t') command++;
        if (strncmp(command, "help", 4) != 0 ||
            (command[4] != '\0' && command[4] != ' ' && command[4] != '\t'))
            return -1;
    }
    return 0;
}

int main(int argc, char **argv) {
    struct options options = {.mode = 0600};
    if (parse_options(argc, argv, &options) != 0) {
        fprintf(stderr, "usage: xfs_io [-f] -c command file\n");
        return 1;
    }
    int fd = -1;
    if (options.path) {
        int flags = options.read_only ? O_RDONLY : O_RDWR;
        if (options.create) flags |= O_CREAT;
        if (options.direct) flags |= O_DIRECT;
        if (options.append) flags |= O_APPEND;
        if (options.truncate) flags |= O_TRUNC;
        if (options.sync) flags |= O_SYNC;
        if (options.tmpfile) flags |= O_TMPFILE;
        fd = open(options.path, flags, options.mode);
        if (fd < 0) { perror(options.path); return 1; }
    }
    int status = 0;
    struct mapping_state mapping = {0};
    for (size_t i = 0; i < options.command_count; i++) {
        if (execute_command(&fd, options.path ? options.path : "", options.commands[i],
                            options.direct, &mapping) != 0)
            status = 1;
    }
    if (mapping.address && munmap(mapping.address, (size_t)mapping.length) != 0) {
        perror("munmap");
        status = 1;
    }
    if (fd >= 0 && close(fd) != 0) { perror("close"); status = 1; }
    return status;
}
