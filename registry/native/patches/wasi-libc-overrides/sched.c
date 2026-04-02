#include <errno.h>
#include <sched.h>

int sched_getcpu(void) {
    errno = ENOSYS;
    return -1;
}
