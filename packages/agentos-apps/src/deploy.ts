import { createClient } from "rivetkit/client";
import {
	provisionAppNamespace,
	resolveDefaultRivetConnection,
} from "./control-plane.js";
import { AgentOSAppsError } from "./errors.js";
import { appRunnerPool } from "./runtime.js";
import { prepareSource } from "./source.js";
import type {
	DeployAppInput,
	Deployment,
	PreparedDeployAppInput,
} from "./types.js";

interface DeploymentHandle {
	deploy(
		input: PreparedDeployAppInput,
	): Promise<Deployment & { appActorId: string; usesRivetKit: boolean }>;
}

export interface DeployAppOptions {
	/** An ordinary RivetKit client. The default client is created lazily. */
	client?: {
		agentOSAppsApp: {
			getOrCreate(key: string | string[]): DeploymentHandle;
		};
	};
}

let defaultClient: NonNullable<DeployAppOptions["client"]> | undefined;
const HOST_REGISTRY_READY_TIMEOUT_MS = 15_000;
const HOST_REGISTRY_RETRY_DELAY_MS = 50;

function getDefaultClient(): NonNullable<DeployAppOptions["client"]> {
	defaultClient ??= createClient() as unknown as NonNullable<
		DeployAppOptions["client"]
	>;
	return defaultClient;
}

export async function deployApp(
	input: DeployAppInput,
	options: DeployAppOptions = {},
): Promise<Deployment> {
	const files = await prepareSource(input);
	const connection = resolveDefaultRivetConnection();
	const runtime = input.createNamespace
		? await provisionAppNamespace(input.appId, connection)
		: {
				endpoint: connection.endpoint,
				namespace: connection.namespace,
				pool: appRunnerPool(input.appId),
			};
	const client = options.client ?? getDefaultClient();
	const app = client.agentOSAppsApp.getOrCreate([input.appId]);
	const result = await deployWhenHostRegistryIsReady(app, {
		appId: input.appId,
		files,
		regions: input.regions,
		scaling: input.scaling,
		namespace: runtime.namespace,
		runtime: {
			endpoint: runtime.endpoint,
			pool: runtime.pool,
		},
	});
	return {
		appId: input.appId,
		release: result.release,
		namespace: result.namespace,
		pool: runtime.pool,
		regions: result.regions,
	};
}

async function deployWhenHostRegistryIsReady(
	app: DeploymentHandle,
	input: PreparedDeployAppInput,
): Promise<Deployment & { appActorId: string; usesRivetKit: boolean }> {
	const deadline = Date.now() + HOST_REGISTRY_READY_TIMEOUT_MS;
	let lastError: unknown;

	do {
		try {
			return await app.deploy(input);
		} catch (error) {
			if (getErrorCode(error) !== "no_runner_config_configured") throw error;
			lastError = error;
			await new Promise((resolve) =>
				setTimeout(resolve, HOST_REGISTRY_RETRY_DELAY_MS),
			);
		}
	} while (Date.now() < deadline);

	throw new AgentOSAppsError(
		"host_registry_not_ready",
		`AgentOS Apps could not reach the host actor runner within ${HOST_REGISTRY_READY_TIMEOUT_MS}ms. Call registry.start() before deployApp().`,
		{
			timeoutMs: HOST_REGISTRY_READY_TIMEOUT_MS,
			lastCode: getErrorCode(lastError),
		},
	);
}

function getErrorCode(error: unknown): string | undefined {
	if (typeof error !== "object" || error === null || !("code" in error)) {
		return undefined;
	}
	return typeof error.code === "string" ? error.code : undefined;
}

/** @internal Test-only reset for verifying lazy client creation. */
export function resetDefaultAppsClientForTest(): void {
	defaultClient = undefined;
}
