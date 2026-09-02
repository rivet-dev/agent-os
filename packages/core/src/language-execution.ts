import type { JsonValue } from "./session-api.js";

export type ExecutionSignal =
	| "SIGHUP"
	| "SIGINT"
	| "SIGQUIT"
	| "SIGTERM"
	| "SIGKILL"
	| "SIGSTOP"
	| "SIGCONT"
	| "SIGUSR1"
	| "SIGUSR2";

export interface LanguageExecutionOptions {
	contextId?: string;
	cwd?: string;
	env?: Record<string, string>;
	args?: string[];
	stdin?: string | Uint8Array;
	timeoutMs?: number;
	pty?: { cols?: number; rows?: number };
	signal?: AbortSignal;
	onStdout?: (chunk: Uint8Array) => void;
	onStderr?: (chunk: Uint8Array) => void;
	output?: ExecutionOutputOptions;
}

export type OutputCapture = "none" | "stderr" | "all";

export interface ExecutionOutputOptions {
	capture?: OutputCapture;
	retainEvents?: boolean;
}

export interface InlineExecutionOptions extends LanguageExecutionOptions {
	inputs?: Record<string, JsonValue>;
}

export interface JavaScriptExecutionOptions extends InlineExecutionOptions {
	/**
	 * `module` (the default) evaluates each call as an independent root ES module.
	 * A retained context preserves `globalThis`, not the module's lexical scope.
	 * Use `commonjs` for REPL-style top-level bindings shared by later calls.
	 */
	format?: "module" | "commonjs";
	filePath?: string;
}

export type JavaScriptEvaluationOptions = JavaScriptExecutionOptions;

export interface TypeScriptExecutionOptions extends InlineExecutionOptions {
	filePath?: string;
	tsconfigPath?: string;
	compilerOptions?: Record<string, JsonValue>;
}

export type TypeScriptEvaluationOptions = TypeScriptExecutionOptions;

export interface TypeScriptFileExecutionOptions
	extends LanguageExecutionOptions {
	tsconfigPath?: string;
	compilerOptions?: Record<string, JsonValue>;
}

export interface TypeScriptCheckOptions {
	contextId?: string;
	cwd?: string;
	filePath?: string;
	tsconfigPath?: string;
	compilerOptions?: Record<string, JsonValue>;
	timeoutMs?: number;
	signal?: AbortSignal;
	output?: ExecutionOutputOptions;
}

export interface NpmProjectInstallOptions
	extends Omit<LanguageExecutionOptions, "args" | "stdin"> {
	frozen?: boolean;
}

export interface NpmPackageInstallOptions
	extends Omit<LanguageExecutionOptions, "args" | "stdin"> {
	dev?: boolean;
	global?: boolean;
}

export interface PythonInstallOptions
	extends Omit<LanguageExecutionOptions, "args" | "stdin"> {
	upgrade?: boolean;
	requirementsFile?: string;
	indexUrl?: string;
	extraIndexUrls?: string[];
}

export type ContextState =
	| "idle"
	| "running"
	| "resetting"
	| "deleting"
	| "failed";
export type ExecutionOutcome =
	| "succeeded"
	| "failed"
	| "cancelled"
	| "timed_out";

export interface ContextDescriptor {
	contextId: string;
	state: ContextState;
	language?: "javascript" | "python";
	createdAtMs: number;
	lastStartedAtMs?: number;
	lastCompletedAtMs?: number;
}

export interface ProcessDescriptor {
	pid: number;
	state: "running" | "exited";
	language?: "javascript" | "python";
	command?: string;
	startedAtMs: number;
}

export interface ProcessExit {
	pid: number;
	outcome: "exited" | "signalled" | "timed_out";
	exitCode?: number;
	signal?: ExecutionSignal;
}

export interface SpawnOptions {
	cwd?: string;
	env?: Record<string, string>;
	args?: string[];
	stdin?: string | Uint8Array;
	timeoutMs?: number;
	pty?: { cols?: number; rows?: number };
	signal?: AbortSignal;
	onStdout?: (chunk: Uint8Array) => void;
	onStderr?: (chunk: Uint8Array) => void;
	output?: { retainEvents?: boolean };
}

export type LanguageSpawnOptions = SpawnOptions;

export interface ExecutionErrorData {
	code: string;
	name: string;
	message: string;
	stack?: string;
	details?: JsonValue;
}

interface CodeExecutionResultBase {
	exitCode?: number;
	stdout?: string;
	stderr?: string;
	stdoutTruncated?: boolean;
	stderrTruncated?: boolean;
}

type ExecutionResultOutcome<TIdentity> =
	| (CodeExecutionResultBase & {
			outcome: "succeeded";
			error?: never;
	  } & TIdentity)
	| (CodeExecutionResultBase & {
			outcome: Exclude<ExecutionOutcome, "succeeded">;
			error: ExecutionErrorData;
	  } & TIdentity);

export type CodeExecutionResult = ExecutionResultOutcome<Record<never, never>>;

export type CodeEvaluationResult<T = JsonValue> =
	| (CodeExecutionResultBase & {
			outcome: "succeeded";
			error?: never;
			value: T;
	  })
	| (CodeExecutionResultBase & {
			outcome: Exclude<ExecutionOutcome, "succeeded">;
			error: ExecutionErrorData;
			value?: never;
	  });

export interface TypeScriptDiagnostic {
	code: number;
	category: "error" | "warning" | "suggestion" | "message";
	message: string;
	filePath?: string;
	line?: number;
	column?: number;
}

export type TypeScriptCheckResult =
	| (CodeExecutionResultBase & {
			outcome: "succeeded";
			error?: never;
			hasErrors: boolean;
			diagnostics: TypeScriptDiagnostic[];
	  })
	| (CodeExecutionResultBase & {
			outcome: Exclude<ExecutionOutcome, "succeeded">;
			error: ExecutionErrorData;
			hasErrors?: never;
			diagnostics: TypeScriptDiagnostic[];
	  });

export interface ProcessOutputEvent<TChunk = Uint8Array> {
	pid: number;
	sequence: number;
	channel: "stdout" | "stderr" | "pty";
	chunk: TChunk;
	timestampMs: number;
}

export interface OutputReplay<TChunk = Uint8Array> {
	pid: number;
	events: ProcessOutputEvent<TChunk>[];
	nextCursor: string;
	hasMore: boolean;
	truncated: boolean;
}
