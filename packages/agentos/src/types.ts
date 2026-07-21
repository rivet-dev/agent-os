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
export type ShellDataPayload = ShellData;
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
