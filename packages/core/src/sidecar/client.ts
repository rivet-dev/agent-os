import { randomUUID } from "node:crypto";

export type AgentOsSidecarPlacement =
	| { kind: "shared"; pool?: string }
	| { kind: "explicit"; sidecarId: string };

export type AgentOsSidecarSessionState =
	| "connecting"
	| "ready"
	| "disposing"
	| "disposed"
	| "failed";

export type AgentOsSidecarVmState =
	| "creating"
	| "ready"
	| "disposing"
	| "disposed"
	| "failed";

export interface AgentOsSidecarSessionLifecycle {
	sessionId: string;
	placement: AgentOsSidecarPlacement;
	state: AgentOsSidecarSessionState;
	createdAt: number;
	connectedAt?: number;
	disposedAt?: number;
	lastError?: string;
	metadata: Record<string, string>;
	vmIds: string[];
}

export interface AgentOsSidecarVmLifecycle {
	vmId: string;
	sessionId: string;
	state: AgentOsSidecarVmState;
	createdAt: number;
	readyAt?: number;
	disposedAt?: number;
	lastError?: string;
	metadata: Record<string, string>;
}

export interface AgentOsSidecarSessionOptions {
	placement?: AgentOsSidecarPlacement;
	metadata?: Record<string, string>;
	signal?: AbortSignal;
}

export interface AgentOsSidecarVmOptions {
	metadata?: Record<string, string>;
}

export interface AgentOsSidecarSessionBootstrap {
	sessionId: string;
	placement: AgentOsSidecarPlacement;
	metadata: Record<string, string>;
	signal?: AbortSignal;
}

export interface AgentOsSidecarVmBootstrap {
	vmId: string;
	sessionId: string;
	metadata: Record<string, string>;
}

export interface AgentOsSidecarTransport {
	createVm?(bootstrap: AgentOsSidecarVmBootstrap): Promise<void>;
	disposeVm?(vmId: string): Promise<void>;
	dispose(): Promise<void>;
}

export interface AgentOsSidecarClientOptions {
	createSessionTransport(
		bootstrap: AgentOsSidecarSessionBootstrap,
	): Promise<AgentOsSidecarTransport>;
	createId?: () => string;
	now?: () => number;
}

interface AgentOsSidecarVmEntry {
	lifecycle: AgentOsSidecarVmLifecycle;
}

interface AgentOsSidecarSessionEntry {
	lifecycle: AgentOsSidecarSessionLifecycle;
	transport?: AgentOsSidecarTransport;
	vms: Map<string, AgentOsSidecarVmEntry>;
}

export class AgentOsSidecarVmHandle {
	constructor(
		private readonly client: AgentOsSidecarClient,
		readonly sessionId: string,
		readonly vmId: string,
	) {}

	describe(): AgentOsSidecarVmLifecycle {
		return this.client.requireVmLifecycle(this.sessionId, this.vmId);
	}

	async dispose(): Promise<void> {
		await this.client.disposeVm(this.sessionId, this.vmId);
	}
}

export class AgentOsSidecarSessionHandle {
	constructor(
		private readonly client: AgentOsSidecarClient,
		readonly sessionId: string,
	) {}

	describe(): AgentOsSidecarSessionLifecycle {
		return this.client.requireSessionLifecycle(this.sessionId);
	}

	listVms(): AgentOsSidecarVmLifecycle[] {
		return this.client.listVms(this.sessionId);
	}

	async createVm(
		options?: AgentOsSidecarVmOptions,
	): Promise<AgentOsSidecarVmHandle> {
		return this.client.createVm(this.sessionId, options);
	}

	async dispose(): Promise<void> {
		await this.client.disposeSession(this.sessionId);
	}
}

export class AgentOsSidecarClient {
	private readonly createSessionTransport: AgentOsSidecarClientOptions["createSessionTransport"];
	private readonly createId: () => string;
	private readonly now: () => number;
	private readonly sessions = new Map<string, AgentOsSidecarSessionEntry>();
	private disposed = false;

	constructor(options: AgentOsSidecarClientOptions) {
		this.createSessionTransport = options.createSessionTransport;
		this.createId = options.createId ?? randomUUID;
		this.now = options.now ?? Date.now;
	}

	async createSession(
		options: AgentOsSidecarSessionOptions = {},
	): Promise<AgentOsSidecarSessionHandle> {
		this.assertActive();

		const sessionId = this.createId();
		const placement = clonePlacement(options.placement);
		const metadata = cloneMetadata(options.metadata);
		const lifecycle: AgentOsSidecarSessionLifecycle = {
			sessionId,
			placement,
			state: "connecting",
			createdAt: this.now(),
			metadata,
			vmIds: [],
		};
		const entry: AgentOsSidecarSessionEntry = {
			lifecycle,
			vms: new Map(),
		};
		this.sessions.set(sessionId, entry);

		try {
			entry.transport = await this.createSessionTransport({
				sessionId,
				placement: clonePlacement(placement),
				metadata: cloneMetadata(metadata),
				signal: options.signal,
			});
			entry.lifecycle.state = "ready";
			entry.lifecycle.connectedAt = this.now();
			return new AgentOsSidecarSessionHandle(this, sessionId);
		} catch (error) {
			entry.lifecycle.state = "failed";
			entry.lifecycle.lastError = toErrorMessage(error);
			throw toError(error);
		}
	}

	listSessions(): AgentOsSidecarSessionLifecycle[] {
		return [...this.sessions.values()].map((entry) =>
			cloneSessionLifecycle(entry.lifecycle),
		);
	}

	requireSessionLifecycle(sessionId: string): AgentOsSidecarSessionLifecycle {
		const entry = this.getSessionEntry(sessionId);
		return cloneSessionLifecycle(entry.lifecycle);
	}

	listVms(sessionId: string): AgentOsSidecarVmLifecycle[] {
		const entry = this.getSessionEntry(sessionId);
		return [...entry.vms.values()].map((vmEntry) =>
			cloneVmLifecycle(vmEntry.lifecycle),
		);
	}

	requireVmLifecycle(
		sessionId: string,
		vmId: string,
	): AgentOsSidecarVmLifecycle {
		const vmEntry = this.getVmEntry(sessionId, vmId);
		return cloneVmLifecycle(vmEntry.lifecycle);
	}

	async createVm(
		sessionId: string,
		options: AgentOsSidecarVmOptions = {},
	): Promise<AgentOsSidecarVmHandle> {
		this.assertActive();

		const entry = this.getSessionEntry(sessionId);
		if (entry.lifecycle.state !== "ready" || !entry.transport) {
			throw new Error(
				`Cannot create VM for sidecar session ${sessionId} while it is ${entry.lifecycle.state}`,
			);
		}

		const vmId = this.createId();
		const metadata = cloneMetadata(options.metadata);
		const vmEntry: AgentOsSidecarVmEntry = {
			lifecycle: {
				vmId,
				sessionId,
				state: "creating",
				createdAt: this.now(),
				metadata,
			},
		};
		entry.vms.set(vmId, vmEntry);
		entry.lifecycle.vmIds = [...entry.vms.keys()];

		try {
			await entry.transport.createVm?.({
				vmId,
				sessionId,
				metadata: cloneMetadata(metadata),
			});
			vmEntry.lifecycle.state = "ready";
			vmEntry.lifecycle.readyAt = this.now();
			return new AgentOsSidecarVmHandle(this, sessionId, vmId);
		} catch (error) {
			vmEntry.lifecycle.state = "failed";
			vmEntry.lifecycle.lastError = toErrorMessage(error);
			throw toError(error);
		}
	}

	async disposeVm(sessionId: string, vmId: string): Promise<void> {
		const sessionEntry = this.getSessionEntry(sessionId);
		const vmEntry = this.getVmEntry(sessionId, vmId);
		await this.disposeVmEntry(sessionEntry, vmEntry);
	}

	async disposeSession(sessionId: string): Promise<void> {
		const entry = this.getSessionEntry(sessionId);
		if (
			entry.lifecycle.state === "disposed" ||
			entry.lifecycle.state === "disposing"
		) {
			return;
		}

		entry.lifecycle.state = "disposing";

		const errors: Error[] = [];
		for (const vmEntry of entry.vms.values()) {
			try {
				await this.disposeVmEntry(entry, vmEntry);
			} catch (error) {
				errors.push(toError(error));
			}
		}

		try {
			await entry.transport?.dispose();
		} catch (error) {
			errors.push(toError(error));
		}

		if (errors.length > 0) {
			entry.lifecycle.state = "failed";
			entry.lifecycle.lastError = errors.map((error) => error.message).join("; ");
			throw new Error(entry.lifecycle.lastError);
		}

		entry.lifecycle.state = "disposed";
		entry.lifecycle.disposedAt = this.now();
	}

	async dispose(): Promise<void> {
		if (this.disposed) {
			return;
		}

		const errors: Error[] = [];
		for (const sessionId of this.sessions.keys()) {
			try {
				await this.disposeSession(sessionId);
			} catch (error) {
				errors.push(toError(error));
			}
		}

		this.disposed = true;

		if (errors.length > 0) {
			throw new Error(errors.map((error) => error.message).join("; "));
		}
	}

	private async disposeVmEntry(
		sessionEntry: AgentOsSidecarSessionEntry,
		vmEntry: AgentOsSidecarVmEntry,
	): Promise<void> {
		if (
			vmEntry.lifecycle.state === "disposed" ||
			vmEntry.lifecycle.state === "disposing"
		) {
			return;
		}

		vmEntry.lifecycle.state = "disposing";
		try {
			await sessionEntry.transport?.disposeVm?.(vmEntry.lifecycle.vmId);
			vmEntry.lifecycle.state = "disposed";
			vmEntry.lifecycle.disposedAt = this.now();
		} catch (error) {
			vmEntry.lifecycle.state = "failed";
			vmEntry.lifecycle.lastError = toErrorMessage(error);
			throw toError(error);
		}
	}

	private getSessionEntry(sessionId: string): AgentOsSidecarSessionEntry {
		const entry = this.sessions.get(sessionId);
		if (!entry) {
			throw new Error(`Unknown sidecar session: ${sessionId}`);
		}
		return entry;
	}

	private getVmEntry(
		sessionId: string,
		vmId: string,
	): AgentOsSidecarVmEntry {
		const entry = this.getSessionEntry(sessionId);
		const vmEntry = entry.vms.get(vmId);
		if (!vmEntry) {
			throw new Error(`Unknown sidecar VM ${vmId} for session ${sessionId}`);
		}
		return vmEntry;
	}

	private assertActive(): void {
		if (this.disposed) {
			throw new Error("Agent OS sidecar client has already been disposed");
		}
	}
}

export function createAgentOsSidecarClient(
	options: AgentOsSidecarClientOptions,
): AgentOsSidecarClient {
	return new AgentOsSidecarClient(options);
}

function clonePlacement(
	placement: AgentOsSidecarPlacement | undefined,
): AgentOsSidecarPlacement {
	if (!placement || placement.kind === "shared") {
		return {
			kind: "shared",
			...(placement?.pool ? { pool: placement.pool } : {}),
		};
	}

	return {
		kind: "explicit",
		sidecarId: placement.sidecarId,
	};
}

function cloneMetadata(
	metadata: Record<string, string> | undefined,
): Record<string, string> {
	return { ...(metadata ?? {}) };
}

function cloneSessionLifecycle(
	lifecycle: AgentOsSidecarSessionLifecycle,
): AgentOsSidecarSessionLifecycle {
	return {
		...lifecycle,
		placement: clonePlacement(lifecycle.placement),
		metadata: cloneMetadata(lifecycle.metadata),
		vmIds: [...lifecycle.vmIds],
	};
}

function cloneVmLifecycle(
	lifecycle: AgentOsSidecarVmLifecycle,
): AgentOsSidecarVmLifecycle {
	return {
		...lifecycle,
		metadata: cloneMetadata(lifecycle.metadata),
	};
}

function toError(error: unknown): Error {
	return error instanceof Error ? error : new Error(String(error));
}

function toErrorMessage(error: unknown): string {
	return toError(error).message;
}
