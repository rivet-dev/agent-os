import { afterAll, afterEach, beforeAll, describe, expect, it } from "vitest";
import { AgentOs } from "@rivet-dev/agent-os-core";
import type { MockS3ServerHandle } from "@rivet-dev/agent-os-core/test/mock-s3";
import { startMockS3Server } from "@rivet-dev/agent-os-core/test/mock-s3";
import { createS3Backend } from "../src/index.js";

let server: MockS3ServerHandle;
let vm: AgentOs | null = null;
const ALLOW_ALL_VM_PERMISSIONS = {
	fs: "allow",
	network: "allow",
	childProcess: "allow",
	process: "allow",
	env: "allow",
	tool: "allow",
} as const;

beforeAll(async () => {
	process.env.AGENT_OS_ALLOW_LOCAL_S3_ENDPOINTS = "1";
	server = await startMockS3Server();
});

afterAll(async () => {
	if (server) {
		await server.stop();
	}
	delete process.env.AGENT_OS_ALLOW_LOCAL_S3_ENDPOINTS;
});

afterEach(async () => {
	if (vm) {
		await vm.dispose();
		vm = null;
	}
});

function createMount(prefix: string) {
	return createS3Backend({
		bucket: server.bucket,
		prefix,
		region: "us-east-1",
		endpoint: server.endpoint,
		credentials: {
			accessKeyId: server.accessKeyId,
			secretAccessKey: server.secretAccessKey,
		},
		chunkSize: 16,
		inlineThreshold: 8,
	});
}

describe("@rivet-dev/agent-os-s3", () => {
	it("serializes a native s3 mount descriptor", () => {
		expect(createMount("descriptor-test")).toEqual({
			id: "s3",
			config: {
				bucket: server.bucket,
				prefix: "descriptor-test",
				region: "us-east-1",
				endpoint: server.endpoint,
				credentials: {
					accessKeyId: server.accessKeyId,
					secretAccessKey: server.secretAccessKey,
				},
				chunkSize: 16,
				inlineThreshold: 8,
			},
		});
	});

	it("mounts an S3-backed filesystem through AgentOs", async () => {
		vm = await AgentOs.create({
			mounts: [{ path: "/data", plugin: createMount("vm-mount") }],
			permissions: ALLOW_ALL_VM_PERMISSIONS,
		});

		await vm.writeFile("/data/notes.txt", "hello from s3");
		const content = await vm.readFile("/data/notes.txt");
		expect(new TextDecoder().decode(content)).toBe("hello from s3");
		expect(await vm.readdir("/data")).toContain("notes.txt");
	});

	it("round-trips large files through the current runtime compatibility path", async () => {
		vm = await AgentOs.create({
			mounts: [{ path: "/data", plugin: createMount(`large-${Date.now()}`) }],
			permissions: ALLOW_ALL_VM_PERMISSIONS,
		});

		const payload = "0123456789abcdef".repeat(32);
		await vm.writeFile("/data/large.txt", payload);
		const content = await vm.readFile("/data/large.txt");
		expect(new TextDecoder().decode(content)).toBe(payload);
	});
});
