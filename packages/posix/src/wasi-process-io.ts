/**
 * Process and FD-stat bridge interface for WASI polyfill kernel delegation.
 *
 * Abstracts process state (args, env, exit) and FD stat so the polyfill
 * does not directly touch FDTable entries for stat or hold its own
 * args/env copies. When mounted in the kernel, implementations wrap
 * KernelInterface with a bound pid. For testing, a standalone
 * implementation wraps an in-memory FDTable + options.
 */

/**
 * Process and FD-stat interface for the WASI polyfill.
 *
 * Method signatures are designed to map cleanly to KernelInterface
 * fdStat / ProcessContext when the kernel is connected.
 */
export interface WasiProcessIO {
  /** Get command-line arguments. */
  getArgs(): string[];

  /** Get environment variables. */
  getEnviron(): Record<string, string>;

  /** Get FD stat (filetype, flags, rights). */
  fdFdstatGet(fd: number): {
    errno: number;
    filetype: number;
    fdflags: number;
    rightsBase: bigint;
    rightsInheriting: bigint;
  };

  /** Set FD flags (for example O_NONBLOCK) on the backing resource. */
  fdFdstatSetFlags(fd: number, flags: number): number;

  /**
   * Record process exit. Called before the WasiProcExit exception is thrown.
   * In kernel mode this delegates to process table markExited.
   */
  procExit(exitCode: number): void;
}
