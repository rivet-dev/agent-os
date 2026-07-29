import { execFile } from "node:child_process";
import {
	mkdir,
	mkdtemp,
	readFile,
	rename,
	rm,
	writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { setup } from "@rivet-dev/agentos";
import { packAospkgFromTarBytes } from "@rivet-dev/agentos-toolchain";
import {
	appsRouter,
	deployApp,
} from "@rivet-dev/agentos-apps";
import { createClient } from "rivetkit/client";
import { runLoadTest } from "../../../../benchmarks/agentos-apps/src/load.js";
import {
	createAppsActors,
	normalizeScaling,
} from "../../../../packages/agentos-apps/src/actors.js";
import { provisionAppNamespace } from "../../../../packages/agentos-apps/src/control-plane.js";
import {
	appRunnerPool,
	canonicalDeploymentHash,
	runnerSource,
} from "../../../../packages/agentos-apps/src/runtime.js";

const execFileAsync = promisify(execFile);

const fast = process.env.AGENTOS_APPS_E2E_FAST === "1";
const buildOnly = process.env.AGENTOS_APPS_E2E_BUILD_ONLY === "1";
const artifactCacheDirectory =
	process.env.AGENTOS_APPS_E2E_ARTIFACT_CACHE;
const appsActors = createAppsActors({
	artifactCache: artifactCacheDirectory
		? {
				async get(release) {
					try {
						return await readFile(
							join(artifactCacheDirectory, `${release}.aospkg`),
						);
					} catch (error) {
						if (
							typeof error === "object" &&
							error !== null &&
							"code" in error &&
							error.code === "ENOENT"
						) {
							return undefined;
						}
						throw error;
					}
				},
				async put(release, artifact) {
					await mkdir(artifactCacheDirectory, {
						recursive: true,
					});
					const target = join(
						artifactCacheDirectory,
						`${release}.aospkg`,
					);
					const temporary = `${target}.${process.pid}.tmp`;
					await writeFile(temporary, artifact);
					await rename(temporary, target);
				},
			}
		: undefined,
});

export const registry = setup({
	use: {
		...appsActors,
	},
});

registry.start();

await verify();

async function verify(): Promise<void> {
	try {
		if (buildOnly) {
			await verifyBuildScriptDiagnosticsAndRollback();
			return;
		}
		const localRivetKitTarball =
			process.env.AGENTOS_APPS_RIVETKIT_TARBALL;
		const hello = await deployApp({
			appId: "agentos-apps-hello-e2e",
			source: new URL(
				"../../../../examples/apps-hello-world/fixtures/app/",
				import.meta.url,
			),
		});
		const helloResponse = await appsRouter.request(
			"/agentos-apps-hello-e2e/",
		);
		if (
			!helloResponse.ok ||
			!(await helloResponse.text()).includes("Hello from agentOS Apps")
		) {
			throw new Error("dependency-free hello-world example returned the wrong body");
		}
		if (fast) {
			const simple = await deployApp({
				appId: "agentos-apps-simple-e2e",
				files: {
					"index.html": "<h1>AgentOS Apps packaging works</h1>",
				},
			});
			const simpleResponse = await appsRouter.request(
				"/agentos-apps-simple-e2e/",
			);
			if (!simpleResponse.ok || !(await simpleResponse.text()).includes("works")) {
				throw new Error("minimal build-VM bundle did not serve its static asset");
			}
			console.log(
				JSON.stringify({
					simpleBundle: true,
					simpleRelease: simple.release,
				}),
			);
		}

		const rivetKitFiles = {
			"package.json": localRivetKitTarball
				? JSON.stringify({
						name: "sqlite-notes-app",
						version: "0.0.0",
						private: true,
						type: "module",
						main: "src/index.ts",
						dependencies: {
							rivetkit: "file:./vendor/rivetkit.tgz",
							"@rivetkit/rivetkit-wasm": "2.3.9",
						},
					})
				: await readFile(
						new URL(
							"../../../../examples/apps-sqlite/fixtures/app/package.json",
							import.meta.url,
						),
					),
			"src/index.ts": await readFile(
				new URL(
					"../../../../examples/apps-sqlite/fixtures/app/src/index.ts",
					import.meta.url,
				),
			),
			...(localRivetKitTarball
				? { "vendor/rivetkit.tgz": await readFile(localRivetKitTarball) }
				: {}),
		};
		if (fast && artifactCacheDirectory) {
			const runtime = await provisionAppNamespace("agentos-apps-e2e");
			const release = canonicalDeploymentHash({
				files: encodeFiles(rivetKitFiles),
				entrypoint: "src/index.ts",
				build: false,
				packagingIdentity:
					"apps-builder@0.0.1;manifest@1;bundle@2;esbuild-wasm@0.27.4;rivetkit-adapter@6",
				deploymentIdentity: JSON.stringify({
					regions: ["default"],
					scaling: normalizeScaling({
						maxReplicas: 2,
						targetConcurrency: 2,
					}),
					namespace: runtime.namespace,
					runtime: {
						endpoint: runtime.endpoint,
						pool: appRunnerPool("agentos-apps-e2e"),
					},
					usesRivetKit: true,
				}),
			});
			await cacheRivetKitArtifact(
				rivetKitFiles,
				artifactCacheDirectory,
				release,
			);
		}
		const deployment = await deployApp({
			appId: "agentos-apps-e2e",
			createNamespace: true,
			files: rivetKitFiles,
			scaling: {
				maxReplicas: 2,
				targetConcurrency: 2,
			},
		});
		const guest = createClient({
			namespace: deployment.namespace,
			poolName: deployment.pool,
		}) as any;
		const notes = guest.notes.getOrCreate(["shared"]);
		await notes.add("first");
		const first = (await notes.list()) as unknown[];
		await notes.add("second");
		const second = (await notes.list()) as unknown[];
		if (second.length !== first.length + 1) {
			throw new Error(
				`DirectActor SQLite state did not advance: ${first.length} -> ${second.length}`,
			);
		}
		const firstResponse = await appsRouter.request("/agentos-apps-e2e/");
		if (!firstResponse.ok) {
			throw new Error(
				`first HTTP request failed with ${firstResponse.status}`,
			);
		}
		const firstBody = (await firstResponse.json()) as {
			app?: unknown;
			message?: unknown;
		};
		if (
			firstBody.app !== "sqlite-notes" ||
			typeof firstBody.message !== "string"
		) {
			throw new Error(
				"the packed RivetKit SQLite application returned the wrong body",
			);
		}
		const load = await runLoadTest(
			{
				target: "http://agentos-apps.test/agentos-apps-e2e",
				concurrency: 8,
				durationSeconds: 30,
				timeoutMs: 10_000,
				maxRequests: 64,
				maxSamples: 64,
				maxResponseBytes: 1_024,
				maxReplicaSeries: 128,
				minSuccessRate: 1,
			},
			async () => appsRouter.request("/agentos-apps-e2e/"),
		);
		if (
			load.completed !== 64 ||
			load.successRate !== 1 ||
			load.replicaHeaderCoverage !== 1
		) {
			throw new Error(`bounded load test failed: ${JSON.stringify(load)}`);
		}
		const control = createClient<typeof registry>();
		const resolution = await control.agentOSAppsApp
			.getOrCreate(["agentos-apps-e2e"])
			.resolveDeployment();
		const scaler = control.agentOSAppsScaler.getOrCreate(resolution.scalerKey);
		const scaleDeadline = Date.now() + 30_000;
		let scalerState = await scaler.inspect();
		while (
			scalerState.readyReplicas.length < 2 &&
			Date.now() < scaleDeadline
		) {
			await new Promise((resolve) => setTimeout(resolve, 100));
			scalerState = await scaler.inspect();
		}
		if (scalerState.readyReplicas.length < 2) {
			throw new Error(
				`autoscaler did not produce a second ready replica: ${JSON.stringify(scalerState)}`,
			);
		}
		const scaledLoad = await runLoadTest(
			{
				target: "http://agentos-apps.test/agentos-apps-e2e",
				concurrency: 4,
				durationSeconds: 10,
				timeoutMs: 10_000,
				maxRequests: 16,
				maxSamples: 16,
				maxResponseBytes: 1_024,
				maxReplicaSeries: 2,
				minSuccessRate: 1,
			},
			async () => appsRouter.request("/agentos-apps-e2e/"),
		);
		if (
			scaledLoad.completed !== 16 ||
			scaledLoad.successRate !== 1 ||
			scaledLoad.maximumReplicaCount < 2 ||
			Object.keys(scaledLoad.replicas).length < 2
		) {
			throw new Error(
				`scaled routing test failed: ${JSON.stringify(scaledLoad)}`,
			);
		}
		if (fast) {
			console.log(
				JSON.stringify(
						{
							hello,
							deployment,
							realRivetKitPackage: true,
							localRivetKitTarball: Boolean(localRivetKitTarball),
							directActorRowCounts: [first.length, second.length],
						load,
						scaledLoad,
						fast: true,
					},
					null,
					2,
				),
			);
			return;
		}

		scalerState = await scaler.inspect();
		const oldReplica = scalerState.readyReplicas[0]?.key;
		if (!oldReplica) throw new Error("regional scaler has no ready replica");
		await scaler.drainReplica(oldReplica);

		const coldResponse = await appsRouter.request("/agentos-apps-e2e/");
		if (!coldResponse.ok) {
			throw new Error(
				`cold HTTP request failed with ${coldResponse.status}`,
			);
		}
		await coldResponse.arrayBuffer();
		const third = (await notes.list()) as unknown[];
		if (third.length !== second.length) {
			throw new Error(
				`DirectActor SQLite state was lost across replica replacement: ${second.length} -> ${third.length}`,
			);
		}

		let failedBuild = false;
		try {
			await deployApp({
				appId: "agentos-apps-e2e",
				createNamespace: true,
				files: {
					"package.json": JSON.stringify({
						private: true,
						type: "module",
						main: "dist/index.js",
						scripts: { build: "tsc" },
						devDependencies: { typescript: "5.7.3" },
					}),
					"tsconfig.json": JSON.stringify({
						compilerOptions: {
							target: "ES2022",
							module: "NodeNext",
							moduleResolution: "NodeNext",
							outDir: "dist",
							strict: true,
						},
						include: ["src"],
					}),
					"src/index.ts":
						'const invalid: string = 42; export default () => new Response(invalid);',
				},
			});
		} catch (error) {
			failedBuild = true;
			assertTypeScriptDiagnostic(error);
		}
		if (!failedBuild) throw new Error("the invalid TypeScript build succeeded");

		const rollbackResponse = await appsRouter.request("/agentos-apps-e2e/");
		if (!rollbackResponse.ok) {
			throw new Error(
				`active release was lost after failed build: ${rollbackResponse.status}`,
			);
		}

		console.log(
			JSON.stringify(
				{
					hello,
					deployment,
					realRivetKitPackage: true,
					localRivetKitTarball: Boolean(localRivetKitTarball),
					directActorRowCounts: [first.length, second.length, third.length],
					load,
					replacedReplica: oldReplica.join("/"),
					coldStart:
						coldResponse.headers.get("x-agentos-app-cold-start") === "1",
					failedBuildPreservedActiveRelease: true,
				},
				null,
				2,
			),
		);
	} finally {
		// The parent test harness owns Engine teardown. A graceful registry drain
		// waits on RivetKit's intentionally long-lived serverless /start stream.
	}
}

async function verifyBuildScriptDiagnosticsAndRollback(): Promise<void> {
	const appId = "agentos-apps-build-e2e";
	await deployApp({
		appId,
		files: {
			"index.html": "<h1>previous release</h1>",
		},
	});
	const before = await appsRouter.request(`/${appId}/`);
	if (!before.ok || !(await before.text()).includes("previous release")) {
		throw new Error("initial release did not become active");
	}

	let diagnostic = "";
	try {
		await deployApp({
			appId,
			files: {
				"package.json": JSON.stringify({
					private: true,
					type: "module",
					main: "dist/index.js",
					scripts: { build: "tsc" },
					devDependencies: { typescript: "5.7.3" },
				}),
				"tsconfig.json": JSON.stringify({
					compilerOptions: {
						target: "ES2022",
						module: "NodeNext",
						moduleResolution: "NodeNext",
						outDir: "dist",
						strict: true,
					},
					include: ["src"],
				}),
				"src/index.ts":
					'const invalid: string = 42; export default () => new Response(invalid);',
			},
		});
	} catch (error) {
		diagnostic = assertTypeScriptDiagnostic(error);
	}
	if (!diagnostic) throw new Error("the invalid TypeScript build succeeded");

	const after = await appsRouter.request(`/${appId}/`);
	if (!after.ok || !(await after.text()).includes("previous release")) {
		throw new Error("failed build replaced the previous active release");
	}
	console.log(
		JSON.stringify({
			buildScriptExecuted: true,
			typeScriptDiagnostic: "TS2322",
			failedBuildPreservedActiveRelease: true,
		}),
	);
}

function assertTypeScriptDiagnostic(error: unknown): string {
	const diagnostic =
		error instanceof Error
			? `${error.message}\n${JSON.stringify(error)}`
			: JSON.stringify(error);
	if (diagnostic.length > 64 * 1024) {
		throw new Error("TypeScript diagnostics were not bounded");
	}
	if (!diagnostic.includes("TS2322")) {
		throw new Error(
			`build failed before TypeScript produced its diagnostic: ${diagnostic}`,
		);
	}
	return diagnostic;
}

async function cacheRivetKitArtifact(
	files: Record<string, string | Uint8Array>,
	cacheDirectory: string,
	release: string,
): Promise<{ release: string; bytes: number }> {
	const encodedFiles = encodeFiles(files);
	const target = join(cacheDirectory, `${release}.aospkg`);
	try {
		const cached = await readFile(target);
		return { release, bytes: cached.byteLength };
	} catch (error) {
		if (
			typeof error !== "object" ||
			error === null ||
			!("code" in error) ||
			error.code !== "ENOENT"
		) {
			throw error;
		}
	}

	const root = await mkdtemp(join(tmpdir(), "agentos-apps-host-bundle-"));
	const workspace = join(root, "workspace");
	const releaseDirectory = join(root, "release");
	try {
		for (const [path, content] of Object.entries(encodedFiles)) {
			const destination = join(workspace, path);
			await mkdir(dirname(destination), { recursive: true });
			await writeFile(destination, content);
		}
			const packageJson = JSON.parse(
				await readFile(join(workspace, "package.json"), "utf8"),
			);
			packageJson.dependencies = {
				...(packageJson.dependencies ?? {}),
				"@rivetkit/rivetkit-wasm":
					packageJson.dependencies?.["@rivetkit/rivetkit-wasm"] ?? "2.3.9",
			};
			packageJson.overrides = {
			...(packageJson.overrides ?? {}),
			"@rivet-dev/agent-os-core": "npm:empty-npm-package@1.0.0",
			"@rivetkit/engine-cli": "npm:empty-npm-package@1.0.0",
			"@rivetkit/rivetkit-napi": "npm:empty-npm-package@1.0.0",
		};
		await writeFile(
			join(workspace, "package.json"),
			JSON.stringify(packageJson),
		);
		await writeFile(
			join(workspace, "runner.mjs"),
			runnerSource({
				entrypoint: "src/index.ts",
				release,
				port: 3080,
				maxRequestBytes: 1024 * 1024,
				maxResponseBytes: 4 * 1024 * 1024,
				usesRivetKit: true,
			}),
		);
		await execFileAsync(
			"npm",
			[
				"install",
				"--install-strategy=shallow",
				"--omit=optional",
				"--omit=peer",
				"--legacy-peer-deps",
				"--ignore-scripts",
				"--no-audit",
				"--no-fund",
				"--loglevel=error",
			],
			{ cwd: workspace },
		);
		const configPath = join(root, "bundle.json");
		await writeFile(
			configPath,
			JSON.stringify({
				version: release,
				workspace,
				release: releaseDirectory,
				entrypoint: "runner.mjs",
				sourceFiles: Object.keys(files),
				usesRivetKit: true,
				maxOutputBytes: 64 * 1024 * 1024,
				maxOutputFiles: 4096,
				maxFileBytes: 32 * 1024 * 1024,
			}),
		);
		const builder = fileURLToPath(
			new URL(
				"../../../../software/apps-builder/cli/apps-builder.mjs",
				import.meta.url,
			),
		);
		await execFileAsync(process.execPath, [builder, configPath]);
		const sourceTarPath = join(root, "release.tar");
		await execFileAsync(
			"tar",
			[
				"--sort=name",
				"--mtime=@0",
				"--owner=0",
				"--group=0",
				"--numeric-owner",
				"-cf",
				sourceTarPath,
				".",
			],
			{ cwd: releaseDirectory },
		);
		const packed = packAospkgFromTarBytes(await readFile(sourceTarPath)).bytes;
		if (packed.byteLength >= 8 * 1024 * 1024) {
			throw new Error(
				`RivetKit App Bundle regressed to ${packed.byteLength} bytes`,
			);
		}
		await mkdir(cacheDirectory, { recursive: true });
		const temporary = `${target}.${process.pid}.tmp`;
		await writeFile(temporary, packed);
		await rename(temporary, target);
		console.log(
			JSON.stringify({
				rivetKitBundle: true,
				release,
				bytes: packed.byteLength,
			}),
		);
		return { release, bytes: packed.byteLength };
	} finally {
		await rm(root, { recursive: true, force: true });
	}
}

function encodeFiles(
	files: Record<string, string | Uint8Array>,
): Record<string, Uint8Array> {
	return Object.fromEntries(
		Object.entries(files).map(([path, content]) => [
			path,
			typeof content === "string" ? new TextEncoder().encode(content) : content,
		]),
	);
}
