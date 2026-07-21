import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll } from "vitest";
import {
	__disposeAllSharedSidecarsForTesting,
	AgentOs,
} from "../../src/agent-os.js";
import { ALLOW_ALL_VM_PERMISSIONS } from "./permissions.js";

const globalState = globalThis as typeof globalThis & {
	__agentOsOriginalCreate?: typeof AgentOs.create;
	__agentOsDefaultPermissionsPatched?: boolean;
};
const databaseDirectories: string[] = [];

function configuredTestWasmBackend(): "v8" | "wasmtime" | undefined {
	const backend = process.env.AGENTOS_TEST_WASM_BACKEND;
	if (backend === undefined || backend === "") return undefined;
	if (backend === "v8" || backend === "wasmtime") return backend;
	throw new Error(
		`AGENTOS_TEST_WASM_BACKEND must be "v8" or "wasmtime", got ${JSON.stringify(backend)}`,
	);
}

function testDatabase() {
	const directory = mkdtempSync(join(tmpdir(), "agentos-test-sqlite-"));
	databaseDirectories.push(directory);
	return {
		type: "sqlite_file" as const,
		path: join(directory, "agentos.sqlite"),
	};
}

if (!globalState.__agentOsDefaultPermissionsPatched) {
	const originalCreate = AgentOs.create.bind(AgentOs);
	globalState.__agentOsOriginalCreate = originalCreate;
	globalState.__agentOsDefaultPermissionsPatched = true;

	AgentOs.create = (async (...args: Parameters<typeof AgentOs.create>) => {
		const [options] = args;
		return originalCreate({
			...(options ?? {}),
			database: options?.database ?? testDatabase(),
			permissions: options?.permissions ?? ALLOW_ALL_VM_PERMISSIONS,
			wasmBackend: options?.wasmBackend ?? configuredTestWasmBackend(),
		});
	}) as typeof AgentOs.create;
}

// Vitest forks a worker per file. Each worker holds the process-global
// `sharedSidecars` map, so we must dispose the shared sidecar on file teardown
// or the underlying native sidecar subprocess keeps its piped stdio open and
// blocks the worker (and therefore `pnpm test`) from exiting.
afterAll(async () => {
	await __disposeAllSharedSidecarsForTesting();
	for (const directory of databaseDirectories.splice(0)) {
		rmSync(directory, { recursive: true, force: true });
	}
});
