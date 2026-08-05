#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <stdint.h>
#include <unistd.h>

static pthread_mutex_t mutex = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t condition = PTHREAD_COND_INITIALIZER;
static pthread_key_t tls_key;
static int joined_ready;
static int detached_ready;
static int tls_destructors;

static void tls_destructor(void *value) {
    if (value != NULL) {
        pthread_mutex_lock(&mutex);
        tls_destructors++;
        pthread_cond_broadcast(&condition);
        pthread_mutex_unlock(&mutex);
    }
}

static void *joined_worker(void *argument) {
    pthread_setspecific(tls_key, argument);
    pthread_mutex_lock(&mutex);
    joined_ready = 1;
    pthread_cond_broadcast(&condition);
    pthread_mutex_unlock(&mutex);
    return (void *)(uintptr_t)42;
}

static void *detached_worker(void *argument) {
    pthread_setspecific(tls_key, argument);
    pthread_mutex_lock(&mutex);
    detached_ready = 1;
    pthread_cond_broadcast(&condition);
    pthread_mutex_unlock(&mutex);
    return NULL;
}

static void *cancelled_worker(void *argument) {
    (void)argument;
    for (;;) {
        pthread_testcancel();
        sched_yield();
    }
}

int main(void) {
    pthread_t joined;
    pthread_t detached;
    pthread_t cancelled;
    pthread_attr_t detached_attributes;
    void *joined_result = NULL;
    void *cancelled_result = NULL;

    /*
     * Force the owned fcntl override into this threaded link. This catches a
     * threaded sysroot whose upstream libc objects use atomics/shared memory
     * while AgentOS override objects were accidentally compiled for the
     * ordinary single-thread target.
     */
    if (fcntl(STDOUT_FILENO, F_GETFD) < 0) {
        return 9;
    }

    if (pthread_key_create(&tls_key, tls_destructor) != 0 ||
        pthread_create(&joined, NULL, joined_worker, (void *)(uintptr_t)1) != 0) {
        return 10;
    }
    pthread_mutex_lock(&mutex);
    while (!joined_ready) {
        pthread_cond_wait(&condition, &mutex);
    }
    pthread_mutex_unlock(&mutex);
    if (pthread_join(joined, &joined_result) != 0 ||
        (uintptr_t)joined_result != 42) {
        return 11;
    }

    if (pthread_attr_init(&detached_attributes) != 0 ||
        pthread_attr_setdetachstate(&detached_attributes, PTHREAD_CREATE_DETACHED) != 0 ||
        pthread_create(&detached, &detached_attributes, detached_worker,
                       (void *)(uintptr_t)1) != 0) {
        return 12;
    }
    pthread_attr_destroy(&detached_attributes);
    pthread_mutex_lock(&mutex);
    while (!detached_ready || tls_destructors < 2) {
        pthread_cond_wait(&condition, &mutex);
    }
    pthread_mutex_unlock(&mutex);

    if (pthread_create(&cancelled, NULL, cancelled_worker, NULL) != 0 ||
        pthread_cancel(cancelled) != 0 ||
        pthread_join(cancelled, &cancelled_result) != 0 ||
        cancelled_result != PTHREAD_CANCELED) {
        return 13;
    }

    pthread_key_delete(tls_key);
    if (write(STDOUT_FILENO, "pthread-ok\n", 11) != 11) {
        return 14;
    }
    return 0;
}
