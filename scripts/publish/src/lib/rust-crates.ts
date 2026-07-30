import { readFileSync } from "node:fs";
import { join } from "node:path";

// AgentOS-owned crates published to crates.io in dependency order. Crates with
// `publish = false` stay out of this list.
export const RUST_CRATE_ORDER = [
	"agentos-build-support",
	"agentos-actor-uds-client",
	"agentos-bridge",
	"agentos-runtime-tokio",
	"agentos-vfs-core",
	"agentos-vfs",
	"agentos-kernel",
	"agentos-vm-config",
	"agentos-sidecar-protocol",
	"agentos-v8-runtime",
	"agentos-execution",
	"agentos-native-sidecar-core",
	"agentos-sidecar-client",
	"agentos-native-sidecar",
	"agentos-protocol",
	"agentos-client",
	"agentos-sidecar",
] as const;

export type PublishableRustCrate = (typeof RUST_CRATE_ORDER)[number];

export const RUST_CRATES = RUST_CRATE_ORDER;

function readPackageName(manifestPath: string): string | undefined {
	const manifest = readFileSync(manifestPath, "utf8");
	const match = manifest.match(/^\s*name\s*=\s*"([^"]+)"/m);
	return match?.[1];
}

function workspaceMembers(repoRoot: string): string[] {
	const manifest = readFileSync(join(repoRoot, "Cargo.toml"), "utf8");
	const match = manifest.match(/\[workspace\][\s\S]*?members\s*=\s*\[([\s\S]*?)\]/);
	if (!match) return [];
	return [...match[1].matchAll(/"([^"]+)"/g)].map((item) => item[1]);
}

export function discoverRustCrates(repoRoot: string): PublishableRustCrate[] {
	const workspaceCrates = new Set<string>();
	for (const member of workspaceMembers(repoRoot)) {
		const packageName = readPackageName(join(repoRoot, member, "Cargo.toml"));
		if (packageName) {
			workspaceCrates.add(packageName);
		}
	}
	return RUST_CRATE_ORDER.filter((crate) => workspaceCrates.has(crate));
}
