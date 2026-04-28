// S3 File System: mount an S3 bucket and use it like a local filesystem.
//
// Uses createS3Backend from @rivet-dev/agent-os-s3 to mount an S3-compatible
// bucket at /mnt/data through the native S3 plugin descriptor.
//
// Required env vars:
//   S3_BUCKET, S3_REGION, S3_ACCESS_KEY_ID, S3_SECRET_ACCESS_KEY
// Optional:
//   S3_ENDPOINT (for MinIO or other S3-compatible services)
//   S3_PREFIX (defaults to "quickstart-s3-filesystem")
//
// Local verification fallback:
//   If the S3 env vars are omitted, this script starts the repo's strict local
//   S3 harness so `pnpm --dir examples/quickstart exec tsx src/s3-filesystem.ts`
//   still exercises the real quickstart flow against signed S3 requests.

import { AgentOs } from "@rivet-dev/agent-os-core";
import { createS3Backend } from "@rivet-dev/agent-os-s3";
import type { MockS3ServerHandle } from "../../../packages/core/src/test/mock-s3.js";
import { startMockS3Server } from "../../../packages/core/src/test/mock-s3.js";

let bucket = process.env.S3_BUCKET;
let region = process.env.S3_REGION ?? "us-east-1";
let prefix = process.env.S3_PREFIX ?? "quickstart-s3-filesystem";
let accessKeyId = process.env.S3_ACCESS_KEY_ID;
let secretAccessKey = process.env.S3_SECRET_ACCESS_KEY;
let endpoint = process.env.S3_ENDPOINT;
let localHarness: MockS3ServerHandle | null = null;
const previousAllowLocalS3Endpoints = process.env.AGENT_OS_ALLOW_LOCAL_S3_ENDPOINTS;

if (!bucket || !accessKeyId || !secretAccessKey) {
	localHarness = await startMockS3Server();
	bucket = localHarness.bucket;
	accessKeyId = localHarness.accessKeyId;
	secretAccessKey = localHarness.secretAccessKey;
	endpoint = localHarness.endpoint;
	prefix = `quickstart-s3-filesystem-${Date.now()}`;
	process.env.AGENT_OS_ALLOW_LOCAL_S3_ENDPOINTS = "1";
	console.log(`Using local strict S3 harness at ${endpoint}`);
}

if (endpoint) {
	const endpointHost = new URL(endpoint).hostname;
	if (endpointHost === "127.0.0.1" || endpointHost === "localhost") {
		process.env.AGENT_OS_ALLOW_LOCAL_S3_ENDPOINTS = "1";
	}
}

const s3Fs = createS3Backend({
	bucket,
	prefix,
	region,
	credentials: {
		accessKeyId,
		secretAccessKey,
	},
	endpoint,
});

const vm = await AgentOs.create({
	mounts: [{ path: "/mnt/data", plugin: s3Fs }],
});

try {
	// Write a file into the S3-backed mount
	await vm.writeFile("/mnt/data/notes.txt", "Hello from agentOS!");
	console.log("Wrote /mnt/data/notes.txt");
	console.log("S3 prefix:", prefix);

	// Read it back
	const content = await vm.readFile("/mnt/data/notes.txt");
	console.log("Read:", new TextDecoder().decode(content));

	// List the directory
	const files = await vm.readdir("/mnt/data");
	console.log(
		"Files:",
		files.filter((f) => f !== "." && f !== ".."),
	);
} finally {
	await vm.dispose();
	if (localHarness) {
		await localHarness.stop();
	}
	if (previousAllowLocalS3Endpoints == null) {
		delete process.env.AGENT_OS_ALLOW_LOCAL_S3_ENDPOINTS;
	} else {
		process.env.AGENT_OS_ALLOW_LOCAL_S3_ENDPOINTS =
			previousAllowLocalS3Endpoints;
	}
}
