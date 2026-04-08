/**
 * @secure-exec/kernel
 *
 * OS kernel providing VFS, FD table, process table, device layer,
 * pipes, command registry, and permissions. All runtimes share the
 * same kernel instance.
 */

// Kernel factory
export { createKernel } from "./kernel.js";

// Types
export type {
	FsMount,
	Kernel,
	KernelOptions,
	KernelInterface,
	KernelLogger,
	ExecOptions,
	ExecResult,
	SpawnOptions,
	ManagedProcess,
	RuntimeDriver,
	ProcessContext,
	DriverProcess,
	ProcessEntry,
	ProcessInfo,
	FDStat,
	FileDescription,
	FDEntry,
	Pipe,
	Permissions,
	PermissionDecision,
	PermissionCheck,
	FsAccessRequest,
	NetworkAccessRequest,
	ChildProcessAccessRequest,
	EnvAccessRequest,
	KernelErrorCode,
	SignalDisposition,
	SignalHandler,
	ProcessSignalState,
	Termios,
	TermiosCC,
	OpenShellOptions,
	ShellHandle,
	ConnectTerminalOptions,
} from "./types.js";

// Structured kernel error, termios defaults, and no-op logger
export { KernelError, defaultTermios, noopKernelLogger } from "./types.js";

// VFS types
export type {
	VirtualFileSystem,
	VirtualDirEntry,
	VirtualStat,
} from "./vfs.js";

// Kernel components (for direct use / testing)
export { FDTableManager, ProcessFDTable } from "./fd-table.js";
export { ProcessTable } from "./process-table.js";
export { createDeviceLayer } from "./device-layer.js";
export {
	createProcLayer,
	createProcessScopedFileSystem,
	resolveProcSelfPath,
} from "./proc-layer.js";
export { createProcBackend } from "./proc-backend.js";
export type { ProcBackendOptions } from "./proc-backend.js";
export { PipeManager } from "./pipe-manager.js";
export { PtyManager } from "./pty.js";
export type { LineDisciplineConfig } from "./pty.js";
export { CommandRegistry } from "./command-registry.js";
export { FileLockManager, LOCK_SH, LOCK_EX, LOCK_UN, LOCK_NB } from "./file-lock.js";
export { WaitHandle, WaitQueue } from "./wait.js";
export { TimerTable } from "./timer-table.js";
export type { KernelTimer, TimerTableOptions } from "./timer-table.js";
export { DnsCache } from "./dns-cache.js";
export type { DnsCacheOptions } from "./dns-cache.js";
export { UserManager } from "./user.js";
export type { UserConfig } from "./user.js";

// Socket table
export { SocketTable } from "./socket-table.js";
export type {
	KernelSocket,
	SocketState,
	SockAddr,
	InetAddr,
	UnixAddr,
	UdpDatagram,
} from "./socket-table.js";
export {
	AF_INET, AF_INET6, AF_UNIX,
	SOCK_STREAM, SOCK_DGRAM,
	SOL_SOCKET, IPPROTO_TCP,
	SO_REUSEADDR, SO_KEEPALIVE, SO_RCVBUF, SO_SNDBUF,
	TCP_NODELAY,
	MSG_PEEK, MSG_DONTWAIT, MSG_NOSIGNAL,
	MAX_DATAGRAM_SIZE, MAX_UDP_QUEUE_DEPTH,
	S_IFSOCK,
	isInetAddr, isUnixAddr, addrKey, optKey,
} from "./socket-table.js";

// Host adapter interfaces (for kernel network delegation)
export type {
	HostNetworkAdapter,
	HostSocket,
	HostListener,
	HostUdpSocket,
	DnsResult,
} from "./host-adapter.js";

// Permissions
export {
	wrapFileSystem,
	filterEnv,
	checkChildProcess,
	allowAll,
	allowAllFs,
	allowAllNetwork,
	allowAllChildProcess,
	allowAllEnv,
} from "./permissions.js";

// Constants
export {
	O_RDONLY, O_WRONLY, O_RDWR, O_CREAT, O_EXCL, O_TRUNC, O_APPEND, O_CLOEXEC,
	F_DUPFD, F_GETFD, F_SETFD, F_GETFL, F_DUPFD_CLOEXEC, FD_CLOEXEC,
	SEEK_SET, SEEK_CUR, SEEK_END,
	FILETYPE_UNKNOWN, FILETYPE_CHARACTER_DEVICE, FILETYPE_DIRECTORY,
	FILETYPE_REGULAR_FILE, FILETYPE_SYMBOLIC_LINK, FILETYPE_PIPE,
	SIGHUP, SIGINT, SIGQUIT, SIGKILL, SIGPIPE, SIGALRM, SIGTERM, SIGCHLD, SIGCONT, SIGSTOP, SIGTSTP, SIGWINCH,
	SA_RESTART, SA_RESETHAND, SA_NOCLDSTOP,
	SIG_BLOCK, SIG_UNBLOCK, SIG_SETMASK,
	WNOHANG,
} from "./types.js";

// POSIX wstatus encoding/decoding
export {
	encodeExitStatus, encodeSignalStatus,
	WIFEXITED, WEXITSTATUS, WIFSIGNALED, WTERMSIG,
} from "./wstatus.js";
