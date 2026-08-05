import type {
	ExecResult,
	OSConfig,
	ProcessConfig,
	RunResult,
	StdioChannel,
	TimingMitigation,
} from "./runtime.js";
import type {
	BrowserSyncBridgePayload,
	BrowserWorkerSyncRequestMessage,
} from "./sync-bridge.js";

export type BrowserWorkerExecOptions = {
	filePath?: string;
	env?: Record<string, string>;
	cwd?: string;
	stdin?: string;
	stdioPty?: {
		open?: boolean;
		slaveFd?: number;
		columns?: number;
		rows?: number;
	};
	timingMitigation?: TimingMitigation;
	persistent?: boolean;
	streamingStdin?: boolean;
};

export type BrowserWorkerExtensionRequestPayload = {
	namespace: string;
	payload: Uint8Array;
};

export type BrowserWorkerExtensionResponse = {
	namespace: string;
	payload: Uint8Array;
};

export type BrowserWorkerInitPayload = {
	processConfig?: ProcessConfig;
	processLimits?: {
		maxSpawnFileActions?: number;
		maxSpawnFileActionBytes?: number;
	};
	osConfig?: OSConfig;
	filesystem?: "opfs" | "memory";
	networkEnabled?: boolean;
	timingMitigation?: TimingMitigation;
	payloadLimits?: {
		base64TransferBytes?: number;
		jsonPayloadBytes?: number;
	};
	syncBridge?: BrowserSyncBridgePayload;
};

type BrowserWorkerControlMessage = {
	controlToken: string;
};

export type BrowserWorkerRequestMessage =
	| (BrowserWorkerControlMessage & {
			id: number;
			type: "init";
			payload: BrowserWorkerInitPayload;
	  })
	| {
			controlToken: string;
			id: number;
			type: "exec";
			payload: {
				executionId: string;
				code: string;
				options?: BrowserWorkerExecOptions;
				captureStdio?: boolean;
			};
	  }
	| {
			controlToken: string;
			id: number;
			type: "run";
			payload: {
				executionId: string;
				code: string;
				filePath?: string;
				captureStdio?: boolean;
			};
	  }
	| {
			controlToken: string;
			id: number;
			type: "signal";
			payload: {
				executionId: string;
				signal: number;
			};
	  }
	| (BrowserWorkerControlMessage & {
			id: number;
			type: "extension";
			payload: BrowserWorkerExtensionRequestPayload;
	  })
	| (BrowserWorkerControlMessage & {
			id: number;
			type: "write-stdin";
			executionId: string;
			data: string;
	  })
	| (BrowserWorkerControlMessage & {
			id: number;
			type: "end-stdin";
			executionId: string;
	  })
	| (BrowserWorkerControlMessage & {
			id: number;
			type: "resize-pty";
			executionId: string;
			columns: number;
			rows: number;
	  })
	| (BrowserWorkerControlMessage & { id: number; type: "dispose" });

export type BrowserWorkerResponseMessage =
	| (BrowserWorkerControlMessage & {
			type: "response";
			id: number;
			ok: true;
			result: ExecResult | RunResult | BrowserWorkerExtensionResponse | true;
	  })
	| {
			controlToken: string;
			type: "response";
			id: number;
			ok: false;
			error: { message: string; stack?: string; code?: string };
	  };

export type BrowserWorkerStdioMessage = BrowserWorkerControlMessage & {
	type: "stdio";
	executionId: string;
	requestId: number;
	channel: StdioChannel;
	message: string;
};

export type BrowserWorkerPtyOpenedMessage = BrowserWorkerControlMessage & {
	type: "pty-opened";
	executionId: string;
	requestId: number;
	masterFd: number;
	slaveFd: number;
	path?: string;
	columns: number;
	rows: number;
};

export type BrowserWorkerOutboundMessage =
	| BrowserWorkerResponseMessage
	| BrowserWorkerStdioMessage
	| BrowserWorkerPtyOpenedMessage
	| BrowserWorkerSyncRequestMessage;
