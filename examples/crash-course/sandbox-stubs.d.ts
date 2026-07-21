/**
 * Local ambient stubs for this example only.
 *
 * The sandbox integration ships as `@rivet-dev/agentos-sandbox`, which depends on
 * the third-party `sandbox-agent` package. Their runtimes pull native/Docker
 * deps, so these declarations model just enough of the public surface for the
 * sandbox example to type-check. Remove once the packages are installed here.
 *
 * Real packages and exports:
 * - `@rivet-dev/agentos-sandbox` -> `createSandboxFs`, `createSandboxBindings`
 * - `sandbox-agent`              -> `SandboxAgent`
 * - `sandbox-agent/docker`       -> `docker`
 */

declare module "sandbox-agent" {
	export class SandboxAgent {
		static start(options: { sandbox: unknown }): Promise<SandboxAgent>;
		dispose(): Promise<void>;
	}
}

declare module "sandbox-agent/docker" {
	export function docker(options?: unknown): unknown;
}

declare module "@rivet-dev/agentos-sandbox" {
	import type {
		NativeMountPluginDescriptor,
		Bindings,
	} from "@rivet-dev/agentos";
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
	export interface SandboxBindingsOptions {
		/** A connected SandboxAgent client instance. */
		client: SandboxAgent;
	}
	/**
	 * Build the mount plugin descriptor that projects the sandbox filesystem into
	 * the VM. Use it as the `plugin` of a `{ path, plugin }` mount entry.
	 */
	export function createSandboxFs(
		options: SandboxFsOptions,
	): NativeMountPluginDescriptor;
	/** Build a binding collection that exposes the sandbox's process management. */
	export function createSandboxBindings(
		options: SandboxBindingsOptions,
	): Bindings;
}
