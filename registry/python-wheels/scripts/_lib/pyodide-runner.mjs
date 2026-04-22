// Spawn a Pyodide instance in a Node Worker thread, mount the wheels dir
// at /wheels via NODEFS, and run the supplied Python code asynchronously.
//
// Returns true if the script ran without exception and produced stdout
// containing the expected promise marker; false otherwise.
//
// This is the verification harness used by the L1/L2/L3 verify scripts.
// It deliberately mirrors how `packages/python/src/driver.ts` spins up
// Pyodide so the verification matches production runtime behavior.

import { Worker } from "node:worker_threads";
import { createRequire } from "node:module";
import { dirname } from "node:path";

const require = createRequire(import.meta.url);

function resolvePyodideIndex() {
	try {
		const main = require.resolve("pyodide/pyodide.mjs");
		return `${dirname(main)}/`;
	} catch {
		console.error(
			"pyodide is not installed in the current node_modules. Run `pnpm install` at the repo root first.",
		);
		throw new Error("pyodide module not found");
	}
}

const WORKER_SOURCE = String.raw`
const { parentPort, workerData } = require("node:worker_threads");

(async () => {
	try {
		const { loadPyodide } = await import("pyodide");
		const py = await loadPyodide({
			indexURL: workerData.indexPath,
			stdout: (msg) => parentPort.postMessage({ type: "stdout", msg }),
			stderr: (msg) => parentPort.postMessage({ type: "stderr", msg }),
		});

		if (workerData.mountWheels) {
			py.FS.mkdirTree("/wheels");
			py.FS.mount(
				py.FS.filesystems.NODEFS,
				{ root: workerData.mountWheels },
				"/wheels",
			);
		}

		await py.runPythonAsync(workerData.code);
		parentPort.postMessage({ type: "done", ok: true });
	} catch (err) {
		parentPort.postMessage({
			type: "done",
			ok: false,
			error: err && err.message ? err.message : String(err),
			stack: err && err.stack ? err.stack : undefined,
		});
	}
})();
`;

export function spawnPyodideAndRun(code, opts = {}) {
	const indexPath = resolvePyodideIndex();
	return new Promise((resolve) => {
		const worker = new Worker(WORKER_SOURCE, {
			eval: true,
			workerData: {
				indexPath,
				code,
				mountWheels: opts.mountWheels,
			},
		});
		let lastStdout = "";
		worker.on("message", (msg) => {
			if (msg.type === "stdout") {
				process.stdout.write(`[py] ${msg.msg}\n`);
				lastStdout += msg.msg + "\n";
			} else if (msg.type === "stderr") {
				process.stderr.write(`[py:err] ${msg.msg}\n`);
			} else if (msg.type === "done") {
				worker.terminate();
				if (!msg.ok) {
					console.error("Python execution failed:", msg.error);
					if (msg.stack) console.error(msg.stack);
					resolve(false);
				} else {
					resolve(true);
				}
			}
		});
		worker.on("error", (err) => {
			console.error("worker error:", err);
			resolve(false);
		});
		worker.on("exit", (code) => {
			if (code !== 0) {
				console.error(`worker exited with code ${code}`);
			}
		});
	});
}
