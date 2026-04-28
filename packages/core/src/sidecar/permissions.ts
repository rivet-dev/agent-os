import type { Permissions } from "../runtime-compat.js";
import type { SidecarPermissionsPolicy } from "./rpc-client.js";

const ALL_OPERATIONS = ["*"];
const ALL_RESOURCES = ["**"];

function serializeFilesystemScope(
	scope: Exclude<Permissions["fs"], string | undefined>,
) {
	return {
		...(scope.default === undefined ? {} : { default: scope.default }),
		rules: scope.rules.map((rule) => ({
			...rule,
			operations: rule.operations ?? ALL_OPERATIONS,
			paths: rule.paths ?? ALL_RESOURCES,
		})),
	};
}

function serializePatternScope(
	scope: Exclude<
		| Permissions["network"]
		| Permissions["childProcess"]
		| Permissions["process"]
		| Permissions["env"]
		| Permissions["tool"],
		string | undefined
	>,
) {
	return {
		...(scope.default === undefined ? {} : { default: scope.default }),
		rules: scope.rules.map((rule) => ({
			...rule,
			operations: rule.operations ?? ALL_OPERATIONS,
			patterns: rule.patterns ?? ALL_RESOURCES,
		})),
	};
}

export function serializePermissionsForSidecar(
	permissions?: Permissions,
): SidecarPermissionsPolicy {
	if (!permissions) {
		return {
			fs: "deny",
			network: "deny",
			childProcess: "deny",
			process: "deny",
			env: "deny",
			tool: "deny",
		};
	}

	return {
		fs:
			typeof permissions.fs === "string" || !permissions.fs
				? permissions.fs
				: serializeFilesystemScope(permissions.fs),
		network:
			typeof permissions.network === "string" || !permissions.network
				? permissions.network
				: serializePatternScope(permissions.network),
		childProcess:
			typeof permissions.childProcess === "string" || !permissions.childProcess
				? permissions.childProcess
				: serializePatternScope(permissions.childProcess),
		process:
			typeof permissions.process === "string" || !permissions.process
				? permissions.process
				: serializePatternScope(permissions.process),
		env:
			typeof permissions.env === "string" || !permissions.env
				? permissions.env
				: serializePatternScope(permissions.env),
		tool:
			typeof permissions.tool === "string" || !permissions.tool
				? permissions.tool
				: serializePatternScope(permissions.tool),
	};
}
