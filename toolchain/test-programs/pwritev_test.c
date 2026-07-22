#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/uio.h>
#include <unistd.h>

static int fail(const char *operation) {
    perror(operation);
    return 1;
}

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: pwritev_test FILE\n");
        return 2;
    }
    int fd = open(argv[1], O_CREAT | O_TRUNC | O_RDWR, 0600);
    if (fd < 0 || write(fd, "00xxxxxxxx", 10) != 10) {
        return fail("prepare");
    }

    struct iovec vectors[] = {
        {.iov_base = "abc", .iov_len = 3},
        {.iov_base = "def", .iov_len = 3},
    };
    if (pwritev(fd, vectors, 2, 2) != 6) {
        fail("pwritev");
        close(fd);
        return 1;
    }

    char actual[11] = {0};
    if (pread(fd, actual, 10, 0) != 10 || memcmp(actual, "00abcdefxx", 10) != 0) {
        fprintf(stderr, "pwritev payload mismatch\n");
        close(fd);
        return 1;
    }

    char first[4] = {0};
    char second[4] = {0};
    struct iovec read_vectors[] = {
        {.iov_base = first, .iov_len = 3},
        {.iov_base = second, .iov_len = 3},
    };
    if (preadv2(fd, read_vectors, 2, 2, 0) != 6 ||
        memcmp(first, "abc", 3) != 0 || memcmp(second, "def", 3) != 0) {
        fprintf(stderr, "preadv2 payload mismatch\n");
        close(fd);
        return 1;
    }

    struct iovec v2_vector = {.iov_base = "gh", .iov_len = 2};
    if (pwritev2(fd, &v2_vector, 1, 8, 0) != 2) {
        fail("pwritev2");
        close(fd);
        return 1;
    }
    errno = 0;
    if (preadv2(fd, read_vectors, 2, 0, 1) != -1 || errno != EOPNOTSUPP) {
        fprintf(stderr, "preadv2 flags errno mismatch: %d\n", errno);
        close(fd);
        return 1;
    }
    errno = 0;
    if (pwritev2(fd, &v2_vector, 1, 0, 1) != -1 ||
        errno != EOPNOTSUPP) {
        fprintf(stderr, "pwritev2 flags errno mismatch: %d\n", errno);
        close(fd);
        return 1;
    }

    struct stat status;
    if (fallocate(fd, 0, 16, 4) != 0 || fstat(fd, &status) != 0 ||
        status.st_size != 20) {
        fail("fallocate");
        close(fd);
        return 1;
    }
    if (fallocate(fd, FALLOC_FL_KEEP_SIZE, 24, 4) != 0 ||
        fstat(fd, &status) != 0 || status.st_size != 20) {
        fail("fallocate keep-size");
        close(fd);
        return 1;
    }
    if (pwrite(fd, "PUNC", 4, 12) != 4 ||
        fallocate(fd, FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE, 12, 4) != 0) {
        fail("fallocate punch-hole");
        close(fd);
        return 1;
    }
    char punched[4];
    const char zeroes[4] = {0};
    if (pread(fd, punched, sizeof(punched), 12) != (ssize_t)sizeof(punched) ||
        memcmp(punched, zeroes, sizeof(punched)) != 0) {
        fprintf(stderr, "fallocate punch-hole payload mismatch\n");
        close(fd);
        return 1;
    }
    errno = 0;
    if (fallocate(fd, FALLOC_FL_NO_HIDE_STALE, 0, 1) != -1 ||
        errno != EOPNOTSUPP) {
        fprintf(stderr, "fallocate flags errno mismatch: %d\n", errno);
        close(fd);
        return 1;
    }

    char splice_path[4096];
    if (snprintf(splice_path, sizeof(splice_path), "%s.splice", argv[1]) >=
        (int)sizeof(splice_path)) {
        fprintf(stderr, "splice path is too long\n");
        close(fd);
        return 1;
    }
    int splice_fd = open(splice_path, O_CREAT | O_TRUNC | O_RDWR, 0600);
    if (splice_fd < 0 || write(splice_fd, "........", 8) != 8) {
        fail("prepare splice");
        close(splice_fd);
        close(fd);
        return 1;
    }
    off_t input_offset = 2;
    off_t output_offset = 1;
    if (splice(fd, &input_offset, splice_fd, &output_offset, 6,
               SPLICE_F_MORE) != 6 ||
        input_offset != 8 || output_offset != 7) {
        fail("splice");
        close(splice_fd);
        close(fd);
        return 1;
    }
    char splice_actual[9] = {0};
    if (pread(splice_fd, splice_actual, 8, 0) != 8 ||
        memcmp(splice_actual, ".abcdef.", 8) != 0) {
        fprintf(stderr, "splice payload mismatch\n");
        close(splice_fd);
        close(fd);
        return 1;
    }
    errno = 0;
    if (splice(fd, NULL, splice_fd, NULL, 1, 0x80000000U) != -1 ||
        errno != EINVAL) {
        fprintf(stderr, "splice flags errno mismatch: %d\n", errno);
        close(splice_fd);
        close(fd);
        return 1;
    }
    if (close(splice_fd) != 0) {
        close(fd);
        return fail("close splice");
    }
    if (close(fd) != 0) {
        return fail("close");
    }
    puts("vector-io-splice-fallocate: ok");
    return 0;
}
