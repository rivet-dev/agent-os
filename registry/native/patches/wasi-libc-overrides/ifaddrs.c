/**
 * Minimal getifaddrs/freeifaddrs shim for runtimes without host network
 * interface enumeration.
 *
 * cpp-httplib includes ifaddrs support on POSIX targets, but DuckDB's embedded
 * HTTP client only uses it when callers opt into binding a request to a named
 * interface. Our WASI runtime does not expose interface enumeration today, so
 * return an empty list instead of failing the build or forcing DuckDB-specific
 * source patches.
 */

#include <errno.h>
#include <ifaddrs.h>

int getifaddrs(struct ifaddrs **ifap) {
	if (ifap) {
		*ifap = 0;
	}
	errno = ENOSYS;
	return -1;
}

void freeifaddrs(struct ifaddrs *ifa) {
	(void)ifa;
}
