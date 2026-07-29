import { spawn } from "node:child_process";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { getEnginePath } from "@rivetkit/engine-cli";
import getPort from "get-port";

const fast = process.argv.includes("--fast");
const buildOnly = process.argv.includes("--build-only");
const root = await mkdtemp(join(tmpdir(), "agentos-apps-e2e-"));
const databasePath = join(root, "db");
await mkdir(databasePath, { recursive: true });
const guardPort = await getPort();
const peerPort = await getPort({ exclude: [guardPort] });
const metricsPort = await getPort({ exclude: [guardPort, peerPort] });
const endpoint = `http://127.0.0.1:${guardPort}`;
const peerEndpoint = `http://127.0.0.1:${peerPort}`;
const configPath = join(root, "engine.json");
await writeFile(
	configPath,
	JSON.stringify({
		file_system: { path: databasePath },
		guard: { host: "127.0.0.1", port: guardPort },
		api_peer: { host: "127.0.0.1", port: peerPort },
		metrics: { host: "127.0.0.1", port: metricsPort },
		topology: {
			datacenter_label: 1,
			datacenters: {
				default: {
					datacenter_label: 1,
					is_leader: true,
					public_url: endpoint,
					peer_url: peerEndpoint,
					proxy_url: null,
				},
			},
		},
		telemetry: { enabled: false },
		runtime: { allow_version_rollback: true },
	}),
);

const engine = spawn(getEnginePath(), ["--config", configPath, "start"], {
	stdio: ["ignore", "inherit", "inherit"],
});

try {
	await waitUntilHealthy(endpoint, engine);
	process.env.RIVET_ENDPOINT = endpoint;
	process.env.AGENTOS_APPS_E2E_FAST = fast ? "1" : "0";
	process.env.AGENTOS_APPS_E2E_BUILD_ONLY = buildOnly ? "1" : "0";
	if (fast) {
		process.env.AGENTOS_APPS_E2E_ARTIFACT_CACHE = join(
			tmpdir(),
			"agentos-apps-artifact-cache-v16",
		);
	} else {
		delete process.env.AGENTOS_APPS_E2E_ARTIFACT_CACHE;
	}
	delete process.env.RIVET_ENGINE;
	delete process.env.RIVET_RUN_ENGINE;
	await import("./verify.js");
} finally {
	await stopEngine(engine);
	await rm(root, { recursive: true, force: true });
}
process.exit(0);

async function stopEngine(
	process: ReturnType<typeof spawn>,
): Promise<void> {
	if (process.exitCode !== null) return;
	process.kill("SIGTERM");
	const stopped = await Promise.race([
		new Promise<true>((resolve) => process.once("exit", () => resolve(true))),
		new Promise<false>((resolve) =>
			setTimeout(() => resolve(false), fast ? 1_000 : 10_000),
		),
	]);
	if (stopped) return;
	console.warn("Rivet Engine exceeded the E2E shutdown limit; sending SIGKILL");
	process.kill("SIGKILL");
	await new Promise<void>((resolve) => process.once("exit", () => resolve()));
}

async function waitUntilHealthy(
	endpoint: string,
	process: ReturnType<typeof spawn>,
): Promise<void> {
	const deadline = Date.now() + 30_000;
	let lastError: unknown;
	while (Date.now() < deadline) {
		if (process.exitCode !== null) {
			throw new Error(`Rivet Engine exited with code ${process.exitCode}`);
		}
		try {
			const response = await fetch(`${endpoint}/health`);
			if (response.ok) return;
			lastError = new Error(`health returned ${response.status}`);
		} catch (error) {
			lastError = error;
		}
		await new Promise((resolve) => setTimeout(resolve, 100));
	}
	throw new Error("Rivet Engine did not become healthy", { cause: lastError });
}
