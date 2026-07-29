import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
	type AgentOsConformanceAction,
	type AgentOsConformanceBackend,
	type AgentOsConformanceEvent,
	defineAgentOsConformanceSuite,
} from "@rivet-dev/agentos-test-harness/agent-os-conformance";
import { expect } from "vitest";
import { actorHandle, startActorRuntime } from "./helpers/actor-runtime.js";

const RUN_E2E = process.env.AGENTOS_ACTOR_E2E === "1";
let conformanceHandle: any;

async function waitForActorReady(
	handle: any,
	runtime: Awaited<ReturnType<typeof startActorRuntime>>,
	timeoutMs = 120_000,
): Promise<void> {
	const deadline = Date.now() + timeoutMs;
	let lastError: unknown;
	while (Date.now() < deadline) {
		if (runtime.child && runtime.child.exitCode !== null) {
			throw new Error(`actor runtime exited during startup\n${runtime.logs()}`);
		}
		try {
			await handle.echo("ready");
			return;
		} catch (error) {
			lastError = error;
		}
		await new Promise((resolve) => setTimeout(resolve, 250));
	}
	throw new Error(
		`actor did not become ready: ${String(lastError)}\n${runtime.logs()}`,
		{ cause: lastError },
	);
}

defineAgentOsConformanceSuite({
	name: RUN_E2E
		? "AgentOS real Rivet actor conformance"
		: "AgentOS real Rivet actor conformance (skipped)",
	skip: !RUN_E2E,
	async createBackend(): Promise<AgentOsConformanceBackend> {
		if (!RUN_E2E) {
			return {
				call: async () => undefined as never,
				on: () => () => {},
				dispose: async () => {},
			};
		}
		const storagePath = mkdtempSync(join(tmpdir(), "agentos-conformance-e2e-"));
		const runtime = await startActorRuntime(storagePath);
		const handle = actorHandle(runtime.endpoint, `conformance-${Date.now()}`);
		conformanceHandle = handle;
		const connection = handle.connect();
		const subscriptions = new Set<() => void>();
		try {
			await waitForActorReady(handle, runtime);
		} catch (error) {
			connection.dispose?.();
			await runtime.stop();
			rmSync(storagePath, { recursive: true, force: true });
			throw error;
		}

		return {
			async call<T>(
				action: AgentOsConformanceAction,
				...args: unknown[]
			): Promise<T> {
				const path = action.split(".");
				let owner = handle;
				for (const segment of path.slice(0, -1)) owner = owner[segment];
				const method = owner[path.at(-1)!];
				if (typeof method !== "function") {
					throw new Error(`Actor backend does not implement ${action}`);
				}
				return (await method.apply(owner, args)) as T;
			},
			on(
				event: AgentOsConformanceEvent,
				handler: (payload: any) => void,
			): () => void {
				const unsubscribe = connection.on(event, handler);
				const dispose =
					typeof unsubscribe === "function" ? unsubscribe : () => undefined;
				subscriptions.add(dispose);
				return () => {
					subscriptions.delete(dispose);
					dispose();
				};
			},
			async dispose() {
				for (const unsubscribe of subscriptions) unsubscribe();
				connection.dispose?.();
				await runtime.stop();
				rmSync(storagePath, { recursive: true, force: true });
			},
		};
	},
	async verifyBackend() {
		if (!RUN_E2E) return;
		const counts = await conformanceHandle.getHookCounts();
		expect(counts.sessionEvent).toBeGreaterThan(0);
	},
});
