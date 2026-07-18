#define _GNU_SOURCE

#include <errno.h>
#include <poll.h>
#include <signal.h>
#include <spawn.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

extern char **environ;

static volatile sig_atomic_t sigchld_calls;

static int poll_abi_matches_linux(void) {
    int ok = POLLIN == 0x001 && POLLPRI == 0x002 && POLLOUT == 0x004 &&
        POLLERR == 0x008 && POLLHUP == 0x010 && POLLNVAL == 0x020 &&
        POLLRDNORM == 0x040 && POLLWRNORM == 0x100;
    printf("poll_linux_abi=%s\n", ok ? "yes" : "no");
    return ok;
}

static int normal_read_write_aliases(void) {
    int descriptors[2];
    if (pipe(descriptors) != 0) {
        fprintf(stderr, "normal aliases: pipe errno=%d\n", errno);
        return 0;
    }
    char byte = 'x';
    if (write(descriptors[1], &byte, 1) != 1) {
        fprintf(stderr, "normal aliases: write errno=%d\n", errno);
        close(descriptors[0]);
        close(descriptors[1]);
        return 0;
    }
    struct pollfd fds[2] = {
        {.fd = descriptors[0], .events = POLLRDNORM},
        {.fd = descriptors[1], .events = POLLWRNORM},
    };
    int result = poll(fds, 2, 0);
    int ok = result == 2 && (fds[0].revents & POLLRDNORM) != 0 &&
        (fds[1].revents & POLLWRNORM) != 0;
    close(descriptors[0]);
    close(descriptors[1]);
    printf("poll_normal_aliases=%s\n", ok ? "yes" : "no");
    if (!ok)
        fprintf(stderr,
            "normal aliases: rc=%d errno=%d read_revents=0x%x "
            "write_revents=0x%x\n",
            result, errno, fds[0].revents, fds[1].revents);
    return ok;
}

static void sigchld_handler(int signal) {
    (void)signal;
    sigchld_calls++;
}

static int mask_member(int signal) {
    sigset_t current;
    if (sigprocmask(SIG_SETMASK, NULL, &current) != 0)
        return -1;
    return sigismember(&current, signal);
}

static int spawn_child(const char *self, const char *mode, pid_t *child) {
    char *argv[] = {(char *)self, (char *)mode, NULL};
    *child = -1;
    return posix_spawnp(child, self, NULL, NULL, argv, environ);
}

static int wait_child(pid_t child) {
    int status = 0;
    return waitpid(child, &status, 0) == child && WIFEXITED(status) &&
        WEXITSTATUS(status) == 0;
}

static int child_exit_probe(pid_t child, int *child_ok) {
    int status = 0;
    pid_t result = waitpid(child, &status, WNOHANG);

    if (result == child) {
        *child_ok = WIFEXITED(status) && WEXITSTATUS(status) == 0;
        return 1;
    }
    *child_ok = 0;
    return result == 0 ? 0 : -1;
}

static int pending_signal_interrupts(const char *self) {
    sigset_t blocked, empty, original;
    struct timespec timeout = {.tv_sec = 1, .tv_nsec = 0};
    pid_t child;
    int child_ok = 0;

    sigemptyset(&blocked);
    sigaddset(&blocked, SIGCHLD);
    sigemptyset(&empty);
    if (sigprocmask(SIG_BLOCK, &blocked, &original) != 0 ||
        spawn_child(self, "--immediate-exit", &child) != 0)
        return 0;
    usleep(50000);
    int exited_before_ppoll = child_exit_probe(child, &child_ok);
    sigchld_calls = 0;
    errno = 0;
    int result = ppoll(NULL, 0, &timeout, &empty);
    int saved_errno = errno;
    int restored_blocked = mask_member(SIGCHLD) == 1;
    if (exited_before_ppoll == 0)
        child_ok = wait_child(child);
    sigprocmask(SIG_SETMASK, &original, NULL);
    int ok = result == -1 && saved_errno == EINTR && sigchld_calls == 1 &&
        restored_blocked && child_ok;
    printf("ppoll_pending_unblocked_eintr=%s\n", ok ? "yes" : "no");
    if (!ok)
        fprintf(stderr,
            "pending: rc=%d errno=%d handlers=%d restored_blocked=%d "
            "exited_before_ppoll=%d child_ok=%d\n",
            result, saved_errno, (int)sigchld_calls, restored_blocked,
            exited_before_ppoll, child_ok);
    return ok;
}

static int temporary_block_preserves_ready_result(const char *self) {
    int descriptors[2];
    sigset_t blocked, original;
    struct timespec timeout = {.tv_sec = 1, .tv_nsec = 0};
    pid_t child;
    int child_ok = 0;

    if (pipe(descriptors) != 0)
        return 0;
    sigemptyset(&blocked);
    sigaddset(&blocked, SIGCHLD);
    if (sigprocmask(SIG_UNBLOCK, &blocked, &original) != 0 ||
        spawn_child(self, "--delayed-exit", &child) != 0) {
        close(descriptors[0]);
        close(descriptors[1]);
        return 0;
    }
    close(descriptors[1]);
    struct pollfd pollfd = {
        .fd = descriptors[0],
        .events = POLLIN,
    };
    int exited_before_ppoll = child_exit_probe(child, &child_ok);
    sigchld_calls = 0;
    errno = 0;
    int result = ppoll(&pollfd, 1, &timeout, &blocked);
    int saved_errno = errno;
    int restored_unblocked = mask_member(SIGCHLD) == 0;
    if (exited_before_ppoll == 0)
        child_ok = wait_child(child);
    close(descriptors[0]);
    sigprocmask(SIG_SETMASK, &original, NULL);
    int ok = result == 1 && (pollfd.revents & POLLHUP) != 0 &&
        saved_errno == 0 && sigchld_calls == 1 && restored_unblocked &&
        child_ok;
    printf("ppoll_temporary_block_ready=%s\n", ok ? "yes" : "no");
    if (!ok)
        fprintf(stderr,
            "temporary: rc=%d errno=%d handlers=%d revents=0x%x "
            "restored_unblocked=%d exited_before_ppoll=%d child_ok=%d\n",
            result, saved_errno, (int)sigchld_calls, pollfd.revents,
            restored_unblocked, exited_before_ppoll, child_ok);
    return ok;
}

static int error_restores_original_mask(void) {
    sigset_t blocked;
    struct timespec invalid = {.tv_sec = 0, .tv_nsec = 1000000000L};

    sigemptyset(&blocked);
    sigaddset(&blocked, SIGCHLD);
    sigprocmask(SIG_UNBLOCK, &blocked, NULL);
    errno = 0;
    int result = ppoll(NULL, 0, &invalid, &blocked);
    int ok = result == -1 && errno == EINVAL && mask_member(SIGCHLD) == 0;
    printf("ppoll_error_mask_restore=%s\n", ok ? "yes" : "no");
    return ok;
}

int main(int argc, char **argv) {
    if (argc == 2 && strcmp(argv[1], "--immediate-exit") == 0)
        return 0;
    if (argc == 2 && strcmp(argv[1], "--delayed-exit") == 0) {
        usleep(50000);
        return 0;
    }

    struct sigaction action;
    memset(&action, 0, sizeof(action));
    action.sa_handler = sigchld_handler;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGCHLD, &action, NULL) != 0)
        return 1;

    int ok = poll_abi_matches_linux();
    ok &= normal_read_write_aliases();
    ok &= pending_signal_interrupts(argv[0]);
    ok &= temporary_block_preserves_ready_result(argv[0]);
    ok &= error_restores_original_mask();
    printf("ppoll_contract=%s\n", ok ? "ok" : "FAIL");
    return ok ? 0 : 1;
}
