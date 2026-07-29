/**
 * POSIX umask surface for AgentOS WASI.
 *
 * Ownership syscalls are implemented in the patched wasi-libc posix.c. Keep
 * this override limited to umask so libc.a has exactly one chown/fchown family.
 */

#include <errno.h>
#include <stdint.h>
#include <sys/stat.h>
#include <wasi/api.h>

__attribute__((import_module("host_process"), import_name("proc_umask")))
__wasi_errno_t host_proc_umask(uint32_t mask, uint32_t *ret_previous);

mode_t umask(mode_t mask) {
	uint32_t previous = 0;
	__wasi_errno_t error = host_proc_umask((uint32_t)mask & 0777, &previous);
	if (error != __WASI_ERRNO_SUCCESS) {
		errno = error;
		return (mode_t)-1;
	}
	return (mode_t)previous;
}
