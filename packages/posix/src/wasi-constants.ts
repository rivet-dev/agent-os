/**
 * WASI protocol constants.
 *
 * All constants from the wasi_snapshot_preview1 specification:
 * file types, fd flags, rights bitmasks, errno codes.
 */

// ---------------------------------------------------------------------------
// WASI file types (filetype enum)
// ---------------------------------------------------------------------------
export const FILETYPE_UNKNOWN = 0 as const;
export const FILETYPE_BLOCK_DEVICE = 1 as const;
export const FILETYPE_CHARACTER_DEVICE = 2 as const;
export const FILETYPE_DIRECTORY = 3 as const;
export const FILETYPE_REGULAR_FILE = 4 as const;
export const FILETYPE_SOCKET_DGRAM = 5 as const;
export const FILETYPE_SOCKET_STREAM = 6 as const;
export const FILETYPE_SYMBOLIC_LINK = 7 as const;

export type WasiFiletype =
  | typeof FILETYPE_UNKNOWN
  | typeof FILETYPE_BLOCK_DEVICE
  | typeof FILETYPE_CHARACTER_DEVICE
  | typeof FILETYPE_DIRECTORY
  | typeof FILETYPE_REGULAR_FILE
  | typeof FILETYPE_SOCKET_DGRAM
  | typeof FILETYPE_SOCKET_STREAM
  | typeof FILETYPE_SYMBOLIC_LINK;

// ---------------------------------------------------------------------------
// WASI fd flags (fdflags bitmask, u16)
// ---------------------------------------------------------------------------
export const FDFLAG_APPEND = 1 << 0;
export const FDFLAG_DSYNC = 1 << 1;
export const FDFLAG_NONBLOCK = 1 << 2;
export const FDFLAG_RSYNC = 1 << 3;
export const FDFLAG_SYNC = 1 << 4;

// ---------------------------------------------------------------------------
// WASI rights (rights bitmask, u64 — we use BigInt)
// ---------------------------------------------------------------------------
export const RIGHT_FD_DATASYNC = 1n << 0n;
export const RIGHT_FD_READ = 1n << 1n;
export const RIGHT_FD_SEEK = 1n << 2n;
export const RIGHT_FD_FDSTAT_SET_FLAGS = 1n << 3n;
export const RIGHT_FD_SYNC = 1n << 4n;
export const RIGHT_FD_TELL = 1n << 5n;
export const RIGHT_FD_WRITE = 1n << 6n;
export const RIGHT_FD_ADVISE = 1n << 7n;
export const RIGHT_FD_ALLOCATE = 1n << 8n;
export const RIGHT_PATH_CREATE_DIRECTORY = 1n << 9n;
export const RIGHT_PATH_CREATE_FILE = 1n << 10n;
export const RIGHT_PATH_LINK_SOURCE = 1n << 11n;
export const RIGHT_PATH_LINK_TARGET = 1n << 12n;
export const RIGHT_PATH_OPEN = 1n << 13n;
export const RIGHT_FD_READDIR = 1n << 14n;
export const RIGHT_PATH_READLINK = 1n << 15n;
export const RIGHT_PATH_RENAME_SOURCE = 1n << 16n;
export const RIGHT_PATH_RENAME_TARGET = 1n << 17n;
export const RIGHT_PATH_FILESTAT_GET = 1n << 18n;
export const RIGHT_PATH_FILESTAT_SET_SIZE = 1n << 19n;
export const RIGHT_PATH_FILESTAT_SET_TIMES = 1n << 20n;
export const RIGHT_FD_FILESTAT_GET = 1n << 21n;
export const RIGHT_FD_FILESTAT_SET_SIZE = 1n << 22n;
export const RIGHT_FD_FILESTAT_SET_TIMES = 1n << 23n;
export const RIGHT_PATH_SYMLINK = 1n << 24n;
export const RIGHT_PATH_REMOVE_DIRECTORY = 1n << 25n;
export const RIGHT_PATH_UNLINK_FILE = 1n << 26n;
export const RIGHT_POLL_FD_READWRITE = 1n << 27n;
export const RIGHT_SOCK_SHUTDOWN = 1n << 28n;
export const RIGHT_SOCK_ACCEPT = 1n << 29n;

// Convenience right sets
export const RIGHTS_STDIO: bigint = RIGHT_FD_READ | RIGHT_FD_WRITE | RIGHT_FD_FDSTAT_SET_FLAGS |
  RIGHT_FD_FILESTAT_GET | RIGHT_POLL_FD_READWRITE;

export const RIGHTS_FILE_ALL: bigint = RIGHT_FD_DATASYNC | RIGHT_FD_READ | RIGHT_FD_SEEK |
  RIGHT_FD_FDSTAT_SET_FLAGS | RIGHT_FD_SYNC | RIGHT_FD_TELL | RIGHT_FD_WRITE |
  RIGHT_FD_ADVISE | RIGHT_FD_ALLOCATE | RIGHT_FD_FILESTAT_GET |
  RIGHT_FD_FILESTAT_SET_SIZE | RIGHT_FD_FILESTAT_SET_TIMES |
  RIGHT_POLL_FD_READWRITE;

export const RIGHTS_DIR_ALL: bigint = RIGHT_FD_FDSTAT_SET_FLAGS | RIGHT_FD_SYNC |
  RIGHT_FD_READDIR | RIGHT_PATH_CREATE_DIRECTORY | RIGHT_PATH_CREATE_FILE |
  RIGHT_PATH_LINK_SOURCE | RIGHT_PATH_LINK_TARGET | RIGHT_PATH_OPEN |
  RIGHT_PATH_READLINK | RIGHT_PATH_RENAME_SOURCE | RIGHT_PATH_RENAME_TARGET |
  RIGHT_PATH_FILESTAT_GET | RIGHT_PATH_FILESTAT_SET_SIZE |
  RIGHT_PATH_FILESTAT_SET_TIMES | RIGHT_PATH_SYMLINK |
  RIGHT_PATH_REMOVE_DIRECTORY | RIGHT_PATH_UNLINK_FILE |
  RIGHT_FD_FILESTAT_GET | RIGHT_FD_FILESTAT_SET_TIMES;

// ---------------------------------------------------------------------------
// WASI errno codes (wasi_snapshot_preview1)
// ---------------------------------------------------------------------------
export const ERRNO_SUCCESS = 0;
export const ERRNO_EADDRINUSE = 3;
export const ERRNO_EACCES = 2;
export const ERRNO_EAGAIN = 6;
export const ERRNO_EBADF = 8;
export const ERRNO_ECHILD = 10;
export const ERRNO_ECONNREFUSED = 14;
export const ERRNO_EEXIST = 20;
export const ERRNO_EINPROGRESS = 26;
export const ERRNO_EINVAL = 28;
export const ERRNO_EIO = 76;
export const ERRNO_EISDIR = 31;
export const ERRNO_EMFILE = 33;
export const ERRNO_EMSGSIZE = 40;
export const ERRNO_ENOENT = 44;
export const ERRNO_ENOSPC = 51;
export const ERRNO_ENOSYS = 52;
export const ERRNO_ENOTCONN = 53;
export const ERRNO_ENOTDIR = 54;
export const ERRNO_ENOTEMPTY = 55;
export const ERRNO_EPERM = 63;
export const ERRNO_EPIPE = 64;
export const ERRNO_ESPIPE = 70;
export const ERRNO_ESRCH = 71;
export const ERRNO_ETIMEDOUT = 73;

/** Map POSIX error code strings to WASI errno numbers. */
export const ERRNO_MAP: Record<string, number> = {
	EACCES: ERRNO_EACCES,
	EADDRINUSE: ERRNO_EADDRINUSE,
	EAGAIN: ERRNO_EAGAIN,
	EBADF: ERRNO_EBADF,
	ECHILD: ERRNO_ECHILD,
	ECONNREFUSED: ERRNO_ECONNREFUSED,
	EEXIST: ERRNO_EEXIST,
	EINPROGRESS: ERRNO_EINPROGRESS,
	EINVAL: ERRNO_EINVAL,
	EIO: ERRNO_EIO,
	EISDIR: ERRNO_EISDIR,
	EMFILE: ERRNO_EMFILE,
	EMSGSIZE: ERRNO_EMSGSIZE,
	ENOENT: ERRNO_ENOENT,
	ENOSPC: ERRNO_ENOSPC,
	ENOSYS: ERRNO_ENOSYS,
	ENOTCONN: ERRNO_ENOTCONN,
	ENOTDIR: ERRNO_ENOTDIR,
	ENOTEMPTY: ERRNO_ENOTEMPTY,
	EPERM: ERRNO_EPERM,
	EPIPE: ERRNO_EPIPE,
	ESPIPE: ERRNO_ESPIPE,
	ESRCH: ERRNO_ESRCH,
	ETIMEDOUT: ERRNO_ETIMEDOUT,
};
