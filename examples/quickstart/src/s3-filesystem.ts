// S3 File System: mount an S3 bucket and use it like a local filesystem.
//
// Uses createS3Backend from @rivet-dev/agent-os-s3 to mount an S3-compatible
// bucket at /mnt/data through the native S3 plugin descriptor.
//
// Required env vars:
//   S3_BUCKET, S3_REGION, S3_ACCESS_KEY_ID, S3_SECRET_ACCESS_KEY
// Optional:
//   S3_ENDPOINT (for MinIO or other S3-compatible services)

import { AgentOs } from "@rivet-dev/agent-os";
import { createS3Backend } from "@rivet-dev/agent-os-s3";

const {
	S3_BUCKET,
	S3_REGION,
	S3_ACCESS_KEY_ID,
	S3_SECRET_ACCESS_KEY,
	S3_ENDPOINT,
} = process.env;
if (!S3_BUCKET || !S3_ACCESS_KEY_ID || !S3_SECRET_ACCESS_KEY) {
	console.error("Required: S3_BUCKET, S3_ACCESS_KEY_ID, S3_SECRET_ACCESS_KEY");
	process.exit(1);
}

const s3Fs = createS3Backend({
	bucket: S3_BUCKET,
	region: S3_REGION ?? "us-east-1",
	credentials: {
		accessKeyId: S3_ACCESS_KEY_ID,
		secretAccessKey: S3_SECRET_ACCESS_KEY,
	},
	endpoint: S3_ENDPOINT,
});

const vm = await AgentOs.create({
	mounts: [{ path: "/mnt/data", plugin: s3Fs }],
});

// Write a file into the S3-backed mount
await vm.writeFile("/mnt/data/notes.txt", "Hello from agentOS!");
console.log("Wrote /mnt/data/notes.txt");

// Read it back
const content = await vm.readFile("/mnt/data/notes.txt");
console.log("Read:", new TextDecoder().decode(content));

// List the directory
const files = await vm.readdir("/mnt/data");
console.log(
	"Files:",
	files.filter((f) => f !== "." && f !== ".."),
);

await vm.dispose();
