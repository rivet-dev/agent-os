/* pipe_edge.c -- pipe edge cases: large write, broken pipe, EOF, close-both */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <errno.h>
#include <signal.h>

#define LARGE_SIZE  (128 * 1024)  /* 128KB > 64KB pipe buffer */
#define CHUNK_SIZE  32768         /* 32KB per chunk — fits in pipe buffer */

int main(void) {
    /* Test 1: large pipe write — 128KB through pipe via chunked write/read
     * Alternates write/read in 32KB chunks so total throughput exceeds
     * the 64KB pipe buffer without requiring a concurrent reader process. */
    {
        int p[2];
        if (pipe(p) != 0) {
            printf("large_write: FAIL (pipe creation failed)\n");
            return 1;
        }

        char *wbuf = (char *)malloc(CHUNK_SIZE);
        char *rbuf = (char *)malloc(LARGE_SIZE);
        if (!wbuf || !rbuf) {
            printf("large_write: FAIL (malloc failed)\n");
            return 1;
        }
        memset(wbuf, 'A', CHUNK_SIZE);

        size_t total_written = 0;
        size_t total_read = 0;
        int ok = 1;

        /* Write and read in alternating chunks */
        while (total_written < (size_t)LARGE_SIZE) {
            /* Write one chunk */
            size_t to_write = CHUNK_SIZE;
            if (total_written + to_write > (size_t)LARGE_SIZE)
                to_write = (size_t)LARGE_SIZE - total_written;

            ssize_t w = write(p[1], wbuf, to_write);
            if (w <= 0) {
                fprintf(stderr, "large_write: write failed at %zu errno=%d\n",
                        total_written, errno);
                ok = 0;
                break;
            }
            total_written += (size_t)w;

            /* Read back what was written */
            while (total_read < total_written) {
                ssize_t r = read(p[0], rbuf + total_read,
                                 total_written - total_read);
                if (r <= 0) {
                    fprintf(stderr, "large_write: read failed at %zu errno=%d\n",
                            total_read, errno);
                    ok = 0;
                    break;
                }
                total_read += (size_t)r;
            }
            if (!ok) break;
        }

        /* Close write end so read gets EOF */
        close(p[1]);

        /* Drain any remaining bytes (should be none) */
        if (ok) {
            ssize_t r;
            while ((r = read(p[0], rbuf + total_read,
                             (size_t)LARGE_SIZE - total_read)) > 0)
                total_read += (size_t)r;
        }
        close(p[0]);

        /* Verify all data transferred */
        if (ok && total_written == (size_t)LARGE_SIZE
              && total_read == (size_t)LARGE_SIZE) {
            /* Verify data integrity — all bytes should be 'A' */
            int corrupt = 0;
            for (size_t i = 0; i < total_read; i++) {
                if (rbuf[i] != 'A') { corrupt = 1; break; }
            }
            if (!corrupt) {
                printf("large_write: ok\n");
            } else {
                printf("large_write: FAIL (data corruption detected)\n");
            }
        } else {
            printf("large_write: FAIL (written=%zu, read=%zu, expected=%d)\n",
                   total_written, total_read, LARGE_SIZE);
        }
        printf("large_write_bytes=%zu\n", total_read);

        free(wbuf);
        free(rbuf);
    }

    /* Test 2: broken pipe — close read end, write to write end -> EPIPE */
    {
        signal(SIGPIPE, SIG_IGN);
        int p[2];
        if (pipe(p) != 0) {
            printf("broken_pipe: FAIL (pipe creation failed)\n");
            return 1;
        }
        close(p[0]); /* close read end */

        errno = 0;
        ssize_t n = write(p[1], "x", 1);
        int saved_errno = errno;
        close(p[1]);

        if (n == -1 && saved_errno == EPIPE) {
            printf("broken_pipe: ok\n");
        } else {
            printf("broken_pipe: FAIL (write returned %zd, errno=%d, expected -1/EPIPE=%d)\n",
                   n, saved_errno, EPIPE);
        }
        printf("broken_pipe_errno=%d\n", saved_errno);
    }

    /* Test 3: EOF — close write end, read from read end -> 0 */
    {
        int p[2];
        if (pipe(p) != 0) {
            printf("eof_read: FAIL (pipe creation failed)\n");
            return 1;
        }
        close(p[1]); /* close write end */

        char buf[16];
        ssize_t n = read(p[0], buf, sizeof(buf));
        close(p[0]);

        if (n == 0) {
            printf("eof_read: ok\n");
        } else {
            printf("eof_read: FAIL (read returned %zd, expected 0)\n", n);
        }
        printf("eof_read_result=%zd\n", n);
    }

    /* Test 4: close both ends — no crash or leak */
    {
        int p[2];
        if (pipe(p) != 0) {
            printf("close_both: FAIL (pipe creation failed)\n");
            return 1;
        }
        close(p[0]);
        close(p[1]);
        printf("close_both: ok\n");
    }

    return 0;
}
