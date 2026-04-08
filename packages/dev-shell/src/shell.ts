#!/usr/bin/env node

import path from "node:path";
import { createDevShellKernel } from "./kernel.js";

interface CliOptions {
	workDir?: string;
	debugLogPath?: string;
	mountWasm: boolean;
	command: string;
	args: string[];
}

function printUsage(): void {
	console.error(
		[
			"Usage:",
			"  agent-os-dev-shell [--work-dir <path>] [--debug-log <path>] [--no-wasm] [--] [command] [args...]",
			"",
			"Examples:",
			"  just dev-shell",
			"  just dev-shell --work-dir /tmp/demo",
			"  just dev-shell --debug-log /tmp/dev-shell-debug.ndjson",
			"  just dev-shell sh",
			"  just dev-shell -- node -e 'console.log(process.version)'",
		].join("\n"),
	);
}

function shQuote(value: string): string {
	return `'${value.replace(/'/g, "'\\''")}'`;
}

function parseArgs(argv: string[]): CliOptions {
	const normalizedArgv = argv[0] === "--" ? argv.slice(1) : argv;
	const options: CliOptions = {
		mountWasm: true,
		command: "bash",
		args: [],
	};

	for (let index = 0; index < normalizedArgv.length; index++) {
		const arg = normalizedArgv[index];
		if (arg === "--") {
			const trailing = normalizedArgv.slice(index + 1);
			if (trailing.length > 0) {
				options.command = trailing[0];
				options.args = trailing.slice(1);
			}
			break;
		}

		if (!arg.startsWith("-")) {
			options.command = arg;
			options.args = normalizedArgv.slice(index + 1);
			break;
		}

		switch (arg) {
			case "--work-dir":
				if (!normalizedArgv[index + 1]) {
					throw new Error("--work-dir requires a path");
				}
				options.workDir = path.resolve(normalizedArgv[++index]);
				break;
			case "--debug-log":
				if (!normalizedArgv[index + 1]) {
					throw new Error("--debug-log requires a file path");
				}
				options.debugLogPath = path.resolve(normalizedArgv[++index]);
				break;
			case "--no-wasm":
				options.mountWasm = false;
				break;
			case "--help":
			case "-h":
				printUsage();
				process.exit(0);
				return options;
			default:
				throw new Error(`Unknown argument: ${arg}`);
		}
	}

	return options;
}

const cli = parseArgs(process.argv.slice(2));
if (!cli.mountWasm && (cli.command === "bash" || cli.command === "sh")) {
	throw new Error(
		"Interactive dev-shell requires WasmVM for the shell process; remove --no-wasm.",
	);
}

const shell = await createDevShellKernel({
	workDir: cli.workDir,
	mountWasm: cli.mountWasm,
	debugLogPath: cli.debugLogPath,
});

console.error(`agent-os dev shell`);
console.error(`work dir: ${shell.workDir}`);
console.error(`loaded commands: ${shell.loadedCommands.join(", ")}`);

const terminalCommand =
	cli.command === "bash" || cli.command === "sh"
		? (() => {
				if (cli.args.length === 0) {
					return {
						command: cli.command,
						args: [],
					};
				}

				if (
					(cli.args[0] === "-c" || cli.args[0] === "-lc") &&
					cli.args.length >= 2
				) {
					return {
						command: cli.command,
						args: [
							cli.args[0],
							`cd ${shQuote(shell.workDir)} && ${cli.args[1]}`,
							...cli.args.slice(2),
						],
					};
				}

				return {
					command: cli.command,
					args: cli.args,
				};
			})()
		: {
				command: cli.command,
				args: cli.args,
			};

const exitCode =
	(terminalCommand.command === "bash" || terminalCommand.command === "sh") &&
	terminalCommand.args.length === 0
		? await shell.kernel.connectTerminal({
				command: terminalCommand.command,
				args: terminalCommand.args,
				cwd: shell.workDir,
				env: shell.env,
			})
		: await new Promise<number>((resolve) => {
				const proc = shell.kernel.spawn(
					terminalCommand.command,
					terminalCommand.args,
					{
						cwd: shell.workDir,
						env: shell.env,
						onStdout: (data) => {
							process.stdout.write(Buffer.from(data));
						},
						onStderr: (data) => {
							process.stderr.write(Buffer.from(data));
						},
					},
				);
				void proc.wait().then(resolve);
			});

await shell.dispose();
process.exit(exitCode);
