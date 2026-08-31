import { AgentOs, type Permissions } from "@rivet-dev/agentos-core";

// The kernel permission policy is the same object the actor takes. Pass it to
// AgentOs.create() instead of agentOS().
const permissions = {
	network: {
		default: "deny",
		rules: [
			{ mode: "allow", operations: ["*"], patterns: ["api.example.com"] },
		],
	},
} satisfies Permissions;

const vm = await AgentOs.create({ permissions });

await vm.dispose();
