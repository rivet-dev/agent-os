#ifndef REGISTRY_NATIVE_C_INCLUDE_SCHED_H
#define REGISTRY_NATIVE_C_INCLUDE_SCHED_H

#include_next <sched.h>

#ifdef _GNU_SOURCE
int sched_getcpu(void);
#endif

#endif
