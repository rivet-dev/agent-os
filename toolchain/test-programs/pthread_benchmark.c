#include <pthread.h>
#include <stdint.h>
#include <unistd.h>

static pthread_mutex_t gate_mutex = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t gate_condition = PTHREAD_COND_INITIALIZER;
static unsigned ready_threads;
static int release_threads;

static void *parked_worker(void *argument) {
    (void)argument;
    if (pthread_mutex_lock(&gate_mutex) != 0) {
        return (void *)(uintptr_t)1;
    }
    ready_threads++;
    pthread_cond_broadcast(&gate_condition);
    while (!release_threads) {
        if (pthread_cond_wait(&gate_condition, &gate_mutex) != 0) {
            pthread_mutex_unlock(&gate_mutex);
            return (void *)(uintptr_t)2;
        }
    }
    pthread_mutex_unlock(&gate_mutex);
    return NULL;
}

static int parse_unsigned(const char *text, unsigned minimum, unsigned maximum,
                          unsigned *output) {
    unsigned value = 0;
    if (*text == '\0') {
        return -1;
    }
    for (const char *cursor = text; *cursor != '\0'; cursor++) {
        if (*cursor < '0' || *cursor > '9') {
            return -1;
        }
        value = value * 10u + (unsigned)(*cursor - '0');
        if (value > maximum) {
            return -1;
        }
    }
    if (value < minimum) {
        return -1;
    }
    *output = (unsigned)value;
    return 0;
}

static void write_state(const char *state, unsigned thread_count) {
    char output[16];
    unsigned length = 0;
    while (state[length] != '\0') {
        output[length] = state[length];
        length++;
    }
    if (thread_count >= 10) {
        output[length++] = (char)('0' + thread_count / 10);
    }
    output[length++] = (char)('0' + thread_count % 10);
    output[length++] = '\n';
    (void)write(STDOUT_FILENO, output, length);
}

int main(int argc, char **argv) {
    unsigned thread_count = 1;
    unsigned park = 0;
    if ((argc > 1 && parse_unsigned(argv[1], 1, 15, &thread_count) != 0) ||
        (argc > 2 && parse_unsigned(argv[2], 0, 1, &park) != 0)) {
        static const char usage[] =
            "usage: pthread_benchmark [threads:1..15] [park:0|1]\n";
        (void)write(STDERR_FILENO, usage, sizeof(usage) - 1);
        return 2;
    }

    pthread_t threads[15];
    for (unsigned index = 0; index < thread_count; index++) {
        if (pthread_create(&threads[index], NULL, parked_worker, NULL) != 0) {
            return 10;
        }
    }

    pthread_mutex_lock(&gate_mutex);
    while (ready_threads != thread_count) {
        if (pthread_cond_wait(&gate_condition, &gate_mutex) != 0) {
            pthread_mutex_unlock(&gate_mutex);
            return 11;
        }
    }
    pthread_mutex_unlock(&gate_mutex);

    write_state("ready:", thread_count);
    if (park != 0) {
        pthread_mutex_lock(&gate_mutex);
        for (;;) {
            pthread_cond_wait(&gate_condition, &gate_mutex);
        }
    }

    pthread_mutex_lock(&gate_mutex);
    release_threads = 1;
    pthread_cond_broadcast(&gate_condition);
    pthread_mutex_unlock(&gate_mutex);

    for (unsigned index = 0; index < thread_count; index++) {
        void *result = NULL;
        if (pthread_join(threads[index], &result) != 0 || result != NULL) {
            return 13;
        }
    }
    write_state("done:", thread_count);
    return 0;
}
