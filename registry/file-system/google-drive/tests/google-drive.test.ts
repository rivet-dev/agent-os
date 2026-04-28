import { afterEach, describe, expect, it } from "vitest";
import { AgentOs } from "@rivet-dev/agent-os-core";
import { createGoogleDriveBackend } from "../src/index.js";

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
	tool: "allow",
} as const;

function itIf(condition: boolean, ...args: Parameters<typeof it>): void {
	if (condition) {
		// @ts-expect-error forwarded it() arguments stay runtime-compatible.
		it(...args);
		return;
	}
	const [name] = args;
	it(String(name), () => {});
}

let vm: AgentOs | null = null;

afterEach(async () => {
	if (vm) {
		await vm.dispose();
		vm = null;
	}
});

describe("@rivet-dev/agent-os-google-drive", () => {
	it("serializes a native google_drive mount descriptor", () => {
		expect(
			createGoogleDriveBackend({
				credentials: {
					clientEmail: "service-account@example.com",
					privateKey: "-----BEGIN PRIVATE KEY-----\n...\n-----END PRIVATE KEY-----",
				},
				folderId: "folder-123",
				keyPrefix: "agent-os/test",
				chunkSize: 16,
				inlineThreshold: 8,
			}),
		).toEqual({
			id: "google_drive",
			config: {
				credentials: {
					clientEmail: "service-account@example.com",
					privateKey: "-----BEGIN PRIVATE KEY-----\n...\n-----END PRIVATE KEY-----",
				},
				folderId: "folder-123",
				keyPrefix: "agent-os/test",
				chunkSize: 16,
				inlineThreshold: 8,
			},
		});
	});

	itIf(
		hasCredentials,
		"mounts a Google Drive-backed filesystem through AgentOs",
		async () => {
			vm = await AgentOs.create({
				mounts: [
					{
						path: "/data",
						plugin: createGoogleDriveBackend({
							credentials: {
								clientEmail: clientEmail!,
								privateKey: privateKey!,
							},
							folderId: folderId!,
							keyPrefix: `agent-os-test-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
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
