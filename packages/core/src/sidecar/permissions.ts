import type { PermissionsPolicy } from "../vm-config.js";
import type { Permissions } from "../runtime-compat.js";

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
		| Permissions["binding"],
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
): PermissionsPolicy {
	if (!permissions) {
		return {
			fs: "deny",
			network: "deny",
			childProcess: "deny",
			process: "deny",
			env: "deny",
			binding: "deny",
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
		binding:
			typeof permissions.binding === "string" || !permissions.binding
				? permissions.binding
				: serializePatternScope(permissions.binding),
	};
}
