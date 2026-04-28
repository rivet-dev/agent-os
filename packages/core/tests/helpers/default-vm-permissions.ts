import { afterAll } from "vitest";
import { AgentOs, __disposeAllSharedSidecarsForTesting } from "../../src/agent-os.js";
import { ALLOW_ALL_VM_PERMISSIONS } from "./permissions.js";

const globalState = globalThis as typeof globalThis & {
	__agentOsOriginalCreate?: typeof AgentOs.create;
	__agentOsDefaultPermissionsPatched?: boolean;
};

if (!globalState.__agentOsDefaultPermissionsPatched) {
	const originalCreate = AgentOs.create.bind(AgentOs);
	globalState.__agentOsOriginalCreate = originalCreate;
	globalState.__agentOsDefaultPermissionsPatched = true;

	AgentOs.create = (async (...args: Parameters<typeof AgentOs.create>) => {
		const [options] = args;
		if (options?.permissions !== undefined) {
			return originalCreate(options);
		}
		return originalCreate({
			...(options ?? {}),
			permissions: ALLOW_ALL_VM_PERMISSIONS,
		});
	}) as typeof AgentOs.create;
}

// Vitest forks a worker per file. Each worker holds the process-global
// `sharedSidecars` map, so we must dispose the shared sidecar on file teardown
// or the underlying native sidecar subprocess keeps its piped stdio open and
// blocks the worker (and therefore `pnpm test`) from exiting.
afterAll(async () => {
	await __disposeAllSharedSidecarsForTesting();
});
