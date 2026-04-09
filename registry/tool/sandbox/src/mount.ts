import type {
	MountConfigJsonObject,
	NativeMountPluginDescriptor,
} from "@rivet-dev/agent-os-core";
import type { SandboxAgent } from "sandbox-agent";

export interface SandboxFsOptions {
	/** A connected SandboxAgent client instance. */
	client: SandboxAgent;
	/** Base path to scope all operations under. Defaults to "/". */
	basePath?: string;
	/** Per-request timeout for sandbox-agent HTTP calls. */
	timeoutMs?: number;
	/** Maximum file size allowed for buffered pread/truncate fallbacks. */
	maxFullReadBytes?: number;
}

export type SandboxMountPluginConfig = MountConfigJsonObject & {
	baseUrl: string;
	token?: string;
	headers?: Record<string, string>;
	basePath?: string;
	timeoutMs?: number;
	maxFullReadBytes?: number;
};

interface SerializableSandboxAgentClient {
	baseUrl?: string;
	token?: string;
	defaultHeaders?: RequestInit["headers"];
}

function normalizeHeaders(
	headers: RequestInit["headers"] | undefined,
): Record<string, string> | undefined {
	if (!headers) {
		return undefined;
	}

	if (headers instanceof Headers) {
		return Object.fromEntries(headers.entries());
	}

	if (Array.isArray(headers)) {
		return Object.fromEntries(
			headers as Iterable<readonly [string, string]>,
		);
	}

	return Object.fromEntries(
		Object.entries(headers).map(([name, value]) => [name, String(value)]),
	);
}

function getSerializableClientConfig(client: SandboxAgent): Pick<
	SandboxMountPluginConfig,
	"baseUrl" | "token" | "headers"
> {
	const serializable = client as unknown as SerializableSandboxAgentClient;
	const baseUrl = serializable.baseUrl?.trim().replace(/\/+$/, "");
	if (!baseUrl) {
		throw new Error(
			"SandboxAgent client does not expose a serializable baseUrl; connect with a standard sandbox-agent client instance",
		);
	}

	return {
		baseUrl,
		...(serializable.token ? { token: serializable.token } : {}),
		...(serializable.defaultHeaders
			? { headers: normalizeHeaders(serializable.defaultHeaders) }
			: {}),
	};
}

/**
 * Create a declarative sandbox-agent mount plugin descriptor.
 *
 * This keeps the legacy helper name while routing first-party sandbox mounts
 * through the native `sandbox_agent` plugin instead of a JS VFS backend.
 */
export function createSandboxFs(
	options: SandboxFsOptions,
): NativeMountPluginDescriptor<SandboxMountPluginConfig> {
	return {
		id: "sandbox_agent",
		config: {
			...getSerializableClientConfig(options.client),
			...(options.basePath ? { basePath: options.basePath } : {}),
			...(options.timeoutMs != null ? { timeoutMs: options.timeoutMs } : {}),
			...(options.maxFullReadBytes != null
				? { maxFullReadBytes: options.maxFullReadBytes }
				: {}),
		},
	};
}
