import type {
	AgentOsSandboxClient,
	AgentOsSandboxProvider,
} from "@rivet-dev/agentos-core";
import {
	SandboxAgent,
	type SandboxProvider as SandboxAgentBackend,
	type SandboxAgentStartOptions,
} from "sandbox-agent";

export type SandboxAgentProviderOptions = Omit<
	SandboxAgentStartOptions,
	"sandbox" | "sandboxId"
>;

interface SandboxAgentTransportInternals {
	baseUrl: string;
	token?: string;
	defaultHeaders?: HeadersInit;
	fetcher?: typeof globalThis.fetch;
	awaitHealthy?(signal?: AbortSignal): Promise<void>;
}

function validateTransportBaseUrl(raw: string): string {
	const normalized = raw.trim().replace(/\/+$/, "");
	if (!normalized) throw new Error("SandboxAgent baseUrl must not be empty");
	const url = new URL(normalized);
	if (!url.hostname || url.search || url.hash) {
		throw new Error(
			"SandboxAgent baseUrl must include a host without a query string or fragment",
		);
	}
	const hostname = url.hostname
		.replace(/^\[/, "")
		.replace(/\]$/, "")
		.toLowerCase();
	const loopback =
		hostname === "localhost" ||
		hostname === "::1" ||
		/^127(?:\.|$)/.test(hostname);
	if (url.protocol !== "http:" && url.protocol !== "https:") {
		throw new Error("SandboxAgent baseUrl must use http or https");
	}
	if (url.protocol !== "https:" && !loopback) {
		throw new Error(
			"SandboxAgent baseUrl must use https unless it targets localhost",
		);
	}
	return normalized;
}

async function requestThroughSandboxAgent(
	client: SandboxAgent,
	path: string,
	init: RequestInit = {},
): Promise<Response> {
	const transport = client as unknown as SandboxAgentTransportInternals;
	await transport.awaitHealthy?.(init.signal ?? undefined);
	if (typeof transport.fetcher !== "function") {
		throw new Error(
			"SandboxAgent client does not expose the authenticated fetch transport required by agentOS",
		);
	}
	const headers = new Headers(transport.defaultHeaders);
	new Headers(init.headers).forEach((value, name) => headers.set(name, value));
	if (transport.token) {
		headers.set("authorization", `Bearer ${transport.token}`);
	}
	return await transport.fetcher(
		new URL(`${validateTransportBaseUrl(transport.baseUrl)}${path}`),
		{
			...init,
			headers,
			redirect: "manual",
		},
	);
}

/** Adapt any sandbox-agent backend into a per-VM agentOS sandbox provider. */
export function sandboxAgentProvider(
	backend: SandboxAgentBackend,
	options: SandboxAgentProviderOptions = {},
): AgentOsSandboxProvider {
	return {
		async start(): Promise<AgentOsSandboxClient> {
			const client = await SandboxAgent.start({ ...options, sandbox: backend });
			return new Proxy(client, {
				get(target, property) {
					if (property === "dispose") {
						return target.destroySandbox.bind(target);
					}
					if (property === "request") {
						return (path: string, init?: RequestInit) =>
							requestThroughSandboxAgent(target, path, init);
					}
					const value = Reflect.get(target, property, target);
					return typeof value === "function" ? value.bind(target) : value;
				},
			}) as AgentOsSandboxClient;
		},
	};
}
