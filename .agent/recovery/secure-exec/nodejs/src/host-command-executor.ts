/**
 * Host-backed CommandExecutor that delegates to Node.js child_process.
 *
 * Provides real subprocess execution for standalone NodeRuntime users
 * who need child_process.spawn() to work inside the sandbox.
 */

import { spawn as hostSpawn } from "node:child_process";
import type { CommandExecutor, SpawnedProcess } from "@secure-exec/core";

/**
 * Create a CommandExecutor that spawns real host processes via Node.js.
 *
 * Pass to `createNodeDriver({ commandExecutor: createNodeHostCommandExecutor() })`
 * to enable subprocess execution inside the sandbox.
 */
export function createNodeHostCommandExecutor(): CommandExecutor {
	return {
		spawn(
			command: string,
			args: string[],
			options: {
				cwd?: string;
				env?: Record<string, string>;
				onStdout?: (data: Uint8Array) => void;
				onStderr?: (data: Uint8Array) => void;
			},
		): SpawnedProcess {
			// Merge provided env with host PATH/HOME so commands can be found.
			// When the sandbox bridge sends env: {}, the host spawn would
			// otherwise get no PATH and fail to locate commands.
			const env = options.env && Object.keys(options.env).length > 0
				? options.env
				: undefined; // inherit host process.env

			const child = hostSpawn(command, args, {
				cwd: options.cwd,
				env,
				stdio: ["pipe", "pipe", "pipe"],
			});

			if (options.onStdout && child.stdout) {
				child.stdout.on("data", (chunk: Buffer) => {
					options.onStdout!(new Uint8Array(chunk));
				});
			}
			if (options.onStderr && child.stderr) {
				child.stderr.on("data", (chunk: Buffer) => {
					options.onStderr!(new Uint8Array(chunk));
				});
			}

			const exitPromise = new Promise<number>((resolve) => {
				child.on("close", (code) => resolve(code ?? 1));
				child.on("error", () => resolve(1));
			});

			return {
				writeStdin(data: Uint8Array | string): void {
					if (child.stdin && !child.stdin.destroyed) {
						child.stdin.write(data);
					}
				},
				closeStdin(): void {
					if (child.stdin && !child.stdin.destroyed) {
						child.stdin.end();
					}
				},
				kill(signal?: number): void {
					try {
						child.kill(signal ?? 15);
					} catch {
						// already exited
					}
				},
				wait(): Promise<number> {
					return exitPromise;
				},
			};
		},
	};
}
