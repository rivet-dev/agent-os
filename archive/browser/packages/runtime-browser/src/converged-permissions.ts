// Declarative kernel permission policies for the converged conformance harness.
//
// The legacy browser harness expressed permission tests as TS callbacks on the
// in-process kernel (e.g. deny fs reads, deny a network port). The converged
// path enforces permissions in the wasm kernel via a declarative
// `PermissionsPolicy` on `CreateVmConfig`, so those test intents are translated
// here into rule sets the kernel evaluates. Operation names match the kernel's
// (`crates/vm-kernel/src/permissions.rs`).

import type { PermissionsPolicy } from "@rivet-dev/agentos-runtime-core/vm-config";

export interface ConvergedPermissionDenials {
	denyFsRead?: boolean;
	denyChildProcess?: boolean;
	denyNetwork?: boolean;
	denyNetworkPort?: number;
}

// Kernel fs read-family operation names (mirrors FilesystemOperation::as_str).
const FS_READ_OPERATIONS = ["read", "readdir"];

/** Build a kernel PermissionsPolicy from the harness's declarative denials. */
export function convergedPermissionsPolicy(
	denials: ConvergedPermissionDenials = {},
): PermissionsPolicy {
	const policy: Record<string, unknown> = {
		fs: "allow",
		network: "allow",
		childProcess: "allow",
		process: "allow",
		env: "allow",
		binding: "allow",
	};

	if (denials.denyFsRead) {
		policy.fs = {
			default: "allow",
			rules: [{ mode: "deny", operations: FS_READ_OPERATIONS, paths: ["**"] }],
		};
	}
	if (denials.denyChildProcess) {
		policy.childProcess = "deny";
	}
	if (denials.denyNetwork) {
		policy.network = "deny";
	}
	if (denials.denyNetworkPort !== undefined) {
		policy.network = {
			default: "allow",
			rules: [
				{
					mode: "deny",
					operations: ["listen", "http", "fetch"],
					patterns: [`tcp://*:${denials.denyNetworkPort}`],
				},
			],
		};
	}

	return policy as unknown as PermissionsPolicy;
}
