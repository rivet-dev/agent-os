import type { ManagedProcess } from "./runtime-compat.js";
import {
	deserializeMessage,
	isRequest,
	isResponse,
	type JsonRpcError,
	type JsonRpcRequest,
	type JsonRpcNotification,
	type JsonRpcResponse,
	serializeMessage,
} from "./protocol.js";

const DEFAULT_TIMEOUT_MS = 120_000;
const EXIT_DRAIN_GRACE_MS = 50;
const LEGACY_PERMISSION_METHOD = "request/permission";
const ACP_PERMISSION_METHOD = "session/request_permission";
const ACP_CANCEL_METHOD = "session/cancel";
const RECENT_ACTIVITY_LIMIT = 20;
const ACTIVITY_TEXT_LIMIT = 240;

interface PendingRequest {
	resolve: (response: JsonRpcResponse) => void;
	reject: (error: Error) => void;
	timer: ReturnType<typeof setTimeout>;
}

interface PendingPermissionRequest {
	id: number | string | null;
	method: string;
	options?: Array<Record<string, unknown>>;
}

export type NotificationHandler = (notification: JsonRpcNotification) => void;
export type InboundRequestHandler = (
	request: JsonRpcRequest,
) =>
	| Promise<
			| {
					result?: unknown;
					error?: JsonRpcError;
			  }
			| null
			| undefined
	  >
	| {
			result?: unknown;
			error?: JsonRpcError;
	  }
	| null
	| undefined;

export class AcpClient {
	private _process: ManagedProcess;
	private _nextId = 1;
	private _pending = new Map<number | string | null, PendingRequest>();
	private _seenInboundRequestIds = new Set<number | string | null>();
	private _notificationHandlers: NotificationHandler[] = [];
	private _closed = false;
	private _timeoutMs: number;
	private _stdoutIterator: AsyncIterator<string> | null = null;
	private _readerClosed = false;
	private _pendingPermissionRequests = new Map<string, PendingPermissionRequest>();
	private _requestHandler?: InboundRequestHandler;
	private _recentActivity: string[] = [];

	constructor(
		process: ManagedProcess,
		stdoutLines: AsyncIterable<string>,
		options?: {
			timeoutMs?: number;
			requestHandler?: InboundRequestHandler;
		},
	) {
		this._process = process;
		this._timeoutMs = options?.timeoutMs ?? DEFAULT_TIMEOUT_MS;
		this._requestHandler = options?.requestHandler;
		this._startReading(stdoutLines);
		this._watchExit();
	}

	request(method: string, params?: unknown): Promise<JsonRpcResponse> {
		if (this._closed) {
			return Promise.reject(new Error("AcpClient is closed"));
		}

		const compatibilityResponse = this._maybeHandlePermissionResponse(
			method,
			params,
		);
		if (compatibilityResponse) {
			return compatibilityResponse;
		}

		const id = this._nextId++;
		const msg = serializeMessage({ jsonrpc: "2.0", id, method, params });
		this._recordActivity(`sent request ${method} id=${String(id)}`);
		this._process.writeStdin(msg);

		const responsePromise = new Promise<JsonRpcResponse>((resolve, reject) => {
			const timer = setTimeout(() => {
				this._pending.delete(id);
				reject(this._createTimeoutError(method, id));
			}, this._timeoutMs);

			this._pending.set(id, { resolve, reject, timer });
		});

		if (method !== ACP_CANCEL_METHOD) {
			return responsePromise;
		}

		return responsePromise.then((response) => {
			if (this._isCancelMethodNotFound(response)) {
				this.notify(method, params);
				return {
					jsonrpc: "2.0",
					id: response.id,
					result: {
						cancelled: false,
						requested: true,
						via: "notification-fallback",
					},
				};
			}
			return response;
		});
	}

	notify(method: string, params?: unknown): void {
		if (this._closed) return;
		const msg = serializeMessage({ jsonrpc: "2.0", method, params });
		this._recordActivity(`sent notification ${method}`);
		this._process.writeStdin(msg);
	}

	onNotification(handler: NotificationHandler): void {
		this._notificationHandlers.push(handler);
	}

	close(): void {
		if (this._closed) return;
		this._closed = true;
		this._closeReader();
		this._rejectAll(new Error("AcpClient closed"));
		// Hard-close the agent process. Sending a graceful stdin-close first can
		// leave a hanging sidecar close_stdin RPC behind when the process is being
		// torn down anyway, which makes session disposal flaky under test load.
		this._process.kill(9);
	}

	private _startReading(stdoutLines: AsyncIterable<string>): void {
		void (async () => {
			const iterator = stdoutLines[Symbol.asyncIterator]();
			this._stdoutIterator = iterator;
			try {
				while (!this._closed) {
					const { value: line, done } = await iterator.next();
					if (done) {
						break;
					}
					if (this._closed) break;
					const trimmed = line.trim();
					if (!trimmed) continue;

					const msg = deserializeMessage(trimmed);
					if (!msg) {
						this._recordActivity(`non_json ${truncateActivityText(trimmed)}`);
						continue; // Skip non-JSON lines
					}
					this._recordActivity(summarizeInboundMessage(msg));

					if (isResponse(msg)) {
						const pending = this._pending.get(msg.id);
						if (pending) {
							this._pending.delete(msg.id);
							clearTimeout(pending.timer);
							pending.resolve(msg);
						}
					} else if (isRequest(msg)) {
						this._handleRequest(msg);
					} else {
						for (const handler of this._notificationHandlers) {
							handler(msg);
						}
					}
				}
			} catch {
				// Stream ended or errored
			} finally {
				if (this._stdoutIterator === iterator) {
					this._stdoutIterator = null;
				}
			}
		})();
	}

	private _watchExit(): void {
		this._process.wait().then(() => {
			this._recordActivity(
				`process_exit exitCode=${String(this._getProcessExitCode())} killed=${String(this._getProcessKilled())}`,
			);
			setTimeout(() => {
				if (this._closed) {
					return;
				}
				this._closed = true;
				this._closeReader();
				this._rejectAll(new Error("Agent process exited"));
			}, EXIT_DRAIN_GRACE_MS);
		});
	}

	private _rejectAll(error: Error): void {
		for (const [id, pending] of this._pending) {
			clearTimeout(pending.timer);
			pending.reject(error);
			this._pending.delete(id);
		}
		this._pendingPermissionRequests.clear();
		this._seenInboundRequestIds.clear();
	}

	private _closeReader(): void {
		if (this._readerClosed) {
			return;
		}
		this._readerClosed = true;
		const iterator = this._stdoutIterator;
		this._stdoutIterator = null;
		if (iterator && typeof iterator.return === "function") {
			void iterator.return();
		}
	}

	private _handleRequest(msg: JsonRpcRequest): void {
		// VM stdout can duplicate NDJSON lines. Requests are stateful, so repeated
		// inbound IDs must be ignored to avoid double-handling permission prompts.
		if (this._seenInboundRequestIds.has(msg.id)) {
			return;
		}
		this._seenInboundRequestIds.add(msg.id);

		if (msg.method === ACP_PERMISSION_METHOD) {
			const requestParams = this._toRecord(msg.params);
			const permissionId = String(msg.id);
			this._pendingPermissionRequests.set(permissionId, {
				id: msg.id,
				method: msg.method,
				options: Array.isArray(requestParams.options)
					? requestParams.options
							.filter(
								(option): option is Record<string, unknown> =>
									option !== null && typeof option === "object",
							)
					: undefined,
			});
			const params = {
				...requestParams,
				permissionId,
				_acpMethod: msg.method,
			};
			for (const handler of this._notificationHandlers) {
				handler({
					jsonrpc: "2.0",
					method: LEGACY_PERMISSION_METHOD,
					params,
				});
			}
			return;
		}

		const params = {
			...this._toRecord(msg.params),
			requestId: msg.id,
		};
		for (const handler of this._notificationHandlers) {
			handler({
				jsonrpc: "2.0",
				method: msg.method,
				params,
			});
		}

		if (!this._requestHandler) {
			this._process.writeStdin(
				serializeMessage({
					jsonrpc: "2.0",
					id: msg.id,
					error: {
						code: -32601,
						message: `Method not found: ${msg.method}`,
					},
				}),
			);
			return;
		}

		void this._handleInboundRequest(msg);
	}

	private async _handleInboundRequest(msg: JsonRpcRequest): Promise<void> {
		try {
			const handled = await this._requestHandler?.(msg);
			if (!handled) {
				this._process.writeStdin(
					serializeMessage({
						jsonrpc: "2.0",
						id: msg.id,
						error: {
							code: -32601,
							message: `Method not found: ${msg.method}`,
						},
					}),
				);
				return;
			}

			if (handled.error) {
				this._process.writeStdin(
					serializeMessage({
						jsonrpc: "2.0",
						id: msg.id,
						error: handled.error,
					}),
				);
				return;
			}

			this._process.writeStdin(
				serializeMessage({
					jsonrpc: "2.0",
					id: msg.id,
					result: handled.result ?? null,
				}),
			);
		} catch (error) {
			this._process.writeStdin(
				serializeMessage({
					jsonrpc: "2.0",
					id: msg.id,
					error: {
						code: -32000,
						message: error instanceof Error ? error.message : String(error),
					},
				}),
			);
		}
	}

	private _recordActivity(entry: string): void {
		this._recentActivity.push(entry);
		if (this._recentActivity.length > RECENT_ACTIVITY_LIMIT) {
			this._recentActivity.splice(
				0,
				this._recentActivity.length - RECENT_ACTIVITY_LIMIT,
			);
		}
	}

	private _createTimeoutError(method: string, id: number | string): Error {
		const processState = `process exitCode=${String(
			this._getProcessExitCode(),
		)} killed=${String(this._getProcessKilled())}`;
		const activity =
			this._recentActivity.length > 0
				? this._recentActivity.join(" | ")
				: "no recent ACP activity";
		return new Error(
			`ACP request ${method} (id=${id}) timed out after ${this._timeoutMs}ms. ${processState}. Recent ACP activity: ${activity}`,
		);
	}

	private _getProcessExitCode(): number | null | undefined {
		return (
			this._process as {
				exitCode?: number | null;
			}
		).exitCode;
	}

	private _getProcessKilled(): boolean | undefined {
		return (
			this._process as {
				killed?: boolean;
			}
		).killed;
	}

	private _maybeHandlePermissionResponse(
		method: string,
		params: unknown,
	): Promise<JsonRpcResponse> | null {
		if (
			method !== LEGACY_PERMISSION_METHOD &&
			method !== ACP_PERMISSION_METHOD
		) {
			return null;
		}

		const payload = this._toRecord(params);
		const permissionIdValue = payload.permissionId;
		if (
			typeof permissionIdValue !== "string" &&
			typeof permissionIdValue !== "number"
		) {
			return null;
		}

		const permissionId = String(permissionIdValue);
		const pending = this._pendingPermissionRequests.get(permissionId);
		if (!pending || pending.method !== ACP_PERMISSION_METHOD) {
			return null;
		}

		this._pendingPermissionRequests.delete(permissionId);
		const result = this._normalizePermissionResult(payload, pending);
		this._process.writeStdin(
			serializeMessage({
				jsonrpc: "2.0",
				id: pending.id,
				result,
			}),
		);
		return Promise.resolve({
			jsonrpc: "2.0",
			id: pending.id,
			result,
		});
	}

	private _normalizePermissionResult(
		params: Record<string, unknown>,
		pending: PendingPermissionRequest,
	): Record<string, unknown> {
		const outcome = params.outcome;
		if (outcome && typeof outcome === "object") {
			return { outcome };
		}

		const requestedReply = params.reply;
		const selectedOptionId = this._resolvePermissionOptionId(
			pending.options,
			typeof requestedReply === "string" ? requestedReply : undefined,
		);
		if (selectedOptionId) {
			return {
				outcome: { outcome: "selected", optionId: selectedOptionId },
			};
		}

		switch (params.reply) {
			case "always":
				return {
					outcome: { outcome: "selected", optionId: "allow_always" },
				};
			case "once":
				return {
					outcome: { outcome: "selected", optionId: "allow_once" },
				};
			case "reject":
				return {
					outcome: { outcome: "selected", optionId: "reject_once" },
				};
			default:
				return {
					outcome: { outcome: "cancelled" },
				};
		}
	}

	private _resolvePermissionOptionId(
		options: Array<Record<string, unknown>> | undefined,
		reply: string | undefined,
	): string | null {
		if (!options || !reply) {
			return null;
		}

		const targets = (() => {
			switch (reply) {
				case "always":
					return {
						optionIds: ["always", "allow_always"],
						kinds: ["allow_always"],
					};
				case "once":
					return {
						optionIds: ["once", "allow_once"],
						kinds: ["allow_once"],
					};
				case "reject":
					return {
						optionIds: ["reject", "reject_once"],
						kinds: ["reject_once"],
					};
				default:
					return null;
			}
		})();

		if (!targets) {
			return null;
		}

		const match = options.find((option) => {
			const optionId = option.optionId;
			const kind = option.kind;
			return (
				(typeof optionId === "string" &&
					targets.optionIds.includes(optionId)) ||
				(typeof kind === "string" && targets.kinds.includes(kind))
			);
		});

		return typeof match?.optionId === "string" ? match.optionId : null;
	}

	private _isCancelMethodNotFound(response: JsonRpcResponse): boolean {
		if (response.error?.code !== -32601) {
			return false;
		}

		const methodFromData = this._toRecord(response.error.data).method;
		if (methodFromData === ACP_CANCEL_METHOD) {
			return true;
		}

		return response.error.message.includes(ACP_CANCEL_METHOD);
	}

	private _toRecord(value: unknown): Record<string, unknown> {
		return value && typeof value === "object"
			? (value as Record<string, unknown>)
			: {};
	}
}

function truncateActivityText(value: string): string {
	if (value.length <= ACTIVITY_TEXT_LIMIT) {
		return value;
	}
	return `${value.slice(0, ACTIVITY_TEXT_LIMIT)}...`;
}

function summarizeInboundMessage(
	msg: JsonRpcResponse | JsonRpcRequest | JsonRpcNotification,
): string {
	if (isResponse(msg)) {
		if (msg.error) {
			return truncateActivityText(
				`received response id=${String(msg.id)} error=${msg.error.code}:${msg.error.message}`,
			);
		}
		return `received response id=${String(msg.id)}`;
	}

	if (isRequest(msg)) {
		return truncateActivityText(
			`received request ${msg.method} id=${String(msg.id)}`,
		);
	}

	return truncateActivityText(`received notification ${msg.method}`);
}
