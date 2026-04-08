/**
 * Sandbox-native command executor for standalone NodeRuntime.
 *
 * Routes `node` commands (and `bash -c "node ..."` wrappers) through
 * child NodeExecutionDriver instances without spawning host processes.
 * Non-node commands still throw ENOSYS.
 */

import type {
	CommandExecutor,
	SpawnedProcess,
	SystemDriver,
	NodeRuntimeDriverFactory,
	NodeRuntimeDriver,
} from "@secure-exec/core";

// Simple shell tokenizer for `bash -c "command"` extraction
function parseShellCommand(
	cmd: string,
): { command: string; args: string[] } | null {
	const tokens: string[] = [];
	let current = "";
	let inSingle = false;
	let inDouble = false;
	let escaped = false;

	for (const char of cmd.trim()) {
		if (escaped) {
			current += char;
			escaped = false;
			continue;
		}
		if (char === "\\" && !inSingle) {
			escaped = true;
			continue;
		}
		if (char === "'" && !inDouble) {
			inSingle = !inSingle;
			continue;
		}
		if (char === '"' && !inSingle) {
			inDouble = !inDouble;
			continue;
		}
		if ((char === " " || char === "\t") && !inSingle && !inDouble) {
			if (current) {
				tokens.push(current);
				current = "";
			}
			continue;
		}
		current += char;
	}
	if (current) tokens.push(current);

	if (tokens.length === 0) return null;
	return { command: tokens[0], args: tokens.slice(1) };
}

function isNodeCommand(command: string): boolean {
	return (
		command === "node" ||
		command === "/usr/bin/node" ||
		command === "/usr/local/bin/node"
	);
}

function isShellCommand(command: string): boolean {
	return (
		command === "bash" ||
		command === "/bin/bash" ||
		command === "sh" ||
		command === "/bin/sh"
	);
}

/**
 * Create a command executor that routes `node` commands through child
 * V8 isolates. Shell wrappers (`bash -c "node ..."`) are unwrapped
 * automatically. Non-node commands throw ENOSYS.
 */
export function createSandboxCommandExecutor(
	factory: NodeRuntimeDriverFactory,
	baseSystemDriver: SystemDriver,
): CommandExecutor {
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
			// Direct node invocation: node -e "code"
			if (isNodeCommand(command)) {
				return spawnNodeChild(factory, baseSystemDriver, args, options);
			}

			// Shell wrapper: bash -c "node -e ..."
			if (
				isShellCommand(command) &&
				args[0] === "-c" &&
				args.length >= 2
			) {
				const innerCmd = args.slice(1).join(" ");
				const parsed = parseShellCommand(innerCmd);
				if (parsed && isNodeCommand(parsed.command)) {
					return spawnNodeChild(
						factory,
						baseSystemDriver,
						parsed.args,
						options,
					);
				}
			}

			// Non-node commands not supported in standalone sandbox mode
			const err = new Error(
				"ENOSYS: function not implemented, spawn",
			) as NodeJS.ErrnoException;
			err.code = "ENOSYS";
			err.errno = -38;
			err.syscall = "spawn";
			throw err;
		},
	};
}

function spawnNodeChild(
	factory: NodeRuntimeDriverFactory,
	baseSystemDriver: SystemDriver,
	args: string[],
	options: {
		cwd?: string;
		env?: Record<string, string>;
		onStdout?: (data: Uint8Array) => void;
		onStderr?: (data: Uint8Array) => void;
	},
): SpawnedProcess {
	// Extract code from node args
	let code: string;
	let filePath = "/child-entry.mjs";

	if (args[0] === "-e" || args[0] === "--eval") {
		code = args[1] ?? "";
	} else if (args[0] === "-p" || args[0] === "--print") {
		code = `process.stdout.write(String(${args[1] ?? "undefined"}))`;
	} else if (args[0] && !args[0].startsWith("-")) {
		// node script.js — require the file
		filePath = args[0];
		code = `await import(${JSON.stringify(args[0])})`;
	} else {
		const err = new Error(
			"ENOSYS: unsupported node invocation",
		) as NodeJS.ErrnoException;
		err.code = "ENOSYS";
		throw err;
	}

	// Build child system driver — no recursive command executor to prevent infinite loops
	const childSystemDriver: SystemDriver = {
		filesystem: baseSystemDriver.filesystem,
		network: baseSystemDriver.network,
		permissions: baseSystemDriver.permissions,
		runtime: {
			process: {
				...baseSystemDriver.runtime?.process,
				cwd: options.cwd ?? baseSystemDriver.runtime?.process?.cwd,
				env: options.env,
				argv: ["node", ...args],
			},
			os: {
				...baseSystemDriver.runtime?.os,
			},
		},
	};

	const encoder = new TextEncoder();
	let driver: NodeRuntimeDriver | undefined;

	// Create child driver with stdio routing
	driver = factory.createRuntimeDriver({
		system: childSystemDriver,
		runtime: {
			process:
				childSystemDriver.runtime?.process ?? ({} as import("@secure-exec/core/internal/shared/api-types").ProcessConfig),
			os:
				childSystemDriver.runtime?.os ?? ({} as import("@secure-exec/core/internal/shared/api-types").OSConfig),
		},
		onStdio: (event) => {
			if (event.channel === "stdout" && options.onStdout) {
				options.onStdout(encoder.encode(event.message));
			}
			if (event.channel === "stderr" && options.onStderr) {
				options.onStderr(encoder.encode(event.message));
			}
		},
	});

	// Track execution asynchronously
	const waitPromise: Promise<number> = (async () => {
		try {
			const result = await driver!.exec(code, {
				cwd: options.cwd,
				filePath,
				env: options.env,
			});
			return result.code;
		} catch {
			return 1;
		} finally {
			try {
				driver!.dispose();
			} catch {
				/* already disposed */
			}
		}
	})();

	return {
		writeStdin(): void {
			/* stdin not supported for sandbox child node processes */
		},
		closeStdin(): void {
			/* no-op */
		},
		kill(): void {
			try {
				driver?.dispose();
			} catch {
				/* already disposed */
			}
		},
		wait: () => waitPromise,
	};
}
