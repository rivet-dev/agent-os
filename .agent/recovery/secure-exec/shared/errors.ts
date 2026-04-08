/** Node-compatible system error shape with code, errno, path, and syscall. */
export interface SystemError extends Error {
	code?: string;
	errno?: number | string;
	path?: string;
	syscall?: string;
}

/** Build a system error with the given POSIX error code (ENOENT, EACCES, etc.). */
export function createSystemError(
	code: string,
	message: string,
	details?: {
		path?: string;
		syscall?: string;
	},
): SystemError {
	const err = new Error(message) as SystemError;
	err.code = code;
	if (details?.path) err.path = details.path;
	if (details?.syscall) err.syscall = details.syscall;
	return err;
}

/** Create a permission-denied error matching Node's EACCES format. */
export function createEaccesError(
	op: string,
	path?: string,
	reason?: string,
): SystemError {
	const suffix = path ? ` '${path}'` : "";
	const reasonSuffix = reason ? `: ${reason}` : "";
	return createSystemError(
		"EACCES",
		`EACCES: permission denied, ${op}${suffix}${reasonSuffix}`,
		{ path, syscall: op },
	);
}

/** Create a "function not implemented" error for unsupported operations. */
export function createEnosysError(op: string, path?: string): SystemError {
	const suffix = path ? ` '${path}'` : "";
	return createSystemError(
		"ENOSYS",
		`ENOSYS: function not implemented, ${op}${suffix}`,
		{ path, syscall: op },
	);
}
