import { agentOS, setup } from "../../dist/index.js";
import { allowAll } from "@rivet-dev/agentos-core/internal/runtime-compat";
import {
	CONFORMANCE_ACP_ADAPTER,
	CONFORMANCE_AGENT_NAME,
} from "@rivet-dev/agentos-test-harness/agent-os-conformance-fixture";
import { createProjectedAgentPackage } from "@rivet-dev/agentos-test-harness/projected-agent-package";
import { event } from "rivetkit";
import coreutils from "@agentos-software/coreutils";

const conformanceAgent = createProjectedAgentPackage({
	name: CONFORMANCE_AGENT_NAME,
	adapterScript: CONFORMANCE_ACP_ADAPTER,
});
for (const signal of ["SIGINT", "SIGTERM"]) {
	process.once(signal, () => {
		conformanceAgent.cleanup();
		process.exit(0);
	});
}

const vm = agentOS({
	defaultSoftware: false,
	software: [coreutils, conformanceAgent.software],
	mounts: [
		{
			path: "/conformance-mount",
			plugin: {
				id: "host_dir",
				config: {
					hostPath: conformanceAgent.packageDir,
					readOnly: true,
				},
			},
			readOnly: true,
		},
	],
	permissions: allowAll,
	preview: { maxActiveTokens: 8 },
	createState: (_c, input) => ({
		wakeCount: 0,
		creationInput: input ?? null,
		onCreateInput: null,
		beforeConnectCount: 0,
		sessionEventHookCount: 0,
	}),
	events: {
		customLifecycle: event(),
	},
	actions: {
		echo: (_c, value) => value,
		getCreationInput: (c) => c.state.creationInput,
		getCreationInputs: (c) => ({
			createState: c.state.creationInput,
			onCreate: c.state.onCreateInput,
		}),
		getWakeCount: (c) => c.state.wakeCount,
		getBeforeConnectCount: (c) => c.state.beforeConnectCount,
		getHookCounts: (c) => ({
			sessionEvent: c.state.sessionEventHookCount,
		}),
		sleepActor: (c) => c.sleep(),
		inspectAgentOsStorage: async (c) => {
			const tables = await c.db.execute(
				"SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'agentos_fs_%' ORDER BY name",
			);
			const metadata = await c.db.execute(
				"SELECT COUNT(*) AS count FROM agentos_fs_metadata_heads",
			);
			const metadataChunks = await c.db.execute(
				"SELECT COUNT(*) AS count, COALESCE(SUM(length(content)), 0) AS bytes FROM agentos_fs_metadata_chunks",
			);
			const blocks = await c.db.execute(
				"SELECT COUNT(*) AS count, COALESCE(SUM(length(content)), 0) AS bytes FROM agentos_fs_blocks",
			);
			return {
				tables: tables.map((row) => row.name),
				metadataCount: Number(metadata[0]?.count ?? 0),
				metadataChunkCount: Number(metadataChunks[0]?.count ?? 0),
				metadataChunkBytes: Number(metadataChunks[0]?.bytes ?? 0),
				blockCount: Number(blocks[0]?.count ?? 0),
				blockBytes: Number(blocks[0]?.bytes ?? 0),
			};
		},
	},
	onCreate: (c, input) => {
		c.state.onCreateInput = input ?? null;
	},
	onWake: (c) => {
		c.state.wakeCount += 1;
		c.broadcast("customLifecycle", {
			phase: "wake",
			wakeCount: c.state.wakeCount,
		});
	},
	onBeforeConnect: (c, params) => {
		c.state.beforeConnectCount += 1;
		if (params?.authToken !== "e2e-allowed") {
			throw new Error("invalid e2e auth token");
		}
	},
	onSessionEvent: (c) => {
		c.state.sessionEventHookCount += 1;
	},
});

export const registry = setup({
	use: { vm },
	endpoint: process.env.AGENTOS_E2E_ENDPOINT,
	namespace: process.env.RIVET_NAMESPACE ?? "default",
	token: process.env.RIVET_TOKEN ?? "dev",
	envoy: { poolName: process.env.AGENTOS_E2E_POOL_NAME ?? "agentos-e2e" },
	runtime: "native",
});

registry.start();
