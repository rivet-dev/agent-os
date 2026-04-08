/**
 * Node.js worker adapter.
 *
 * Wraps node:worker_threads for spawning Workers.
 * Used by the WasmVM runtime for WASM process execution.
 */

import { Worker } from "node:worker_threads";

export interface WorkerHandle {
	postMessage(data: unknown, transferList?: Transferable[]): void;
	onMessage(handler: (data: unknown) => void): void;
	onError(handler: (err: Error) => void): void;
	onExit(handler: (code: number) => void): void;
	terminate(): Promise<number>;
}

export class NodeWorkerAdapter {
	/**
	 * Spawn a Worker for the given script.
	 */
	static create(
		scriptPath: string | URL,
		options?: { workerData?: unknown },
	): WorkerHandle {
		const worker = new Worker(scriptPath, {
			workerData: options?.workerData,
		});

		return {
			postMessage(data, transferList) {
				worker.postMessage(data, transferList as any);
			},
			onMessage(handler) {
				worker.on("message", handler);
			},
			onError(handler) {
				worker.on("error", handler);
			},
			onExit(handler) {
				worker.on("exit", handler);
			},
			terminate() {
				return worker.terminate();
			},
		};
	}
}
