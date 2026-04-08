// V8 runtime process manager.
export { createV8Runtime, fnv1aHash } from "./runtime.js";
export type { V8Runtime, V8RuntimeOptions } from "./runtime.js";

// V8 session types.
export type {
	V8Session,
	V8SessionOptions,
	V8ExecutionOptions,
	V8ExecutionResult,
	V8ExecutionError,
	BridgeHandler,
	BridgeHandlers,
} from "./session.js";

// IPC client for communicating with the Rust V8 runtime process.
export { IpcClient } from "./ipc-client.js";
export type { IpcClientOptions, MessageHandler } from "./ipc-client.js";

// Binary frame types (active wire format).
export type { BinaryFrame, ExecutionErrorBin } from "./ipc-binary.js";
export { encodeFrame, decodeFrame, serializePayload, deserializePayload } from "./ipc-binary.js";

// Legacy IPC message types (kept for backward compatibility).
export type {
	HostMessage,
	RustMessage,
	AuthenticateMsg,
	CreateSessionMsg,
	DestroySessionMsg,
	ExecuteMsg,
	InjectGlobalsMsg,
	BridgeResponseMsg,
	StreamEventMsg,
	TerminateExecutionMsg,
	BridgeCallMsg,
	ExecutionResultMsg,
	LogMsg,
	StreamCallbackMsg,
	ProcessConfig,
	OsConfig,
} from "./ipc-types.js";
