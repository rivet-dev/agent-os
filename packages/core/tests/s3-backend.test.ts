import { createS3Backend } from "@rivet-dev/agent-os-s3";
import { afterAll, afterEach, beforeAll, describe, expect, test } from "vitest";
import { AgentOs } from "../src/index.js";
import type {
	MockS3Request,
	MockS3ServerHandle,
} from "./helpers/mock-s3.js";
import { startMockS3Server } from "./helpers/mock-s3.js";

const DATA_DIR = "/mnt/data";
const NOTES_PATH = `${DATA_DIR}/notes.txt`;
const NOTES_CONTENT = "Hello from agentOS!";
const ALLOW_LOCAL_S3_ENDPOINTS_ENV = "AGENT_OS_ALLOW_LOCAL_S3_ENDPOINTS";
const skipS3 = process.env.SKIP_S3 === "1";

function createMount(server: MockS3ServerHandle, prefix: string) {
	return createS3Backend({
		bucket: server.bucket,
		prefix,
		region: "us-east-1",
		endpoint: server.endpoint,
		credentials: {
			accessKeyId: server.accessKeyId,
			secretAccessKey: server.secretAccessKey,
		},
	});
}

describe("S3 filesystem quickstart truth test", () => {
	if (skipS3) {
		test("is disabled only by the explicit SKIP_S3=1 gate", () => {
			expect(process.env.SKIP_S3).toBe("1");
		});
		return;
	}

	let server: MockS3ServerHandle | null = null;
	let vm: AgentOs | null = null;
	const previousAllowLocalS3Endpoints =
		process.env[ALLOW_LOCAL_S3_ENDPOINTS_ENV];

	beforeAll(async () => {
		process.env[ALLOW_LOCAL_S3_ENDPOINTS_ENV] = "1";
		try {
			server = await startMockS3Server();
		} catch (error) {
			const message = error instanceof Error ? error.message : String(error);
			throw new Error(
				[
					"S3 quickstart truth test requires a reachable S3-compatible endpoint.",
					"Use the local mock harness for this suite or set SKIP_S3=1 to bypass it explicitly.",
					`Underlying error: ${message}`,
				].join(" "),
			);
		}
	});

	afterEach(async () => {
		if (vm) {
			await vm.dispose();
			vm = null;
		}
	});

	afterAll(async () => {
		if (vm) {
			await vm.dispose();
			vm = null;
		}
		if (server) {
			await server.stop();
			server = null;
		}
		if (previousAllowLocalS3Endpoints == null) {
			delete process.env[ALLOW_LOCAL_S3_ENDPOINTS_ENV];
		} else {
			process.env[ALLOW_LOCAL_S3_ENDPOINTS_ENV] =
				previousAllowLocalS3Endpoints;
		}
	});

	test(
		"round-trips writeFile, readFile, and readdir through createS3Backend",
		async () => {
			if (!server) {
				throw new Error("Mock S3 test harness did not start.");
			}
			const vmPrefix = `quickstart-${Date.now()}`;

			vm = await AgentOs.create({
				mounts: [
					{
						path: DATA_DIR,
						plugin: createMount(server, vmPrefix),
					},
				],
			});

			await vm.writeFile(NOTES_PATH, NOTES_CONTENT);

			const content = await vm.readFile(NOTES_PATH);
			expect(new TextDecoder().decode(content)).toBe(NOTES_CONTENT);

			const files = (await vm.readdir(DATA_DIR)).filter(
				(entry) => entry !== "." && entry !== "..",
			);
			expect(files).toContain("notes.txt");

			const requestMethods = server.requests().map(
				(request: MockS3Request) => request.method,
			);
			expect(requestMethods.length).toBeGreaterThan(0);
			expect(
				requestMethods.every((method) => ["GET", "PUT"].includes(method)),
			).toBe(true);
			expect(
				server
					.requests()
					.every(
						(request: MockS3Request) =>
							request.path.startsWith(`/${server.bucket}/${vmPrefix}/`) &&
							(request.query === "x-id=GetObject" ||
								request.query === "x-id=PutObject"),
					),
			).toBe(true);
		},
		120_000,
	);
});
