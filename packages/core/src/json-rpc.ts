export interface AcpTimeoutErrorData {
	kind: "acp_timeout";
	method: string;
	id: number | string | null;
	timeoutMs: number;
	exitCode?: number;
	killed?: boolean;
	transportState?: string;
	recentActivity: string[];
}

export type JsonRpcErrorData = AcpTimeoutErrorData | Record<string, unknown>;

export interface JsonRpcRequest {
	jsonrpc: "2.0";
	id: number | string | null;
	method: string;
	params?: unknown;
}

export interface JsonRpcResponse {
	jsonrpc: "2.0";
	id: number | string | null;
	result?: unknown;
	error?: JsonRpcError;
}

export interface JsonRpcError {
	code: number;
	message: string;
	data?: JsonRpcErrorData;
}

export interface JsonRpcNotification {
	jsonrpc: "2.0";
	method: string;
	params?: unknown;
}

export function isAcpTimeoutErrorData(
	value: unknown,
): value is AcpTimeoutErrorData {
	if (!value || typeof value !== "object" || Array.isArray(value)) {
		return false;
	}
	const record = value as Record<string, unknown>;
	return (
		record.kind === "acp_timeout" &&
		typeof record.method === "string" &&
		(typeof record.id === "number" ||
			typeof record.id === "string" ||
			record.id === null) &&
		typeof record.timeoutMs === "number" &&
		Array.isArray(record.recentActivity) &&
		record.recentActivity.every((entry) => typeof entry === "string")
	);
}
