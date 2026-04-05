export const SYNC_BRIDGE_SIGNAL_STATE_INDEX = 0;
export const SYNC_BRIDGE_SIGNAL_STATUS_INDEX = 1;
export const SYNC_BRIDGE_SIGNAL_KIND_INDEX = 2;
export const SYNC_BRIDGE_SIGNAL_LENGTH_INDEX = 3;

export const SYNC_BRIDGE_SIGNAL_STATE_IDLE = 0;
export const SYNC_BRIDGE_SIGNAL_STATE_READY = 1;

export const SYNC_BRIDGE_STATUS_OK = 0;
export const SYNC_BRIDGE_STATUS_ERROR = 1;

export const SYNC_BRIDGE_KIND_NONE = 0;
export const SYNC_BRIDGE_KIND_TEXT = 1;
export const SYNC_BRIDGE_KIND_BINARY = 2;
export const SYNC_BRIDGE_KIND_JSON = 3;

export const SYNC_BRIDGE_SIGNAL_BYTES = 4 * Int32Array.BYTES_PER_ELEMENT;
export const SYNC_BRIDGE_DEFAULT_WAIT_TIMEOUT_MS = 30_000;
export const SYNC_BRIDGE_DEFAULT_DATA_BYTES = 16 * 1024 * 1024;
export const SYNC_BRIDGE_MIN_DATA_BYTES = 64 * 1024;
export const SYNC_BRIDGE_PAYLOAD_LIMIT_ERROR_CODE =
	"ERR_SANDBOX_PAYLOAD_TOO_LARGE";

export type BrowserWorkerSyncOperation =
	| "fs.readFile"
	| "fs.writeFile"
	| "fs.readFileBinary"
	| "fs.writeFileBinary"
	| "fs.readDir"
	| "fs.createDir"
	| "fs.mkdir"
	| "fs.rmdir"
	| "fs.exists"
	| "fs.stat"
	| "fs.lstat"
	| "fs.unlink"
	| "fs.rename"
	| "fs.realpath"
	| "fs.readlink"
	| "fs.symlink"
	| "fs.link"
	| "fs.chmod"
	| "fs.truncate"
	| "module.resolve"
	| "module.loadFile";

export interface BrowserSyncBridgeBuffers {
	signalBuffer: SharedArrayBuffer;
	dataBuffer: SharedArrayBuffer;
}

export interface BrowserSyncBridgePayload extends BrowserSyncBridgeBuffers {
	timeoutMs?: number;
}

export interface BrowserWorkerSyncRequestMessage {
	type: "sync-request";
	controlToken: string;
	requestId: number;
	operation: BrowserWorkerSyncOperation;
	args: unknown[];
}

export interface BrowserSyncBridgeErrorPayload {
	message: string;
	code?: string;
}

export function assertBrowserSyncBridgeSupport(): void {
	if (typeof SharedArrayBuffer === "undefined") {
		throw new Error(
			"Browser runtime requires SharedArrayBuffer for sync filesystem and module loading parity",
		);
	}

	if (typeof Atomics === "undefined" || typeof Atomics.wait !== "function") {
		throw new Error(
			"Browser runtime requires Atomics.wait for sync filesystem and module loading parity",
		);
	}
}

export function getBrowserSyncBridgeDataBytes(payloadLimits?: {
	base64TransferBytes?: number;
	jsonPayloadBytes?: number;
}): number {
	return Math.max(
		payloadLimits?.base64TransferBytes ?? SYNC_BRIDGE_DEFAULT_DATA_BYTES,
		payloadLimits?.jsonPayloadBytes ?? 4 * 1024 * 1024,
		SYNC_BRIDGE_MIN_DATA_BYTES,
	);
}

export function createBrowserSyncBridgePayload(payloadLimits?: {
	base64TransferBytes?: number;
	jsonPayloadBytes?: number;
}): BrowserSyncBridgePayload {
	assertBrowserSyncBridgeSupport();
	return {
		signalBuffer: new SharedArrayBuffer(SYNC_BRIDGE_SIGNAL_BYTES),
		dataBuffer: new SharedArrayBuffer(
			getBrowserSyncBridgeDataBytes(payloadLimits),
		),
		timeoutMs: SYNC_BRIDGE_DEFAULT_WAIT_TIMEOUT_MS,
	};
}

export function toBrowserSyncBridgeError(
	error: unknown,
): BrowserSyncBridgeErrorPayload {
	if (error instanceof Error) {
		return {
			message: error.message,
			code:
				typeof (error as { code?: unknown }).code === "string"
					? (error as { code?: string }).code
					: undefined,
		};
	}

	return {
		message: String(error),
	};
}
