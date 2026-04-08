export type TimingMitigation = "off" | "freeze";

export type StdioChannel = "stdout" | "stderr";

export interface StdioEvent {
	channel: StdioChannel;
	message: string;
}

export type StdioHook = (event: StdioEvent) => void;

export interface ProcessConfig {
	platform?: string;
	arch?: string;
	version?: string;
	cwd?: string;
	env?: Record<string, string>;
	argv?: string[];
	execPath?: string;
	pid?: number;
	ppid?: number;
	uid?: number;
	gid?: number;
	/** Stdin data to provide to the script */
	stdin?: string;
	/** Internal execution timing policy for bridge/process polyfills */
	timingMitigation?: TimingMitigation;
	/** Internal frozen clock source used when timing mitigation is enabled */
	frozenTimeMs?: number;
	/** Whether stdin is a TTY (PTY slave attached) */
	stdinIsTTY?: boolean;
	/** Whether stdout is a TTY (PTY slave attached) */
	stdoutIsTTY?: boolean;
	/** Whether stderr is a TTY (PTY slave attached) */
	stderrIsTTY?: boolean;
	/** Terminal columns (from PTY dimensions). */
	cols?: number;
	/** Terminal rows (from PTY dimensions). */
	rows?: number;
}

export interface OSConfig {
	platform?: string;
	arch?: string;
	type?: string;
	release?: string;
	version?: string;
	homedir?: string;
	tmpdir?: string;
	hostname?: string;
}

export interface ExecutionStatus {
	code: number;
	errorMessage?: string;
}

export interface RunResult<T = unknown> extends ExecutionStatus {
	exports?: T;
}

export interface PythonRunOptions {
	filePath?: string;
	globals?: string[];
	env?: Record<string, string>;
	cwd?: string;
	stdin?: string;
	cpuTimeLimitMs?: number;
	onStdio?: StdioHook;
}

export interface PythonRunResult<T = unknown> extends ExecutionStatus {
	value?: T;
	globals?: Record<string, unknown>;
}

export interface ExecOptions {
	filePath?: string;
	env?: Record<string, string>;
	cwd?: string;
	/** Stdin data to pass to the script */
	stdin?: string;
	/** Maximum CPU time budget in milliseconds */
	cpuTimeLimitMs?: number;
	/** Timing side-channel mitigation mode */
	timingMitigation?: TimingMitigation;
	/** Optional streaming hook for console output events */
	onStdio?: StdioHook;
	/** Override execution mode. 'run' mode processes async operations (timers, network). */
	mode?: "exec" | "run";
}

export interface ExecResult extends ExecutionStatus {}
