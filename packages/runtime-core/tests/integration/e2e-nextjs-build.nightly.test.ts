/**
 * E2E test: Next.js build through kernel.
 *
 * Verifies that 'next build' completes through the kernel on the repo-owned
 * Next.js fixture, proving the kernel can handle a complex real-world
 * build pipeline:
 *   1. Host-side package install populates node_modules
 *   2. NodeFileSystem mounts the project into the kernel
 *   3. kernel.spawn('node', ['/src/index.js']) runs and validates Next.js
 *   4. The expected Next.js manifests and compiled page/API artifacts exist
 *
 * The fixture's checked-in Babel configuration avoids Next's native SWC addon
 * while still exercising a complete production webpack build.
 */

import { cp, mkdir, mkdtemp, rm, symlink } from "node:fs/promises";
import { execSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, beforeAll, expect, it } from "vitest";
import {
	describeIf,
	COMMANDS_DIR,
	createKernel,
	NodeFileSystem,
	createWasmVmRuntime,
	createNodeRuntime,
	skipUnlessWasmBuilt,
} from "@rivet-dev/agentos-vm-test-harness";

const wasmSkip = skipUnlessWasmBuilt();
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const NEXTJS_FIXTURE_DIR = path.resolve(__dirname, "projects/nextjs-pass");
const NEXTJS_CACHE_DIR = path.resolve(__dirname, "../../.cache/nextjs-build");

/** Check if npm registry is reachable (5s timeout). */
async function checkNetwork(): Promise<string | false> {
	try {
		const controller = new AbortController();
		const timeout = setTimeout(() => controller.abort(), 5_000);
		await fetch("https://registry.npmjs.org/", {
			signal: controller.signal,
			method: "HEAD",
		});
		clearTimeout(timeout);
		return false;
	} catch {
		return "network not available (cannot reach npm registry)";
	}
}

const skipReason = wasmSkip || (await checkNetwork());
void skipReason;

// TODO(P6): Next.js build E2E depends on package-install artifacts.
describeIf(
	process.env.AGENTOS_NPM_WORKFLOWS_E2E === "1",
	"e2e Next.js build through kernel",
	() => {
		let tempDir: string;
		let installDir: string;

		// Copy the checked-in fixture so the build can mutate /.next without touching the repo.
		beforeAll(async () => {
			await mkdir(NEXTJS_CACHE_DIR, { recursive: true });
			tempDir = await mkdtemp(path.join(NEXTJS_CACHE_DIR, "worktree-"));
			installDir = await mkdtemp(path.join(NEXTJS_CACHE_DIR, "install-"));
			await cp(NEXTJS_FIXTURE_DIR, tempDir, { recursive: true });
			await cp(NEXTJS_FIXTURE_DIR, installDir, { recursive: true });

			// Match the full catalog's isolated worktree: install once, then project the
			// prepared node_modules tree through a symlink into the mutable build copy.
			execSync("pnpm install --ignore-workspace --prefer-offline", {
				cwd: installDir,
				stdio: "pipe",
				timeout: 60_000,
			});
			await symlink(
				path.join(installDir, "node_modules"),
				path.join(tempDir, "node_modules"),
				"dir",
			);
		}, 90_000);

		afterAll(async () => {
			if (tempDir) {
				await rm(tempDir, { recursive: true, force: true });
			}
			if (installDir) {
				await rm(installDir, { recursive: true, force: true });
			}
		});

		it("next build produces manifests and compiled page artifacts", async () => {
			const vfs = new NodeFileSystem({ root: tempDir });
			const kernel = createKernel({
				filesystem: vfs,
				cwd: "/",
				limits: {
					reactor: { operationDeadlineMs: 120_000 },
					jsRuntime: {
						cpuTimeLimitMs: 300_000,
						importCacheMaterializeTimeoutMs: 120_000,
					},
				},
			});
			expect(
				new TextDecoder().decode(await vfs.readFile("/.babelrc")),
			).toContain("next/babel");

			await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));
			await kernel.mount(createNodeRuntime());

			try {
				const stdoutChunks: Uint8Array[] = [];
				const stderrChunks: Uint8Array[] = [];
				const process = kernel.spawn("node", ["/src/index.js"], {
					cwd: "/",
					onStdout: (chunk) => stdoutChunks.push(chunk),
					onStderr: (chunk) => stderrChunks.push(chunk),
				});
				const exitCode = await process.wait();
				const result = {
					exitCode,
					stdout: Buffer.concat(stdoutChunks).toString("utf8"),
					stderr: Buffer.concat(stderrChunks).toString("utf8"),
				};
				expect(
					result.exitCode,
					`stdout:\n${result.stdout}\nstderr:\n${result.stderr}`,
				).toBe(0);

				const buildManifest = JSON.parse(
					new TextDecoder().decode(
						await vfs.readFile("/.next/build-manifest.json"),
					),
				);
				const pagesManifest = JSON.parse(
					new TextDecoder().decode(
						await vfs.readFile("/.next/server/pages-manifest.json"),
					),
				);
				const compiledIndex = new TextDecoder().decode(
					await vfs.readFile("/.next/server/pages/index.js"),
				);

				expect(Object.keys(buildManifest.pages)).toContain("/");
				expect(pagesManifest["/"]).toBe("pages/index.js");
				expect(pagesManifest["/api/hello"]).toBe("pages/api/hello.js");
				expect(compiledIndex).toContain("Hello from Next.js");
				await expect(
					vfs.readFile("/.next/server/pages/api/hello.js"),
				).resolves.toBeInstanceOf(Uint8Array);
			} finally {
				await kernel.dispose();
			}
		}, 120_000);
	},
);
