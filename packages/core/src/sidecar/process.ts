import {
	SidecarProcess,
	type SidecarSpawnOptions,
} from "./process-client.js";

export interface AgentOsSidecarProcessHandle {
	client: SidecarProcess;
	dispose(): Promise<void>;
}

export function spawnAgentOsSidecar(
	options: SidecarSpawnOptions,
): AgentOsSidecarProcessHandle {
	const client = SidecarProcess.spawn(options);
	return {
		client,
		dispose: () => client.dispose(),
	};
}
