import { createHash } from "node:crypto";
import { controlFetch } from "./control-request.js";
import { AgentOSAppsError } from "./errors.js";
import { appRunnerPool, ensureServerlessRunnerConfig } from "./runtime.js";

const DEFAULT_ENDPOINT = "http://localhost:6420";
const MAX_CONTROL_RESPONSE_BYTES = 1024 * 1024;

export interface ResolvedRivetConnection {
	endpoint: string;
	namespace: string;
	token?: string;
}

async function readBoundedJson(response: Response): Promise<unknown> {
	if (!response.body) return null;
	const reader = response.body.getReader();
	const chunks: Uint8Array[] = [];
	let bytes = 0;
	try {
		for (;;) {
			const { value, done } = await reader.read();
			if (done) break;
			bytes += value.byteLength;
			if (bytes > MAX_CONTROL_RESPONSE_BYTES) {
				await reader.cancel("AgentOS Apps control response limit exceeded");
				throw new AgentOSAppsError(
					"agentos_apps_control_response_limit",
					`Rivet control response exceeded ${MAX_CONTROL_RESPONSE_BYTES} bytes`,
					{ limit: MAX_CONTROL_RESPONSE_BYTES },
				);
			}
			chunks.push(value);
		}
	} finally {
		reader.releaseLock();
	}
	const text = new TextDecoder().decode(Buffer.concat(chunks));
	return text ? JSON.parse(text) : null;
}

/**
 * Resolves the same standard Rivet connection variables as createClient(), but
 * only when a deployment or replica actually needs control-plane information.
 * This is intentionally not called by setupApps() or at module import time.
 */
export function resolveDefaultRivetConnection(): ResolvedRivetConnection {
	const rawEndpoint =
		process.env.RIVET_ENGINE ?? process.env.RIVET_ENDPOINT ?? DEFAULT_ENDPOINT;
	const url = new URL(rawEndpoint);
	const endpointNamespace = url.username
		? decodeURIComponent(url.username)
		: undefined;
	const endpointToken = url.password
		? decodeURIComponent(url.password)
		: undefined;
	url.username = "";
	url.password = "";
	return {
		endpoint: url.toString().replace(/\/$/, ""),
		namespace: endpointNamespace ?? process.env.RIVET_NAMESPACE ?? "default",
		token: endpointToken ?? process.env.RIVET_TOKEN,
	};
}

function namespaceName(appId: string, hostNamespace: string): string {
	const suffix = createHash("sha256")
		.update(hostNamespace)
		.update("\0")
		.update(appId)
		.digest("hex")
		.slice(0, 10);
	return `agentos-app-${appId}`.slice(0, 63 - suffix.length - 1) + `-${suffix}`;
}

function controlHeaders(token?: string): Record<string, string> {
	return {
		accept: "application/json",
		"content-type": "application/json",
		...(token ? { authorization: `Bearer ${token}` } : {}),
	};
}

function serverlessAppUrl(
	appActorId: string,
	connection: ResolvedRivetConnection,
): string {
	const url = new URL(
		`/gateway/${encodeURIComponent(appActorId)}/request/.agentos/apps/rivet`,
		connection.endpoint,
	);
	return url.toString();
}

/** Idempotently provisions the isolated application namespace. */
export async function provisionAppNamespace(
	appId: string,
	connection = resolveDefaultRivetConnection(),
): Promise<{ namespace: string; endpoint: string; pool: string }> {
	const namespace = namespaceName(appId, connection.namespace);
	const headers = controlHeaders(connection.token);
	const lookupUrl = new URL("/namespaces", connection.endpoint);
	lookupUrl.searchParams.set("name", namespace);
	lookupUrl.searchParams.set("limit", "1");
	const lookup = async (): Promise<boolean> => {
		const response = await controlFetch(lookupUrl, {
			headers,
		});
		if (!response.ok) {
			throw new AgentOSAppsError(
				"agentos_apps_namespace_lookup_failed",
				`Rivet namespace lookup failed with HTTP ${response.status}`,
				{ status: response.status },
			);
		}
		const body = (await readBoundedJson(response)) as {
			namespaces?: Array<{ name?: unknown }>;
		};
		return body.namespaces?.some((entry) => entry.name === namespace) ?? false;
	};

	if (!(await lookup())) {
		const response = await controlFetch(
			new URL("/namespaces", connection.endpoint),
			{
				method: "POST",
				headers,
				body: JSON.stringify({
					name: namespace,
					display_name: `AgentOS App ${appId}`,
				}),
			},
		);
		if (!response.ok && !(await lookup())) {
			throw new AgentOSAppsError(
				"agentos_apps_namespace_create_failed",
				`Rivet namespace creation failed with HTTP ${response.status}`,
				{ status: response.status },
			);
		}
	}

	return {
		namespace,
		endpoint: connection.endpoint,
		pool: appRunnerPool(appId),
	};
}

/** Configures the guest pool only after a healthy release has been activated. */
export async function configureAppNamespaceRunner(
	appActorId: string,
	runtime: { endpoint: string; namespace: string; pool: string },
	callbackSecret: string,
	connection = resolveDefaultRivetConnection(),
): Promise<void> {
	try {
		await ensureServerlessRunnerConfig({
			endpoint: runtime.endpoint,
			namespace: runtime.namespace,
			url: serverlessAppUrl(appActorId, connection),
			pool: runtime.pool,
			token: connection.token,
			callbackSecret,
		});
	} catch (error) {
		throw new AgentOSAppsError(
			"agentos_apps_runner_config_failed",
			`Rivet runner configuration failed for namespace ${runtime.namespace} and pool ${runtime.pool}`,
			{
				namespace: runtime.namespace,
				pool: runtime.pool,
				error: error instanceof Error ? error.message : String(error),
			},
		);
	}
}
