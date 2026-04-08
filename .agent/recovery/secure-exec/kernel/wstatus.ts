/**
 * POSIX wstatus encoding/decoding.
 *
 * Encodes how a process terminated into a single integer matching
 * the layout expected by WIFEXITED / WEXITSTATUS / WIFSIGNALED / WTERMSIG.
 *
 *   Normal exit:  (exitCode & 0xFF) << 8  (bits 8-15 = exit code, bits 0-6 = 0)
 *   Signal death: signalNumber & 0x7F     (bits 0-6 = signal, bits 8-15 = 0)
 */

/** Encode a normal exit into POSIX wstatus. */
export function encodeExitStatus(exitCode: number): number {
	return (exitCode & 0xff) << 8;
}

/** Encode a signal death into POSIX wstatus. */
export function encodeSignalStatus(signal: number): number {
	return signal & 0x7f;
}

/** True if process exited normally (not killed by a signal). */
export function WIFEXITED(status: number): boolean {
	return (status & 0x7f) === 0;
}

/** Extract exit code (only valid when WIFEXITED is true). */
export function WEXITSTATUS(status: number): number {
	return (status >> 8) & 0xff;
}

/** True if process was killed by a signal. */
export function WIFSIGNALED(status: number): boolean {
	return (status & 0x7f) !== 0;
}

/** Extract signal number (only valid when WIFSIGNALED is true). */
export function WTERMSIG(status: number): number {
	return status & 0x7f;
}
