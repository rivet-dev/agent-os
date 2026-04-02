import { mkdir, mkdtemp, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import { createBrowserPlaygroundServer } from "../backend/server.js";

let server: ReturnType<typeof createBrowserPlaygroundServer> | null = null;
const playgroundDir = resolve(fileURLToPath(new URL("..", import.meta.url)));
const vendorDir = resolve(playgroundDir, "vendor");
let tempDir: string | null = null;

async function createVendorFixture(): Promise<string> {
	tempDir = await mkdtemp(resolve(tmpdir(), "playground-vendor-"));
	const sourcePath = resolve(tempDir, "fixture.js");
	const linkPath = resolve(vendorDir, "test-fixture.js");

	await mkdir(vendorDir, { recursive: true });
	await rm(linkPath, { force: true });
	await writeFile(sourcePath, 'console.log("fixture");\n');
	await symlink(sourcePath, linkPath);

	return "/vendor/test-fixture.js";
}

function listenOnRandomPort(
	s: ReturnType<typeof createBrowserPlaygroundServer>,
): Promise<number> {
	return new Promise((resolve, reject) => {
		s.listen(0, () => {
			const address = s.address();
			if (!address || typeof address === "string") {
				reject(new Error("Expected a TCP server address"));
				return;
			}
			resolve(address.port);
		});
		s.once("error", reject);
	});
}

afterEach(async () => {
	if (server) {
		await new Promise<void>((resolve, reject) => {
			server?.close((error) => {
				if (error) {
					reject(error);
					return;
				}
				resolve();
			});
		});
		server = null;
	}
	await rm(resolve(vendorDir, "test-fixture.js"), { force: true });
	if (tempDir) {
		await rm(tempDir, { recursive: true, force: true });
		tempDir = null;
	}
});

describe("browser playground server", () => {
	it("serves vendor assets with COEP/COOP headers", async () => {
		server = createBrowserPlaygroundServer();
		const port = await listenOnRandomPort(server);
		const assetPath = await createVendorFixture();

		const response = await fetch(`http://127.0.0.1:${port}${assetPath}`);

		expect(response.status).toBe(200);
		expect(response.headers.get("content-type")).toBe(
			"text/javascript; charset=utf-8",
		);
		expect(response.headers.get("cross-origin-embedder-policy")).toBe(
			"require-corp",
		);
		expect(response.headers.get("cross-origin-opener-policy")).toBe(
			"same-origin",
		);
		const body = await response.text();
		expect(body.length).toBeGreaterThan(0);
	});

	it("redirects directory requests to a trailing slash", async () => {
		server = createBrowserPlaygroundServer();
		const port = await listenOnRandomPort(server);

		const response = await fetch(`http://127.0.0.1:${port}/frontend`, {
			redirect: "manual",
		});

		expect(response.status).toBe(308);
		expect(response.headers.get("location")).toBe("/frontend/");
	});

	it("serves the runtime harness page with the browser isolation headers", async () => {
		server = createBrowserPlaygroundServer();
		const port = await listenOnRandomPort(server);

		const response = await fetch(
			`http://127.0.0.1:${port}/frontend/runtime-harness.html`,
		);

		expect(response.status).toBe(200);
		expect(response.headers.get("content-type")).toBe(
			"text/html; charset=utf-8",
		);
		expect(response.headers.get("cross-origin-embedder-policy")).toBe(
			"require-corp",
		);
		expect(response.headers.get("cross-origin-opener-policy")).toBe(
			"same-origin",
		);
		expect(await response.text()).toContain("/dist/runtime-harness.js");
	});
});
