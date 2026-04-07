import type { Permissions } from "../runtime-compat.js";
import type { SidecarPermissionsPolicy } from "./rpc-client.js";

export function serializePermissionsForSidecar(
	permissions?: Permissions,
): SidecarPermissionsPolicy {
	if (!permissions) {
		return {
			fs: "allow",
			network: "allow",
			childProcess: "allow",
			env: "allow",
		};
	}

	return {
		fs: permissions.fs,
		network: permissions.network,
		childProcess: permissions.childProcess,
		env: permissions.env,
	};
}
