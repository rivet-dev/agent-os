/**
 * Minimal mlock/munlock shim for WASI/POSIX builds without page pinning.
 *
 * DuckDB uses mlock/munlock as a best-effort hardening step for encryption
 * keys. Our runtime does not currently expose memory locking primitives, so
 * treat these calls as successful no-ops instead of failing the link.
 *
 * Installed into the patched sysroot so upstream code can keep its existing
 * POSIX calls without carrying a WASI-specific source patch.
 */

#include <stddef.h>

int mlock(const void *addr, size_t len) {
	(void)addr;
	(void)len;
	return 0;
}

int munlock(const void *addr, size_t len) {
	(void)addr;
	(void)len;
	return 0;
}

int mlockall(int flags) {
	(void)flags;
	return 0;
}

int munlockall(void) {
	return 0;
}

int madvise(void *addr, size_t len, int advice) {
	(void)addr;
	(void)len;
	(void)advice;
	return 0;
}
