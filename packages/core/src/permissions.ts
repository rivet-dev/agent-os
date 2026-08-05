import * as protocol from "./generated-protocol.js";
import {
	toGeneratedPermissionMode,
	type LivePermissionMode,
} from "./protocol-maps.js";

export type { LivePermissionMode } from "./protocol-maps.js";

export interface LiveFsPermissionRule {
	mode: LivePermissionMode;
	operations?: string[];
	paths?: string[];
}

export interface LivePatternPermissionRule {
	mode: LivePermissionMode;
	operations?: string[];
	patterns?: string[];
}

export interface LiveRulePermissions<TRule> {
	default?: LivePermissionMode;
	rules: TRule[];
}

export type LivePermissionScope<TRule> =
	| LivePermissionMode
	| LiveRulePermissions<TRule>;

export interface LivePermissionsPolicy {
	fs?: LivePermissionScope<LiveFsPermissionRule>;
	network?: LivePermissionScope<LivePatternPermissionRule>;
	child_process?: LivePermissionScope<LivePatternPermissionRule>;
	process?: LivePermissionScope<LivePatternPermissionRule>;
	env?: LivePermissionScope<LivePatternPermissionRule>;
	binding?: LivePermissionScope<LivePatternPermissionRule>;
}

export function toGeneratedPermissionsPolicy(
	policy: LivePermissionsPolicy | undefined,
): protocol.PermissionsPolicy | null {
	if (policy === undefined) {
		return null;
	}
	return {
		fs:
			policy.fs === undefined
				? null
				: toGeneratedFilesystemPermissionScope(policy.fs),
		network:
			policy.network === undefined
				? null
				: toGeneratedPatternPermissionScope(policy.network),
		childProcess:
			policy.child_process === undefined
				? null
				: toGeneratedPatternPermissionScope(policy.child_process),
		process:
			policy.process === undefined
				? null
				: toGeneratedPatternPermissionScope(policy.process),
		env:
			policy.env === undefined
				? null
				: toGeneratedPatternPermissionScope(policy.env),
		binding:
			policy.binding === undefined
				? null
				: toGeneratedPatternPermissionScope(policy.binding),
	};
}

export function toGeneratedFilesystemPermissionScope(
	scope: LivePermissionScope<LiveFsPermissionRule>,
): protocol.FsPermissionScope {
	if (typeof scope === "string") {
		return {
			tag: "PermissionMode",
			val: toGeneratedPermissionMode(scope),
		};
	}
	return {
		tag: "FsPermissionRuleSet",
		val: {
			default:
				scope.default === undefined
					? null
					: toGeneratedPermissionMode(scope.default),
			rules: scope.rules.map((rule) => ({
				mode: toGeneratedPermissionMode(rule.mode),
				operations: rule.operations ?? [],
				paths: rule.paths ?? [],
			})),
		},
	};
}

export function toGeneratedPatternPermissionScope(
	scope: LivePermissionScope<LivePatternPermissionRule>,
): protocol.PatternPermissionScope {
	if (typeof scope === "string") {
		return {
			tag: "PermissionMode",
			val: toGeneratedPermissionMode(scope),
		};
	}
	return {
		tag: "PatternPermissionRuleSet",
		val: {
			default:
				scope.default === undefined
					? null
					: toGeneratedPermissionMode(scope.default),
			rules: scope.rules.map((rule) => ({
				mode: toGeneratedPermissionMode(rule.mode),
				operations: rule.operations ?? [],
				patterns: rule.patterns ?? [],
			})),
		},
	};
}
