#define _GNU_SOURCE
#include <fcntl.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_renameat2
#  if defined(__x86_64__)
#    define SYS_renameat2 316
#  elif defined(__aarch64__)
#    define SYS_renameat2 276
#  elif defined(__arm__)
#    define SYS_renameat2 382
#  elif defined(__i386__)
#    define SYS_renameat2 353
#  elif defined(__powerpc64__)
#    define SYS_renameat2 357
#  else
#    error "renameat2 syscall number unknown for this architecture"
#  endif
#endif

int renameat2(int olddirfd, const char *oldpath, int newdirfd,
              const char *newpath, unsigned int flags) {
  return syscall(SYS_renameat2, olddirfd, oldpath, newdirfd, newpath, flags);
}
