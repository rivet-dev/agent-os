import type {
	AgentExitEvent,
	CronEvent,
	CronJobInfo,
	InlineExecutionOptions,
	JavaScriptExecutionOptions,
	LanguageExecutionOptions,
	LanguageSpawnOptions,
	NpmPackageInstallOptions,
	NpmProjectInstallOptions,
	ProcessExit,
	ProcessOutput,
	PythonInstallOptions,
	SessionStreamEntry,
	ShellData,
	ShellExit,
	SpawnOptions,
	TypeScriptCheckOptions,
	TypeScriptExecutionOptions,
	TypeScriptFileExecutionOptions,
} from "@rivet-dev/agentos-core";

export type VmBootedPayload = Record<string, never>;

export interface VmShutdownPayload {
	reason: "sleep" | "destroy" | "error";
}

export type ProcessOutputPayload = ProcessOutput;
export type ProcessExitPayload = ProcessExit;
/** `seq` orders this chunk against `ShellSnapshot.seq`; it is 0 when the shell
 * has no server-side emulator and therefore no replay. */
export type ShellDataPayload = ShellData & { seq: number };
export type ShellExitPayload = ShellExit;
export type SerializableCronEvent = CronEvent;
export type ActorData =
	| { encoding: "utf8"; data: string }
	| { encoding: "base64"; data: string };

export type ActorLanguageExecutionOptions = Omit<
	LanguageExecutionOptions,
	"stdin" | "signal" | "onStdout" | "onStderr"
> & { stdin?: ActorData };
export type ActorSpawnOptions = Omit<
	SpawnOptions,
	"stdin" | "signal" | "onStdout" | "onStderr"
> & { stdin?: ActorData };
export type ActorLanguageSpawnOptions = Omit<
	LanguageSpawnOptions,
	"stdin" | "signal" | "onStdout" | "onStderr"
> & { stdin?: ActorData };
export type ActorInlineExecutionOptions = Omit<
	InlineExecutionOptions,
	"stdin" | "signal" | "onStdout" | "onStderr"
> & { stdin?: ActorData };
export type ActorJavaScriptExecutionOptions = Omit<
	JavaScriptExecutionOptions,
	"stdin" | "signal" | "onStdout" | "onStderr"
> & { stdin?: ActorData };
export type ActorTypeScriptExecutionOptions = Omit<
	TypeScriptExecutionOptions,
	"stdin" | "signal" | "onStdout" | "onStderr"
> & { stdin?: ActorData };
export type ActorTypeScriptFileExecutionOptions = Omit<
	TypeScriptFileExecutionOptions,
	"stdin" | "signal" | "onStdout" | "onStderr"
> & { stdin?: ActorData };
export type ActorTypeScriptCheckOptions = Omit<
	TypeScriptCheckOptions,
	"signal"
>;
export type ActorNpmProjectInstallOptions = Omit<
	NpmProjectInstallOptions,
	"signal"
>;
export type ActorNpmPackageInstallOptions = Omit<
	NpmPackageInstallOptions,
	"signal"
>;
export type ActorPythonInstallOptions = Omit<PythonInstallOptions, "signal">;
// --- Event schema map (used by actor() events config) ---

export interface AgentOsEvents {
	sessionEvent: SessionStreamEntry;
	agentExit: AgentExitEvent;
	vmBooted: VmBootedPayload;
	vmShutdown: VmShutdownPayload;
	processOutput: ProcessOutputPayload;
	processExit: ProcessExitPayload;
	/** Ordered PTY output containing stdout and stderr exactly once. */
	shellData: ShellDataPayload;
	/** Optional stderr-only diagnostic tap; do not render it with `shellData`. */
	shellStderr: ShellDataPayload;
	/** Shell process exit (mirrors `waitShell` resolution). */
	shellExit: ShellExitPayload;
	cronEvent: SerializableCronEvent;
}

// --- Observe-only runtime health (reply of the `health` action) ---

/** One buffered near-capacity limit warning (`RuntimeHealth.warnings`). */
export interface RuntimeLimitWarning {
	/** Epoch milliseconds when the warning was observed host-side. */
	ts: number;
	limit: string;
	category: string;
	observed: number;
	capacity: number;
	fillPercent: number;
}

/** One buffered unexpected adapter exit (`RuntimeHealth.agentExits`). */
export interface RuntimeAgentExit {
	/** Epoch milliseconds when the exit was observed host-side. */
	ts: number;
	sessionId: string;
	agentType: string;
	exitCode: number | null;
	restart: string;
	restartCount: number;
}

/**
 * One buffered adapter stderr line (`RuntimeHealth.stderrTail`). Always empty
 * today; kept in the reply shape so consumers stay stable when a stderr feed
 * lands.
 */
export interface RuntimeStderrLine {
	ts: number;
	line: string;
}

/** `RuntimeHealth.sidecar`: the non-waking sidecar descriptor subset. */
export interface RuntimeSidecarInfo {
	state: string;
	activeVmCount: number;
}

/**
 * Reply of the observe-only `health` action. Reading it never boots the VM;
 * the post-mortem buffers survive VM sleep so warnings and crash exits stay
 * readable while `booted` is false.
 */
export interface RuntimeHealth {
	booted: boolean;
	/**
	 * Count of sessions with live event subscriptions through this actor since
	 * the last wake (not total loaded sessions); `null` while the VM is down.
	 */
	sessions: number | null;
	/** Sidecar descriptor; `null` while the VM is down. */
	sidecar: RuntimeSidecarInfo | null;
	warnings: RuntimeLimitWarning[];
	agentExits: RuntimeAgentExit[];
	stderrTail: RuntimeStderrLine[];
}

/**
 * One live shell from the observe-only `listShells` action. A shell outlives
 * the page that opened it and keeps the VM awake until closed, so enumeration
 * is server-owned rather than tracked per client.
 */
export interface ShellInfo {
	shellId: string;
	/** Epoch ms when the shell was opened through this actor. */
	openedAt: number;
}

/**
 * How much of a shell's history a snapshot repaints.
 *
 * - `none` — sequence number only; the caller repaints nothing.
 * - `screen` — the visible viewport, cursor, and terminal modes.
 * - `scrollback` — the retained scrollback above the viewport, then `screen`.
 */
export type ShellReplayMode = "none" | "screen" | "scrollback";

/**
 * Server-rendered repaint for a shell that is already running. `data` is an
 * ANSI stream to write verbatim into a fresh terminal.
 */
export interface ShellSnapshot {
	shellId: string;
	mode: ShellReplayMode;
	/**
	 * Sequence of the last `shellData` chunk folded into this snapshot.
	 * Broadcasts carry the same counter, so a client applies the snapshot and
	 * then only the chunks whose `seq` is greater — no gap, no double-render.
	 */
	seq: number;
	cols: number;
	rows: number;
	data: string;
}

// --- Serializable cron action (excludes callback type) ---

export type SerializableCronAction =
	| { type: "session"; agentType: string; prompt: string; cwd?: string }
	| { type: "exec"; command: string; args?: string[] };

export interface SerializableCronJobOptions {
	id?: string;
	schedule: string;
	action: SerializableCronAction;
	overlap?: "allow" | "skip" | "queue";
}

export type SerializableCronJobInfo = CronJobInfo;
