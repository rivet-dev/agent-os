import { describe, expect, it } from "vitest";
import {
	type LocalCompatMount,
	NativeSidecarKernelProxy,
} from "../src/sidecar/rpc-client.js";

// Regression coverage for the NativeSidecarKernelProxy tracking-collection leaks:
//   H6 - trackedProcesses / trackedProcessesById and the onStdout/onStderr
//        listener Sets were populated at spawn but never released on exit.
//   M8 - signalStates kept a per-pid entry forever (its sibling signalRefreshes
//        was already deleted on process_exited).
//   H7 - localMounts was never cleared on dispose().
// The proxy is exercised against a stub SidecarProcess so the test stays fast and
// deterministic without booting a real VM.

const session = { connectionId: "conn-1", sessionId: "sess-1" };
const vm = { vmId: "vm-test" };

interface PumpEvent {
	ownership: { scope: string; vm_id: string };
	payload: Record<string, unknown>;
}

function createStubClient() {
	const queue: PumpEvent[] = [];
	const stdinWrites: unknown[] = [];
	let stdinCloseCount = 0;
	let notify: (() => void) | null = null;

	const client = {
		async execute() {
			return { pid: 4242 };
		},
		async getProcessSnapshot() {
			return [];
		},
		async getSignalState() {
			return { handlers: new Map() };
		},
		async killProcess() {},
		async writeStdin(
			_session: unknown,
			_vm: unknown,
			_processId: unknown,
			data: unknown,
		) {
			stdinWrites.push(data);
		},
		async closeStdin() {
			stdinCloseCount += 1;
		},
		async disposeVm() {},
		async dispose() {},
		waitForEvent(
			_filter: unknown,
			_unused: unknown,
			options: { signal: AbortSignal },
		) {
			return new Promise<PumpEvent>((resolve, reject) => {
				const tryDeliver = () => {
					const event = queue.shift();
					if (event) {
						resolve(event);
						return true;
					}
					return false;
				};
				if (tryDeliver()) {
					return;
				}
				notify = () => {
					if (tryDeliver()) {
						notify = null;
					}
				};
				options.signal.addEventListener("abort", () =>
					reject(new Error("aborted")),
				);
			});
		},
	};

	const pushEvent = (event: PumpEvent) => {
		queue.push(event);
		notify?.();
	};

	return {
		client,
		pushEvent,
		stdinWrites,
		stdinCloseCount: () => stdinCloseCount,
	};
}

function createProxy(client: unknown, localMounts: LocalCompatMount[] = []) {
	const options = {
		client,
		session,
		vm,
		env: {},
		cwd: "/work",
		localMounts,
		sidecarMounts: [],
		commandGuestPaths: new Map<string, string>(),
		ownsClient: true,
	};
	return new NativeSidecarKernelProxy(
		options as ConstructorParameters<typeof NativeSidecarKernelProxy>[0],
	);
}

async function waitFor(predicate: () => boolean, timeoutMs = 500) {
	const start = Date.now();
	while (Date.now() - start < timeoutMs) {
		if (predicate()) {
			return;
		}
		await new Promise((resolve) => setTimeout(resolve, 5));
	}
}

describe("NativeSidecarKernelProxy tracking-collection cleanup", () => {
	it("forwards initial stdin and closes non-streaming process input", async () => {
		const stub = createStubClient();
		const proxy = createProxy(stub.client);

		proxy.spawn("node", ["script.js"], {
			stdin: "initial input",
			streamStdin: false,
		});

		await waitFor(() => stub.stdinCloseCount() === 1);
		expect(stub.stdinWrites).toEqual(["initial input"]);
		expect(stub.stdinCloseCount()).toBe(1);
		await proxy.dispose();
	});

	it("releases tracked process + signal state + listeners when a process exits", async () => {
		const { client, pushEvent } = createStubClient();
		const proxy = createProxy(client);

		const proc = proxy.spawn("node", ["script.js"], {
			onStdout: () => {},
			onStderr: () => {},
		});

		// Populate signalStates the same way the kernel does (getSignalState ->
		// refreshSignalState), so we can prove it is released on exit.
		proxy.getSignalState(proc.pid);
		await proxy.__awaitSignalRefreshesForTest();

		const entry = proxy.__trackedEntryForTest(proc.pid);
		expect(entry).toBeDefined();
		expect(entry?.onStdout.size).toBe(1);
		expect(entry?.onStderr.size).toBe(1);

		const before = proxy.__trackingSizesForTest();
		expect(before.trackedProcesses).toBe(1);
		expect(before.trackedProcessesById).toBe(1);
		expect(before.signalStates).toBe(1);

		// Drive the real event-pump exit path (the sibling signalRefreshes delete
		// already lives here; signalStates must be released alongside it).
		pushEvent({
			ownership: { scope: "vm", vm_id: vm.vmId },
			payload: {
				type: "process_exited",
				process_id: `proc-${proc.pid}`,
				exit_code: 0,
			},
		});

		await waitFor(() => proxy.__trackingSizesForTest().trackedProcesses === 0);

		const after = proxy.__trackingSizesForTest();
		expect(after.trackedProcesses).toBe(0);
		expect(after.trackedProcessesById).toBe(0);
		expect(after.signalStates).toBe(0);
		// The listener Sets on the (now untracked) entry must be emptied too.
		expect(entry?.onStdout.size).toBe(0);
		expect(entry?.onStderr.size).toBe(0);

		await proxy.dispose();
	});

	it("clears all tracking state and local mounts on dispose", async () => {
		const { client } = createStubClient();
		const localMount: LocalCompatMount = {
			path: "/mnt/data",
			fs: {} as LocalCompatMount["fs"],
			readOnly: false,
		};
		const proxy = createProxy(client, [localMount]);

		const proc = proxy.spawn("node", ["server.js"], {
			onStdout: () => {},
		});
		proxy.getSignalState(proc.pid);
		await proxy.__awaitSignalRefreshesForTest();

		const before = proxy.__trackingSizesForTest();
		expect(before.trackedProcesses).toBe(1);
		expect(before.localMounts).toBe(1);
		expect(before.signalStates).toBe(1);

		// Dispose with a still-live process: every collection must end up empty.
		await proxy.dispose();

		const after = proxy.__trackingSizesForTest();
		expect(after.trackedProcesses).toBe(0);
		expect(after.trackedProcessesById).toBe(0);
		expect(after.signalStates).toBe(0);
		expect(after.signalRefreshes).toBe(0);
		expect(after.localMounts).toBe(0);
	});
});
