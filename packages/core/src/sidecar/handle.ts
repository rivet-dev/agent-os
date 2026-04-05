import { randomUUID } from "node:crypto";
import {
	createAgentOsSidecarClient,
	type AgentOsSidecarPlacement,
	type AgentOsSidecarSessionHandle,
	type AgentOsSidecarVmHandle,
} from "./client.js";
import {
	createInProcessSidecarTransport,
	type CreateInProcessSidecarTransportOptions,
	type InProcessSidecarTransport,
	type InProcessSidecarVmAdmin,
} from "./in-process-transport.js";

export interface AgentOsSharedSidecarOptions {
	pool?: string;
}

export interface AgentOsCreateSidecarOptions {
	sidecarId?: string;
}

export type AgentOsSidecarConfig =
	| { kind: "shared"; pool?: string }
	| { kind: "explicit"; handle: AgentOsSidecar };

export interface AgentOsSidecarDescription {
	sidecarId: string;
	placement: AgentOsSidecarPlacement;
	state: "ready" | "disposing" | "disposed";
	activeVmCount: number;
}

export interface AgentOsSidecarVmLease<
	TVmAdmin extends InProcessSidecarVmAdmin,
> {
	sidecar: AgentOsSidecar;
	session: AgentOsSidecarSessionHandle;
	vm: AgentOsSidecarVmHandle;
	admin: TVmAdmin;
	dispose(): Promise<void>;
}

interface AgentOsSidecarLeaseRecord {
	dispose(): Promise<void>;
}

interface AgentOsSidecarState {
	description: AgentOsSidecarDescription;
	activeLeases: Set<AgentOsSidecarLeaseRecord>;
	sharedPool?: string;
}

const sidecarStates = new WeakMap<AgentOsSidecar, AgentOsSidecarState>();
const sharedSidecars = new Map<string, AgentOsSidecar>();

export class AgentOsSidecar {
	constructor(
		sidecarId: string,
		placement: AgentOsSidecarPlacement,
		sharedPool?: string,
	) {
		sidecarStates.set(this, {
			description: {
				sidecarId,
				placement: clonePlacement(placement),
				state: "ready",
				activeVmCount: 0,
			},
			activeLeases: new Set(),
			sharedPool,
		});
	}

	describe(): AgentOsSidecarDescription {
		const state = getSidecarState(this);
		return cloneDescription(state.description);
	}

	async dispose(): Promise<void> {
		const state = getSidecarState(this);
		if (state.description.state === "disposed") {
			return;
		}

		state.description.state = "disposing";
		const errors: Error[] = [];
		for (const lease of [...state.activeLeases]) {
			try {
				await lease.dispose();
			} catch (error) {
				errors.push(
					error instanceof Error ? error : new Error(String(error)),
				);
			}
		}
		state.activeLeases.clear();
		state.description.activeVmCount = 0;
		state.description.state = "disposed";
		if (
			state.sharedPool
			&& sharedSidecars.get(state.sharedPool) === this
		) {
			sharedSidecars.delete(state.sharedPool);
		}
		if (errors.length > 0) {
			throw new Error(errors.map((error) => error.message).join("; "));
		}
	}
}

export function createAgentOsSidecar(
	options: AgentOsCreateSidecarOptions = {},
): AgentOsSidecar {
	const sidecarId = options.sidecarId ?? `agent-os-sidecar-${randomUUID()}`;
	return new AgentOsSidecar(sidecarId, {
		kind: "explicit",
		sidecarId,
	});
}

export function getSharedAgentOsSidecar(
	options: AgentOsSharedSidecarOptions = {},
): AgentOsSidecar {
	const pool = options.pool ?? "default";
	const existing = sharedSidecars.get(pool);
	if (existing && existing.describe().state !== "disposed") {
		return existing;
	}

	const sidecar = new AgentOsSidecar(
		`agent-os-shared-sidecar:${pool}`,
		{ kind: "shared", ...(pool ? { pool } : {}) },
		pool,
	);
	sharedSidecars.set(pool, sidecar);
	return sidecar;
}

export async function leaseAgentOsSidecarVm<
	TVmAdmin extends InProcessSidecarVmAdmin,
>(
	sidecar: AgentOsSidecar,
	options: CreateInProcessSidecarTransportOptions<TVmAdmin>,
): Promise<AgentOsSidecarVmLease<TVmAdmin>> {
	const state = getSidecarState(sidecar);
	if (state.description.state !== "ready") {
		throw new Error(
			`Cannot lease VM from sidecar ${state.description.sidecarId} while it is ${state.description.state}`,
		);
	}

	let transport: InProcessSidecarTransport<TVmAdmin> | undefined;
	const client = createAgentOsSidecarClient({
		async createSessionTransport(sessionBootstrap) {
			transport = await createInProcessSidecarTransport(
				sessionBootstrap,
				options,
			);
			return transport;
		},
	});

	let disposed = false;
	let leaseRecord: AgentOsSidecarLeaseRecord | undefined;

	try {
		const session = await client.createSession({
			placement: clonePlacement(state.description.placement),
		});
		const vm = await session.createVm();
		const admin = transport?.getVmAdmin(vm.vmId);
		if (!admin) {
			throw new Error(`Sidecar VM admin was not registered for ${vm.vmId}`);
		}

		const lease: AgentOsSidecarVmLease<TVmAdmin> = {
			sidecar,
			session,
			vm,
			admin,
			async dispose() {
				if (disposed) {
					return;
				}
				disposed = true;
				state.activeLeases.delete(leaseRecord!);
				state.description.activeVmCount = state.activeLeases.size;
				await client.dispose();
			},
		};

		leaseRecord = {
			dispose: () => lease.dispose(),
		};
		state.activeLeases.add(leaseRecord);
		state.description.activeVmCount = state.activeLeases.size;
		return lease;
	} catch (error) {
		await client.dispose().catch(() => {});
		throw error;
	}
}

function getSidecarState(sidecar: AgentOsSidecar): AgentOsSidecarState {
	const state = sidecarStates.get(sidecar);
	if (!state) {
		throw new Error("Unknown Agent OS sidecar handle");
	}
	return state;
}

function cloneDescription(
	description: AgentOsSidecarDescription,
): AgentOsSidecarDescription {
	return {
		...description,
		placement: clonePlacement(description.placement),
	};
}

function clonePlacement(
	placement: AgentOsSidecarPlacement,
): AgentOsSidecarPlacement {
	if (placement.kind === "shared") {
		return {
			kind: "shared",
			...(placement.pool ? { pool: placement.pool } : {}),
		};
	}

	return {
		kind: "explicit",
		sidecarId: placement.sidecarId,
	};
}
