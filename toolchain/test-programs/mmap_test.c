#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

static int verify_payload(const char *path, const char *expected) {
    char actual[9] = {0};
    int fd = open(path, O_RDONLY);
    if (fd < 0 || read(fd, actual, 8) != 8 || close(fd) != 0) return -1;
    return memcmp(actual, expected, 8);
}

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: mmap_test FILE\n");
        return 2;
    }
    int fd = open(argv[1], O_CREAT | O_TRUNC | O_RDWR, 0600);
    if (fd < 0 || write(fd, "abcdefgh", 8) != 8) {
        perror("prepare mmap");
        return 1;
    }
    char *shared = mmap(NULL, 8, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (shared == MAP_FAILED || close(fd) != 0) {
        perror("mmap shared");
        return 1;
    }
    long page_size = sysconf(_SC_PAGESIZE);
    if (page_size < 8) {
        fprintf(stderr, "invalid page size: %ld\n", page_size);
        return 1;
    }
    if ((uintptr_t)shared % (uintptr_t)page_size != 0) {
        fprintf(stderr, "mmap address is not page aligned\n");
        return 1;
    }
    for (long i = 8; i < page_size; i++) {
        if (shared[i] != 0) {
            fprintf(stderr, "nonzero mmap byte past EOF at %ld\n", i);
            return 1;
        }
    }
    memcpy(shared + 2, "XY", 2);
    if (msync(shared, 8, MS_SYNC) != 0 || munmap(shared, 8) != 0 ||
        verify_payload(argv[1], "abXYefgh") != 0) {
        perror("shared writeback");
        return 1;
    }

    fd = open(argv[1], O_RDONLY);
    char *private_map = mmap(NULL, 8, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 0);
    if (private_map == MAP_FAILED || close(fd) != 0) {
        perror("mmap private");
        return 1;
    }
    memcpy(private_map, "PRIVATE!", 8);
    if (munmap(private_map, 8) != 0 || verify_payload(argv[1], "abXYefgh") != 0) {
        perror("private isolation");
        return 1;
    }

    fd = open(argv[1], O_RDWR | O_DIRECT);
    if (fd < 0 || (fcntl(fd, F_GETFL) & O_DIRECT) == 0) {
        perror("open direct mmap");
        return 1;
    }
    shared = mmap(NULL, 8, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (shared == MAP_FAILED) {
        perror("mmap direct");
        return 1;
    }
    memcpy(shared + 4, "12", 2);
    if (msync(shared, 8, MS_SYNC) != 0 ||
        (fcntl(fd, F_GETFL) & O_DIRECT) == 0 ||
        munmap(shared, 8) != 0 || close(fd) != 0 ||
        verify_payload(argv[1], "abXY12gh") != 0) {
        perror("direct shared writeback");
        return 1;
    }

    size_t page_length = (size_t)page_size;
    unsigned char *page = NULL;
    if (posix_memalign((void **)&page, page_length, page_length) != 0) {
        fprintf(stderr, "page allocation failed\n");
        return 1;
    }
    memset(page, 'C', page_length);
    fd = open(argv[1], O_CREAT | O_TRUNC | O_RDWR, 0600);
    if (fd < 0 || ftruncate(fd, (off_t)(2 * page_length)) != 0 ||
        pwrite(fd, page, page_length, (off_t)page_length) != (ssize_t)page_length) {
        perror("prepare mmap coherence");
        free(page);
        return 1;
    }

    private_map = mmap(NULL, 2 * page_length, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE, fd, 0);
    if (private_map == MAP_FAILED ||
        pwrite(fd, private_map + page_length, page_length, 0) !=
            (ssize_t)page_length ||
        memcmp(private_map, page, page_length) != 0) {
        perror("private clean-page coherence");
        free(page);
        close(fd);
        return 1;
    }
    private_map[0] = 'P';
    if (pwrite(fd, "D", 1, 0) != 1 || private_map[0] != 'P' ||
        munmap(private_map, 2 * page_length) != 0) {
        perror("private copy-on-write isolation");
        free(page);
        close(fd);
        return 1;
    }

    shared = mmap(NULL, 2 * page_length, PROT_READ | PROT_WRITE,
                  MAP_SHARED, fd, 0);
    if (shared == MAP_FAILED || shared[0] != 'D' ||
        pwrite(fd, "S", 1, 0) != 1 || shared[0] != 'S' ||
        munmap(shared, 2 * page_length) != 0 || close(fd) != 0) {
        perror("shared positioned-write coherence");
        free(page);
        return 1;
    }
    free(page);
    puts("mmap: ok");
    return 0;
}
