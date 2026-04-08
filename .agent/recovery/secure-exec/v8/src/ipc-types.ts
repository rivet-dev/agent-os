// IPC message types matching the Rust-side protocol.
// Uses internally-tagged unions (discriminated by "type" field).

/** Process configuration injected as _processConfig global. */
export interface ProcessConfig {
	platform?: string;
	arch?: string;
	version?: string;
	cwd: string;
	env: Record<string, string>;
	argv?: string[];
	execPath?: string;
	pid?: number;
	ppid?: number;
	uid?: number;
	gid?: number;
	stdin?: string;
	timing_mitigation: string;
	frozen_time_ms: number | null;
}

/** OS configuration injected as _osConfig global. */
export interface OsConfig {
	homedir: string;
	tmpdir: string;
	platform: string;
	arch: string;
}

/** Structured error from V8 execution. */
export interface ExecutionError {
	type: string;
	message: string;
	stack: string;
	code?: string;
}

// -- Host → Rust messages --

export interface AuthenticateMsg {
	type: "Authenticate";
	token: string;
}

export interface CreateSessionMsg {
	type: "CreateSession";
	session_id: string;
	heap_limit_mb?: number | null;
	cpu_time_limit_ms?: number | null;
}

export interface DestroySessionMsg {
	type: "DestroySession";
	session_id: string;
}

export interface ExecuteMsg {
	type: "Execute";
	session_id: string;
	bridge_code: string;
	user_code: string;
	file_path?: string | null;
	mode: "exec" | "run";
}

export interface InjectGlobalsMsg {
	type: "InjectGlobals";
	session_id: string;
	process_config: ProcessConfig;
	os_config: OsConfig;
}

export interface BridgeResponseMsg {
	type: "BridgeResponse";
	call_id: number;
	result: Uint8Array | null;
	error: string | null;
}

export interface StreamEventMsg {
	type: "StreamEvent";
	session_id: string;
	event_type: string;
	payload: Uint8Array;
}

export interface TerminateExecutionMsg {
	type: "TerminateExecution";
	session_id: string;
}

export type HostMessage =
	| AuthenticateMsg
	| CreateSessionMsg
	| DestroySessionMsg
	| ExecuteMsg
	| InjectGlobalsMsg
	| BridgeResponseMsg
	| StreamEventMsg
	| TerminateExecutionMsg;

// -- Rust → Host messages --

export interface BridgeCallMsg {
	type: "BridgeCall";
	call_id: number;
	session_id: string;
	method: string;
	args: Uint8Array;
}

export interface ExecutionResultMsg {
	type: "ExecutionResult";
	session_id: string;
	code: number;
	exports: Uint8Array | null;
	error: ExecutionError | null;
}

export interface LogMsg {
	type: "Log";
	session_id: string;
	channel: "stdout" | "stderr";
	message: string;
}

export interface StreamCallbackMsg {
	type: "StreamCallback";
	session_id: string;
	callback_type: string;
	payload: Uint8Array;
}

export type RustMessage =
	| BridgeCallMsg
	| ExecutionResultMsg
	| LogMsg
	| StreamCallbackMsg;
