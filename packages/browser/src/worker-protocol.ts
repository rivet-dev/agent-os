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

export type SerializedPermissions = {
	fs?: string;
	network?: string;
	childProcess?: string;
	env?: string;
};

export type BrowserWorkerExecOptions = {
	filePath?: string;
	env?: Record<string, string>;
	cwd?: string;
	stdin?: string;
	timingMitigation?: TimingMitigation;
};

export type BrowserWorkerInitPayload = {
	processConfig?: ProcessConfig;
	osConfig?: OSConfig;
	permissions?: SerializedPermissions;
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
				code: string;
				filePath?: string;
				captureStdio?: boolean;
			};
	  }
	| (BrowserWorkerControlMessage & { id: number; type: "dispose" });

export type BrowserWorkerResponseMessage =
	| (BrowserWorkerControlMessage & {
			type: "response";
			id: number;
			ok: true;
			result: ExecResult | RunResult | true;
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
	requestId: number;
	channel: StdioChannel;
	message: string;
};

export type BrowserWorkerOutboundMessage =
	| BrowserWorkerResponseMessage
	| BrowserWorkerStdioMessage
	| BrowserWorkerSyncRequestMessage;
