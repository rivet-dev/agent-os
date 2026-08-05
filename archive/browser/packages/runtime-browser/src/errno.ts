// Canonical Linux errno numbers, so guest-facing errors carry the same negated
// `errno` Node reports (e.g. ENOENT -> -2, EEXIST -> -17) for every code, not
// just a hardcoded few. The structured error `code` (from the kernel or the
// fd-table) is the source of truth; this maps it to the number.
//
// Mirrors the POSIX_ERRNO table in packages/build-tools/bridge-src/prelude.ts
// (the V8 bridge bundle is a separate generated JS asset that cannot import this
// TS module). Keep the two in sync.
const POSIX_ERRNO: Record<string, number> = {
	EPERM: 1,
	ENOENT: 2,
	ESRCH: 3,
	EINTR: 4,
	EIO: 5,
	ENXIO: 6,
	E2BIG: 7,
	ENOEXEC: 8,
	EBADF: 9,
	ECHILD: 10,
	EAGAIN: 11,
	EWOULDBLOCK: 11,
	ENOMEM: 12,
	EACCES: 13,
	EFAULT: 14,
	ENOTBLK: 15,
	EBUSY: 16,
	EEXIST: 17,
	EXDEV: 18,
	ENODEV: 19,
	ENOTDIR: 20,
	EISDIR: 21,
	EINVAL: 22,
	ENFILE: 23,
	EMFILE: 24,
	ENOTTY: 25,
	ETXTBSY: 26,
	EFBIG: 27,
	ENOSPC: 28,
	ESPIPE: 29,
	EROFS: 30,
	EMLINK: 31,
	EPIPE: 32,
	EDOM: 33,
	ERANGE: 34,
	ENAMETOOLONG: 36,
	ENOSYS: 38,
	ENOTEMPTY: 39,
	ELOOP: 40,
	EOVERFLOW: 75,
	ENOTSOCK: 88,
	EDESTADDRREQ: 89,
	EMSGSIZE: 90,
	EPROTOTYPE: 91,
	ENOPROTOOPT: 92,
	EPROTONOSUPPORT: 93,
	ENOTSUP: 95,
	EOPNOTSUPP: 95,
	EAFNOSUPPORT: 97,
	EADDRINUSE: 98,
	EADDRNOTAVAIL: 99,
	ENETDOWN: 100,
	ENETUNREACH: 101,
	ECONNABORTED: 103,
	ECONNRESET: 104,
	ENOBUFS: 105,
	EISCONN: 106,
	ENOTCONN: 107,
	ETIMEDOUT: 110,
	ECONNREFUSED: 111,
	EHOSTUNREACH: 113,
	EALREADY: 114,
	EINPROGRESS: 115,
};

/** The negated Linux errno for a code (Node's `err.errno`), or undefined. */
export function posixErrno(code: string | undefined): number | undefined {
	if (code && Object.prototype.hasOwnProperty.call(POSIX_ERRNO, code)) {
		return -POSIX_ERRNO[code];
	}
	return undefined;
}
