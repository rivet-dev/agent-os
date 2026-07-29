import { agentOS, setup } from "@rivet-dev/agentos";
import type { Permissions } from "@rivet-dev/agentos";

// docs:start deny-vault
// Allow the filesystem everywhere, but deny anything under /home/agentos/vault.
const denyVault = {
	fs: {
		default: "allow",
		rules: [{ mode: "deny", operations: ["*"], paths: ["/home/agentos/vault/**"] }],
	},
} satisfies Permissions;
// docs:end deny-vault

// docs:start allow-only-data
// Deny the filesystem by default, allow only reads under /home/agentos/data.
const allowOnlyData = {
	fs: {
		default: "deny",
		rules: [{ mode: "allow", operations: ["read", "readdir", "stat"], paths: ["/home/agentos/data/**"] }],
	},
} satisfies Permissions;
// docs:end allow-only-data

const vm = agentOS({
	permissions: {
		...denyVault,
		...allowOnlyData,
	},
});

export const registry = setup({ use: { vm } });
registry.start();
