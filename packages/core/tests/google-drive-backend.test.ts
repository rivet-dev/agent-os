import type { NativeMountPluginDescriptor } from "@rivet-dev/agentos-core/descriptors";
import { afterEach, describe, expect, it } from "vitest";
import { AgentOs } from "../src/index.js";

interface GoogleDriveMountConfig {
	credentials: { clientEmail: string; privateKey: string };
	folderId: string;
	keyPrefix?: string;
	chunkSize?: number;
	inlineThreshold?: number;
	[key: string]: unknown;
}

/**
 * Declarative Google Drive native mount descriptor. Routes a first-party Google
 * Drive-backed filesystem through the sidecar's native `google_drive` plugin.
 * Google Drive mounts are part of core agentOS now, so the descriptor is built
 * inline here rather than via a separate file-system registry package.
 */
function googleDriveMountPlugin(
	config: GoogleDriveMountConfig,
): NativeMountPluginDescriptor {
	return { id: "google_drive", config: config as never };
}

const clientEmail = process.env.GOOGLE_DRIVE_CLIENT_EMAIL;
const privateKey = process.env.GOOGLE_DRIVE_PRIVATE_KEY;
const folderId = process.env.GOOGLE_DRIVE_FOLDER_ID;
const hasCredentials = !!(clientEmail && privateKey && folderId);
const ALLOW_ALL_VM_PERMISSIONS = {
	fs: "allow",
	network: "allow",
	childProcess: "allow",
	process: "allow",
	env: "allow",
	binding: "allow",
} as const;

function itIf(condition: boolean, ...args: Parameters<typeof it>): void {
	if (condition) {
		// @ts-expect-error forwarded it() arguments stay runtime-compatible.
		it(...args);
		return;
	}
	const [name] = args;
	it.skip(`${String(name)} [missing Google Drive credentials]`, () => {});
}

let vm: AgentOs | null = null;

afterEach(async () => {
	if (vm) {
		await vm.dispose();
		vm = null;
	}
});

describe("Google Drive filesystem backend", () => {
	itIf(
		hasCredentials,
		"mounts a Google Drive-backed filesystem through AgentOs",
		async () => {
			vm = await AgentOs.create({
				mounts: [
					{
						path: "/data",
						plugin: googleDriveMountPlugin({
							credentials: {
								clientEmail: clientEmail!,
								privateKey: privateKey!,
							},
							folderId: folderId!,
							keyPrefix: `agentos-test-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
							chunkSize: 16,
							inlineThreshold: 8,
						}),
					},
				],
				permissions: ALLOW_ALL_VM_PERMISSIONS,
			});

			const payload = "0123456789abcdef".repeat(32);
			await vm.writeFile("/data/notes.txt", payload);
			const content = await vm.readFile("/data/notes.txt");

			expect(new TextDecoder().decode(content)).toBe(payload);
			expect(await vm.readdir("/data")).toContain("notes.txt");
		},
	);
});
