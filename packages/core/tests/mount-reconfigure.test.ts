import { describe, expect, it } from "vitest";
import { NativeSidecarKernelProxy } from "../src/sidecar/rpc-client.js";
import { createInMemoryFileSystem } from "../src/test/runtime.js";

// Regression coverage for post-boot mountFs delivery to the native sidecar:
//   1. Rust `configure_vm` rebuilds the whole VM configuration from each
//      payload, so a runtime mount reconfigure that omits the boot `packages` /
//      `packagesMountAt` / `bindingShimCommands` strips the `/opt/agentos`
//      projections and binding shims from the VM as a side effect.
//   2. mountFs used to be fire-and-forget with a swallowed rejection, so a
//      failed reconfigure left the mount silently host-only and callers had no
//      way to know when (or whether) the guest could see it.
// The proxy is exercised against a stub SidecarProcess so the test stays fast
// and deterministic without booting a real VM.

const session = { connectionId: "conn-1", sessionId: "sess-1" };
const vm = { vmId: "vm-test" };

const bootPackages = [
	{
		name: "common",
		version: "1.0.0",
		path: "/tmp/common.aospkg",
	},
];
const bootBindingShims = ["agentos", "agentos-demo"];
const bootRuntimeCommands = ["node", "npm", "python"];

function createStubClient(options?: { failConfigureVm?: boolean }) {
	const configureCalls: Array<Record<string, unknown>> = [];
	const client = {
		async configureVm(
			_session: unknown,
			_vm: unknown,
			payload: Record<string, unknown>,
		) {
			configureCalls.push(payload);
			if (options?.failConfigureVm) {
				throw new Error("configure_vm rejected");
			}
			return {
				appliedMounts: [],
				appliedSoftware: [],
				projectedCommands: [],
				agents: [],
			};
		},
		async disposeVm() {},
		async dispose() {},
		waitForEvent(
			_filter: unknown,
			_unused: unknown,
			opts: { signal: AbortSignal },
		) {
			return new Promise((_resolve, reject) => {
				opts.signal.addEventListener("abort", () =>
					reject(new Error("aborted")),
				);
			});
		},
	};
	return { client, configureCalls };
}

function createProxy(client: unknown) {
	const options = {
		client,
		session,
		vm,
		env: {},
		cwd: "/work",
		localMounts: [],
		sidecarMounts: [],
		packages: bootPackages,
		packagesMountAt: "/opt/agentos",
		bootstrapCommands: bootRuntimeCommands,
		bindingShimCommands: bootBindingShims,
		commandGuestPaths: new Map<string, string>(),
		ownsClient: true,
	};
	return new NativeSidecarKernelProxy(
		options as ConstructorParameters<typeof NativeSidecarKernelProxy>[0],
	);
}

describe("post-boot mount reconfiguration", () => {
	it("resends boot packages and commands on runtime mountFs", async () => {
		const { client, configureCalls } = createStubClient();
		const proxy = createProxy(client);

		await proxy.mountFs("/mnt/dynamic", createInMemoryFileSystem());

		expect(configureCalls).toHaveLength(1);
		const payload = configureCalls[0];
		expect(payload.packages).toEqual(bootPackages);
		expect(payload.packagesMountAt).toBe("/opt/agentos");
		expect(payload.bootstrapCommands).toEqual(bootRuntimeCommands);
		expect(payload.bindingShimCommands).toEqual(bootBindingShims);
		expect(payload.mounts).toEqual([
			expect.objectContaining({ guestPath: "/mnt/dynamic" }),
		]);

		await proxy.unmountFs("/mnt/dynamic");
		expect(configureCalls).toHaveLength(2);
		expect(configureCalls[1].mounts).toEqual([]);
		expect(configureCalls[1].packages).toEqual(bootPackages);
		expect(configureCalls[1].bootstrapCommands).toEqual(bootRuntimeCommands);
		expect(configureCalls[1].bindingShimCommands).toEqual(bootBindingShims);

		await proxy.dispose();
	});

	it("resends runtime-linked packages on later mount reconfigures", async () => {
		const { client, configureCalls } = createStubClient();
		const proxy = createProxy(client);

		// linkSoftware() records the linked package on the proxy; a later
		// mountFs must resend it alongside the boot packages or configure_vm
		// (replace-on-write) unprojects it from /opt/agentos.
		proxy.registerLinkedPackage({ path: "/tmp/linked.aospkg" });
		await proxy.mountFs("/mnt/dynamic", createInMemoryFileSystem());
		expect(configureCalls[0].packages).toEqual([
			...bootPackages,
			{ path: "/tmp/linked.aospkg" },
		]);

		// Duplicate registration is a no-op.
		proxy.registerLinkedPackage({ path: "/tmp/linked.aospkg" });
		await proxy.unmountFs("/mnt/dynamic");
		expect(configureCalls[1].packages).toEqual([
			...bootPackages,
			{ path: "/tmp/linked.aospkg" },
		]);

		await proxy.dispose();
	});

	it("rejects the mountFs promise when sidecar delivery fails", async () => {
		const { client } = createStubClient({ failConfigureVm: true });
		const proxy = createProxy(client);

		await expect(
			proxy.mountFs("/mnt/dynamic", createInMemoryFileSystem()),
		).rejects.toThrow("configure_vm rejected");

		await proxy.dispose();
	});

	it("resolves unmountFs immediately for an unknown mount without reconfiguring", async () => {
		const { client, configureCalls } = createStubClient();
		const proxy = createProxy(client);

		await proxy.unmountFs("/mnt/never-mounted");
		expect(configureCalls).toHaveLength(0);

		await proxy.dispose();
	});
});
