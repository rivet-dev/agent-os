import type { Permissions } from "../../src/runtime.js";

export const ALLOW_ALL_VM_PERMISSIONS: Permissions = {
	fs: "allow",
	network: "allow",
	childProcess: "allow",
	process: "allow",
	env: "allow",
	tool: "allow",
};
