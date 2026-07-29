import { agentOS, setup } from "@rivet-dev/agentos";

const vm = agentOS({
	// Configure software, permissions, mounts, and resource limits here.
});

export const registry = setup({ use: { vm } });
