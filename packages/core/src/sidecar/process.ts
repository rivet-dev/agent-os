import {
	NativeSidecarProcessClient,
	type NativeSidecarSpawnOptions,
} from "./native-process-client.js";

export interface AgentOsSidecarProcessHandle {
	client: NativeSidecarProcessClient;
	dispose(): Promise<void>;
}

export function spawnAgentOsSidecar(
	options: NativeSidecarSpawnOptions,
): AgentOsSidecarProcessHandle {
	const client = NativeSidecarProcessClient.spawn(options);
	return {
		client,
		dispose: () => client.dispose(),
	};
}
