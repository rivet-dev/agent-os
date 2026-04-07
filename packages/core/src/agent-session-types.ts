import type { JsonRpcNotification } from "./json-rpc.js";

export type SessionEventHandler = (event: JsonRpcNotification) => void;

export interface PermissionRequest {
	permissionId: string;
	description?: string;
	params: Record<string, unknown>;
}

export type PermissionReply = "once" | "always" | "reject";

export type PermissionRequestHandler = (request: PermissionRequest) => void;

export interface SessionMode {
	id: string;
	name?: string;
	label?: string;
	description?: string;
	[key: string]: unknown;
}

export interface SessionModeState {
	currentModeId: string;
	availableModes: SessionMode[];
}

export interface SessionConfigOption {
	id: string;
	category?: string;
	label?: string;
	description?: string;
	currentValue?: string;
	allowedValues?: Array<{ id: string; label?: string }>;
	readOnly?: boolean;
}

export interface PromptCapabilities {
	audio?: boolean;
	embeddedContext?: boolean;
	image?: boolean;
	[key: string]: unknown;
}

export interface AgentCapabilities {
	permissions?: boolean;
	plan_mode?: boolean;
	questions?: boolean;
	tool_calls?: boolean;
	text_messages?: boolean;
	images?: boolean;
	file_attachments?: boolean;
	session_lifecycle?: boolean;
	error_events?: boolean;
	reasoning?: boolean;
	status?: boolean;
	streaming_deltas?: boolean;
	mcp_tools?: boolean;
	promptCapabilities?: PromptCapabilities;
	[key: string]: unknown;
}

export interface AgentInfo {
	name: string;
	title?: string;
	version?: string;
	[key: string]: unknown;
}

export interface SessionInitData {
	modes?: SessionModeState;
	configOptions?: SessionConfigOption[];
	capabilities?: AgentCapabilities;
	agentInfo?: AgentInfo;
}

export interface SequencedEvent {
	sequenceNumber: number;
	notification: JsonRpcNotification;
}

export interface GetEventsOptions {
	since?: number;
	method?: string;
}
