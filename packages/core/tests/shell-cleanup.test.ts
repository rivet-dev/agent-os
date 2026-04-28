import { describe, expect, test, vi } from "vitest";
import { AgentOs } from "../src/index.js";

interface FakeShellEntry {
	handle: {
		pid: number;
		onData: ((data: Uint8Array) => void) | null;
		write(data: Uint8Array | string): void;
		resize(cols: number, rows: number): void;
		kill(signal?: number): void;
		wait(): Promise<number>;
	};
	dataHandlers: Set<(data: Uint8Array) => void>;
	exitPromise: Promise<void>;
}

interface AgentOsShellCleanupBackdoor {
	_shells: Map<string, FakeShellEntry>;
	_pendingShellExitPromises: Set<Promise<void>>;
	_disposeSidecarEventListener: () => void;
}

describe("shell cleanup", () => {
	test("dispose waits for tracked shell exits before removing the sidecar event listener", async () => {
		let vm: AgentOs | null = await AgentOs.create();
		const backdoor = vm as AgentOs & AgentOsShellCleanupBackdoor;
		const textEncoder = new TextEncoder();
		const consoleErrors: string[] = [];
		const consoleErrorSpy = vi
			.spyOn(console, "error")
			.mockImplementation((...args: unknown[]) => {
				consoleErrors.push(args.map((value) => String(value)).join(" "));
			});

		try {
			backdoor._shells.clear();
			backdoor._pendingShellExitPromises.clear();

			for (let index = 0; index < 20; index += 1) {
				const shellId = `shell-${index + 1}`;
				let resolved = false;
				let resolveWait!: (exitCode: number) => void;
				const waitPromise = new Promise<number>((resolve) => {
					resolveWait = resolve;
				});
				const dataHandlers = new Set<(data: Uint8Array) => void>();
				const entry: FakeShellEntry = {
					handle: {
						pid: 10_000 + index,
						onData: null,
						write() {},
						resize() {},
						kill() {
							if (resolved) {
								return;
							}
							resolved = true;
							setTimeout(() => {
								resolveWait(0);
							}, 25);
						},
						wait() {
							return waitPromise;
						},
					},
					dataHandlers,
					exitPromise: Promise.resolve(),
				};

				entry.exitPromise = waitPromise.then(
					() => undefined,
					() => undefined,
				);
				entry.exitPromise = entry.exitPromise.finally(() => {
					backdoor._pendingShellExitPromises.delete(entry.exitPromise);
				});
				backdoor._pendingShellExitPromises.add(entry.exitPromise);
				backdoor._shells.set(shellId, entry);
				vm.onShellData(shellId, () => {});
				for (const handler of dataHandlers) {
					handler(textEncoder.encode(`tick:${shellId}\n`));
				}
			}

			expect(backdoor._pendingShellExitPromises.size).toBe(20);

			let pendingShellsWhenListenerDisposed: number | null = null;
			const originalDisposeSidecarEventListener =
				backdoor._disposeSidecarEventListener;
			backdoor._disposeSidecarEventListener = () => {
				pendingShellsWhenListenerDisposed =
					backdoor._pendingShellExitPromises.size;
				originalDisposeSidecarEventListener();
			};

			await vm.dispose();
			vm = null;

			expect(pendingShellsWhenListenerDisposed).toBe(0);
			expect(backdoor._pendingShellExitPromises.size).toBe(0);
			expect(
				consoleErrors.some((message) => message.includes("bridge closed")),
			).toBe(false);
		} finally {
			consoleErrorSpy.mockRestore();
			if (vm) {
				await vm.dispose();
			}
		}
	}, 30_000);
});
