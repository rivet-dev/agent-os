import { afterAll, afterEach, beforeAll, describe, expect, it } from "vitest";
import { AgentOs } from "@rivet-dev/agent-os";
import type { MinioContainerHandle } from "@rivet-dev/agent-os/test/docker";
import { startMinioContainer } from "@rivet-dev/agent-os/test/docker";
import { createS3Backend } from "../src/index.js";

let minio: MinioContainerHandle;
let vm: AgentOs | null = null;

beforeAll(async () => {
	minio = await startMinioContainer({ healthTimeout: 60_000 });
}, 90_000);

afterAll(async () => {
	if (minio) {
		await minio.stop();
	}
});

afterEach(async () => {
	if (vm) {
		await vm.dispose();
		vm = null;
	}
});

function createMount(prefix: string) {
	return createS3Backend({
		bucket: minio.bucket,
		prefix,
		region: "us-east-1",
		endpoint: minio.endpoint,
		allowLoopbackEndpoint: true,
		credentials: {
			accessKeyId: minio.accessKeyId,
			secretAccessKey: minio.secretAccessKey,
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
				bucket: minio.bucket,
				prefix: "descriptor-test",
				region: "us-east-1",
				endpoint: minio.endpoint,
				allowLoopbackEndpoint: true,
				credentials: {
					accessKeyId: minio.accessKeyId,
					secretAccessKey: minio.secretAccessKey,
				},
				chunkSize: 16,
				inlineThreshold: 8,
			},
		});
	});

	it("mounts an S3-backed filesystem through AgentOs", async () => {
		vm = await AgentOs.create({
			mounts: [{ path: "/data", plugin: createMount("vm-mount") }],
		});

		await vm.writeFile("/data/notes.txt", "hello from s3");
		const content = await vm.readFile("/data/notes.txt");
		expect(new TextDecoder().decode(content)).toBe("hello from s3");
		expect(await vm.readdir("/data")).toContain("notes.txt");
	});

	it("round-trips large files through the current runtime compatibility path", async () => {
		vm = await AgentOs.create({
			mounts: [{ path: "/data", plugin: createMount(`large-${Date.now()}`) }],
		});

		const payload = "0123456789abcdef".repeat(32);
		await vm.writeFile("/data/large.txt", payload);
		const content = await vm.readFile("/data/large.txt");
		expect(new TextDecoder().decode(content)).toBe(payload);
	});
});
