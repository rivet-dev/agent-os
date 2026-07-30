// Converged module-resolution servicer.
//
// Reuses the shared naive-Node resolver (`resolveModule`/`loadFile`/
// `moduleFormat` from runtime.ts) unchanged, backed by a kernel-backed
// filesystem so resolution reads the kernel's exact view (mounts, symlinks,
// exports/conditions) on the converged path — satisfying convergence item K
// (one module resolver) without forking it. Async because the resolver walks
// the filesystem; the router awaits it.

import type { ConvergedSyncResponse } from "./converged-fs-bridge.js";
import { loadFile, moduleFormat, resolveModule } from "./runtime.js";
import type { VirtualFileSystem } from "./runtime.js";
import {
	SYNC_BRIDGE_KIND_JSON,
	SYNC_BRIDGE_KIND_NONE,
	SYNC_BRIDGE_KIND_TEXT,
} from "./sync-bridge.js";

const MODULE_OPERATIONS = new Set([
	"module.resolve",
	"module.loadFile",
	"module.format",
	"module.batchResolve",
]);

export class ConvergedModuleServicer {
	private readonly filesystem: VirtualFileSystem;

	constructor(filesystem: VirtualFileSystem) {
		this.filesystem = filesystem;
	}

	handles(operation: string): boolean {
		return MODULE_OPERATIONS.has(operation);
	}

	async handle(
		operation: string,
		args: readonly unknown[],
	): Promise<ConvergedSyncResponse> {
		switch (operation) {
			case "module.resolve": {
				const mode =
					args[2] === "import" || args[2] === "require" ? args[2] : "require";
				const resolved = await resolveModule(
					String(args[0]),
					String(args[1]),
					this.filesystem,
					mode,
				);
				return resolved === null
					? { kind: SYNC_BRIDGE_KIND_NONE }
					: { kind: SYNC_BRIDGE_KIND_TEXT, value: resolved };
			}
			case "module.loadFile": {
				const source = await loadFile(String(args[0]), this.filesystem);
				return source === null
					? { kind: SYNC_BRIDGE_KIND_NONE }
					: { kind: SYNC_BRIDGE_KIND_TEXT, value: source };
			}
			case "module.format": {
				const format = await moduleFormat(String(args[0]), this.filesystem);
				return format === null
					? { kind: SYNC_BRIDGE_KIND_NONE }
					: { kind: SYNC_BRIDGE_KIND_TEXT, value: format };
			}
			case "module.batchResolve": {
				const requests = parseModuleBatchRequests(args[0]);
				const results: Array<{ resolved: string; source: string } | null> = [];
				for (const [specifier, referrer] of requests) {
					const resolved = await resolveModule(
						specifier,
						referrer,
						this.filesystem,
						"import",
					);
					if (resolved === null) {
						results.push(null);
						continue;
					}
					const source = await loadFile(resolved, this.filesystem);
					results.push(source === null ? null : { resolved, source });
				}
				return { kind: SYNC_BRIDGE_KIND_JSON, value: results };
			}
			default:
				throw new Error(
					`converged module servicer does not handle ${operation}`,
				);
		}
	}
}

function parseModuleBatchRequests(value: unknown): [string, string][] {
	const parsed = typeof value === "string" ? JSON.parse(value) : value;
	if (!Array.isArray(parsed)) {
		throw new Error("module.batchResolve requests must be an array");
	}
	return parsed.map((entry) => {
		if (
			!Array.isArray(entry) ||
			entry.length < 2 ||
			typeof entry[0] !== "string" ||
			typeof entry[1] !== "string"
		) {
			throw new Error(
				"module.batchResolve requests must be [specifier, referrer] pairs",
			);
		}
		return [entry[0], entry[1]];
	});
}
