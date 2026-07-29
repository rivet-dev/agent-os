import { agentOS, setup } from "@rivet-dev/agentos";
import type { Permissions } from "@rivet-dev/agentos";

// Allow the filesystem everywhere, but deny anything under /home/agentos/vault.
const denyVault = {
	fs: {
		default: "allow",
		rules: [{ mode: "deny", operations: ["*"], paths: ["/home/agentos/vault/**"] }],
	},
} satisfies Permissions;

// Deny the network by default, allow only api.example.com.
const allowOneHost = {
	network: {
		default: "deny",
		rules: [{ mode: "allow", operations: ["*"], patterns: ["api.example.com"] }],
	},
} satisfies Permissions;

// Deny all bindings by default, allow only the "add" binding by name.
const allowOneBinding = {
	binding: {
		default: "deny",
		rules: [{ mode: "allow", operations: ["*"], patterns: ["add"] }],
	},
} satisfies Permissions;

const vm = agentOS({
	permissions: {
		...denyVault,
		...allowOneHost,
		...allowOneBinding,
	},
});

export const registry = setup({ use: { vm } });
registry.start();
