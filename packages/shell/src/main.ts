#!/usr/bin/env node

/**
 * Goal: `agentos-shell` should feel like the VM equivalent of `docker run`.
 *
 * Keep the CLI surface intentionally close to Docker's process flags:
 * stdin and TTY mode default on; `--no-interactive` and `--no-tty` disable them.
 * `-i/--interactive` and `-t/--tty` remain accepted for Docker CLI familiarity,
 * `-e/--env` and `--env-file` inject environment variables, `-v/--volume`
 * and `--mount type=bind,...` mount host paths, and `-w/--workdir` chooses
 * the guest cwd. When TTY mode is requested, the guest command goes through
 * Agent OS's terminal API instead of a custom prompt or line editor; non-TTY
 * commands use process spawn with Docker-like stdin attachment rules.
 */

import {
	cpSync,
	existsSync,
	mkdirSync,
	mkdtempSync,
	readFileSync,
	statSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import codex from "@agentos-software/codex-cli";
import coreutils from "@agentos-software/coreutils";
import curl from "@agentos-software/curl";
import diffutils from "@agentos-software/diffutils";
import fd from "@agentos-software/fd";
import file from "@agentos-software/file";
import findutils from "@agentos-software/findutils";
import gawk from "@agentos-software/gawk";
import git from "@agentos-software/git";
import grep from "@agentos-software/grep";
import gzip from "@agentos-software/gzip";
import jq from "@agentos-software/jq";
import ripgrep from "@agentos-software/ripgrep";
import sed from "@agentos-software/sed";
import sqlite3 from "@agentos-software/sqlite3";
import tar from "@agentos-software/tar";
import tree from "@agentos-software/tree";
import unzip from "@agentos-software/unzip";
import vim from "@agentos-software/vim";
import wget from "@agentos-software/wget";
import yq from "@agentos-software/yq";
import zip from "@agentos-software/zip";
import type { MountConfig, SoftwareInput } from "@rivet-dev/agentos-core";
import { AgentOs } from "@rivet-dev/agentos-core";
import { allowAll } from "@rivet-dev/agentos-core/internal/runtime-compat";
import { Command, Option } from "commander";
import { createActorShellVm, type ShellVmHandle } from "./actor-vm.js";

const __dirname = dirname(fileURLToPath(import.meta.url));
const workspaceRoot = resolve(__dirname, "../../..");
const fallbackCommandDirs = [
	resolve(
		workspaceRoot,
		"toolchain/target/wasm32-wasip1/release/commands",
	),
];
const BRUSH_SHELL_COMMANDS = new Set(["bash", "sh"]);
const SHELL_OPTIONS_WITH_VALUES = new Set([
	"--command",
	"--debuglog-enable",
	"--init-file",
	"--input-backend",
	"--log-disable",
	"--rcfile",
	"-c",
]);

interface CliOptions {
	interactive: boolean;
	tty: boolean;
	actor: boolean;
	workdir: string;
	env: string[];
	envFile: string[];
	volume: string[];
	mount: string[];
	rm: boolean;
	name?: string;
	command: string;
	args: string[];
}

// The `@agentos-software/*` packages default-export a package descriptor
// pointing at their packed `.aospkg` via `packagePath`. Local transition
// builds may still export a package directory as `packagePath`.
interface RegistryPackage {
	name?: string;
	packagePath?: string;
}

// Transition package dirs must carry the toolchain-input manifest.
function isUsablePackageDir(dir: string | undefined): dir is string {
	return dir !== undefined && existsSync(resolve(dir, "agentos-package.json"));
}

function isUsablePackageFile(file: string | undefined): file is string {
	if (file === undefined) return false;
	try {
		return statSync(file).isFile();
	} catch {
		return false;
	}
}

// Published packages ship their packed `.aospkg` materialized. Workspace
// packages may not have run the native build yet, so the pack can be missing —
// fall back to the native command-build output dir when it is. A package whose
// dir exists but predates the agentos-package.json manifest requirement is
// skipped (with a warning) rather than aborting VM creation for the shell.
function withLocalCommandFallback(pkg: RegistryPackage): SoftwareInput | null {
	if (isUsablePackageFile(pkg.packagePath)) {
		return { packagePath: pkg.packagePath };
	}
	if (isUsablePackageDir(pkg.packagePath)) {
		return { packagePath: pkg.packagePath };
	}
	const fallbackCommandDir = fallbackCommandDirs.find(isUsablePackageDir);
	if (fallbackCommandDir !== undefined) {
		return { packagePath: fallbackCommandDir };
	}
	console.warn(
		`agentos-shell: skipping software package without a usable packagePath: ${
			pkg.name ?? pkg.packagePath ?? "<unknown>"
		}`,
	);
	return null;
}

const software: SoftwareInput[] = [
	coreutils,
	sed,
	grep,
	gawk,
	findutils,
	diffutils,
	tar,
	gzip,
	curl,
	zip,
	unzip,
	jq,
	ripgrep,
	fd,
	tree,
	file,
	yq,
	codex,
	git,
	sqlite3,
	wget,
]
	.map(withLocalCommandFallback)
	.filter((input): input is SoftwareInput => input !== null);

// vim ships as a software package (which carries its runtime tree + VIMRUNTIME
// via the manifest `provides`). An unbuilt package is a valid empty placeholder
// — skip it rather than projecting a command-less package (build it locally:
// drop the wasm binary into toolchain/target/.../commands and run
// `just software-build`).
for (const editor of [vim] as RegistryPackage[]) {
	if (isUsablePackageFile(editor.packagePath)) {
		software.push({ packagePath: editor.packagePath });
		continue;
	}
	const packageDir = editor.packagePath;
	if (packageDir === undefined || !isUsablePackageDir(packageDir)) continue;
	try {
		const bin = JSON.parse(
			readFileSync(join(packageDir, "package.json"), "utf8"),
		).bin as Record<string, string> | undefined;
		if (bin && Object.keys(bin).length > 0) {
			software.push({ packagePath: packageDir });
		}
	} catch {
		// placeholder/unreadable — leave the editor out
	}
}

function createShellDiagnosticStripper(): (
	data: Uint8Array,
) => Uint8Array | null {
	let suppressUntilNewline = false;
	return (data: Uint8Array) => {
		let text = Buffer.from(data).toString("utf8");
		let output = "";

		while (text.length > 0) {
			if (suppressUntilNewline) {
				const newlineIndex = text.indexOf("\n");
				if (newlineIndex < 0) {
					return output.length > 0 ? Buffer.from(output, "utf8") : null;
				}
				text = text.slice(newlineIndex + 1);
				suppressUntilNewline = false;
				continue;
			}

			const warningIndex = text.indexOf("WARN could not retrieve pid");
			if (warningIndex < 0) {
				output += text;
				break;
			}

			const lineStartIndex = text.lastIndexOf("\n", warningIndex);
			const lineStart = lineStartIndex < 0 ? 0 : lineStartIndex + 1;
			output += text.slice(0, lineStart);

			const lineEnd = text.indexOf("\n", warningIndex);
			if (lineEnd < 0) {
				suppressUntilNewline = true;
				break;
			}
			text = text.slice(lineEnd + 1);
		}

		return output.length > 0 ? Buffer.from(output, "utf8") : null;
	};
}

function collectOption(value: string, previous: string[]): string[] {
	previous.push(value);
	return previous;
}

function parseCli(argv: string[]): CliOptions {
	const program = new Command()
		.name("agentos-shell")
		.description("Run a command or terminal inside an Agent OS VM.")
		.exitOverride()
		.passThroughOptions()
		.allowExcessArguments()
		.argument("[command]", "guest command to run", "bash")
		.argument("[args...]", "guest command arguments")
		.addOption(
			new Option("-i, --interactive", "keep stdin attached").default(true),
		)
		.addOption(new Option("--no-interactive", "detach stdin"))
		.addOption(new Option("-t, --tty", "connect a terminal").default(true))
		.addOption(new Option("--no-tty", "disable terminal mode"))
		.addOption(
			new Option(
				"--actor",
				"run through the RivetKit AgentOS actor instead of the in-process core client",
			).default(false),
		)
		.option(
			"-e, --env <env>",
			"set environment variable (KEY=VALUE or KEY to copy from host)",
			collectOption,
			[],
		)
		.option(
			"--env-file <path>",
			"read environment variables from a file",
			collectOption,
			[],
		)
		.option(
			"-v, --volume <spec>",
			"bind mount a volume (host:guest[:ro|rw])",
			collectOption,
			[],
		)
		.option(
			"--mount <spec>",
			"bind mount using Docker syntax (type=bind,src=...,target=...,readonly)",
			collectOption,
			[],
		)
		.option("-w, --workdir <path>", "working directory inside the VM", "/")
		.option(
			"--name <name>",
			"container-style name label (accepted for Docker CLI parity)",
		)
		.option("--rm", "remove VM after exit (always true for this CLI)", false);

	try {
		program.parse(["node", "agentos-shell", ...argv]);
	} catch (error) {
		if (
			error &&
			typeof error === "object" &&
			"code" in error &&
			error.code === "commander.helpDisplayed"
		) {
			process.exit(0);
		}
		throw error;
	}

	const opts = program.opts<{
		interactive: boolean;
		tty: boolean;
		actor: boolean;
		workdir: string;
		env: string[];
		envFile: string[];
		volume: string[];
		mount: string[];
		rm: boolean;
		name?: string;
	}>();
	const [command = "bash", ...args] = program.args;

	return {
		interactive: opts.interactive,
		tty: opts.tty,
		actor: opts.actor,
		workdir: opts.workdir,
		env: opts.env,
		envFile: opts.envFile,
		volume: opts.volume,
		mount: opts.mount,
		rm: opts.rm,
		name: opts.name,
		command,
		args,
	};
}

function parseEnvLine(line: string): [string, string] | null {
	const trimmed = line.trim();
	if (!trimmed || trimmed.startsWith("#")) {
		return null;
	}
	const equalsIndex = trimmed.indexOf("=");
	if (equalsIndex < 0) {
		const hostValue = process.env[trimmed];
		return hostValue === undefined ? null : [trimmed, hostValue];
	}
	return [trimmed.slice(0, equalsIndex), trimmed.slice(equalsIndex + 1)];
}

function buildEnv(options: CliOptions): Record<string, string> {
	const env: Record<string, string> = {};
	for (const envFilePath of options.envFile) {
		const content = readFileSync(resolve(envFilePath), "utf8");
		for (const line of content.split(/\r?\n/)) {
			const entry = parseEnvLine(line);
			if (entry) {
				env[entry[0]] = entry[1];
			}
		}
	}
	for (const value of options.env) {
		const entry = parseEnvLine(value);
		if (entry) {
			env[entry[0]] = entry[1];
		}
	}
	return env;
}

function hostDirMount(
	hostPath: string,
	guestPath: string,
	readOnly: boolean,
): MountConfig {
	return {
		path: guestPath,
		readOnly,
		plugin: {
			id: "host_dir",
			config: {
				hostPath: resolve(hostPath),
				readOnly,
			},
		},
	};
}

function parseVolumeSpec(spec: string): MountConfig {
	const [hostPath, guestPath, mode] = spec.split(":");
	if (!hostPath || !guestPath) {
		throw new Error(
			`Invalid volume spec "${spec}"; expected host:guest[:ro|rw]`,
		);
	}
	if (mode && mode !== "ro" && mode !== "rw") {
		throw new Error(`Invalid volume mode "${mode}" in "${spec}"`);
	}
	return hostDirMount(hostPath, guestPath, mode === "ro");
}

function parseMountSpec(spec: string): MountConfig {
	const fields = new Map<string, string | true>();
	for (const rawPart of spec.split(",")) {
		const part = rawPart.trim();
		if (!part) {
			continue;
		}
		const equalsIndex = part.indexOf("=");
		if (equalsIndex < 0) {
			fields.set(part, true);
		} else {
			fields.set(part.slice(0, equalsIndex), part.slice(equalsIndex + 1));
		}
	}

	if (fields.get("type") !== "bind") {
		throw new Error(`Only bind mounts are supported: --mount ${spec}`);
	}
	const source = fields.get("source") ?? fields.get("src");
	const target =
		fields.get("target") ?? fields.get("dst") ?? fields.get("destination");
	if (typeof source !== "string" || typeof target !== "string") {
		throw new Error(
			`Invalid mount spec "${spec}"; expected type=bind,source=...,target=...`,
		);
	}
	const readOnly = fields.has("readonly") || fields.get("ro") === "true";
	return hostDirMount(source, target, readOnly);
}

function buildMounts(options: CliOptions): MountConfig[] {
	return [
		...options.volume.map(parseVolumeSpec),
		...options.mount.map(parseMountSpec),
	];
}

function isBrushShellCommand(command: string): boolean {
	return BRUSH_SHELL_COMMANDS.has(basename(command));
}

function hasBrushInputBackend(args: string[]): boolean {
	return args.some(
		(arg) => arg === "--input-backend" || arg.startsWith("--input-backend="),
	);
}

function hasInteractiveShellFlag(args: string[]): boolean {
	return args.some((arg) => arg === "-i" || arg === "--interactive");
}

function shellArgsRequestCommandOrScript(args: string[]): boolean {
	for (let i = 0; i < args.length; i++) {
		const arg = args[i];
		if (arg === "--") {
			return i + 1 < args.length;
		}
		if (arg.startsWith("--") && arg.includes("=")) {
			if (arg.startsWith("--command=")) {
				return true;
			}
			continue;
		}
		if (SHELL_OPTIONS_WITH_VALUES.has(arg)) {
			if (arg === "-c" || arg === "--command") {
				return true;
			}
			i++;
			continue;
		}
		if (arg.startsWith("-")) {
			continue;
		}
		return true;
	}
	return false;
}

// The error brush prints when the requested input backend is not compiled
// into the wasm build (e.g. reedline missing from the shipped package). Used
// to auto-fall back to the always-available `minimal` backend.
const BRUSH_BACKEND_UNSUPPORTED_MARKER =
	"requested input backend type not supported";

/**
 * Terminal command candidates, tried in order. Prefer `reedline` (history,
 * arrows, reverse search — requires a brush wasm build with the reedline
 * feature) and fall back to `minimal` when the shipped build rejects it. An
 * explicit user-provided backend is used verbatim with no fallback.
 */
function buildTerminalCommandAttempts(options: CliOptions): {
	command: string;
	args: string[];
}[] {
	const baseArgs = [...options.args];
	if (!isBrushShellCommand(options.command)) {
		return [{ command: options.command, args: baseArgs }];
	}
	if (
		!hasInteractiveShellFlag(baseArgs) &&
		!shellArgsRequestCommandOrScript(baseArgs)
	) {
		baseArgs.push("-i");
	}
	if (hasBrushInputBackend(baseArgs)) {
		// Explicit backend: use it verbatim; the brush error surfaces as-is.
		return [{ command: options.command, args: baseArgs }];
	}
	return ["reedline", "minimal"].map((backend) => ({
		command: options.command,
		args: ["--input-backend", backend, ...baseArgs],
	}));
}

async function runSpawnedCommand(
	vm: ShellVmHandle,
	options: CliOptions,
	env: Record<string, string>,
): Promise<number> {
	const child = await vm.spawn(options.command, options.args, {
		cwd: options.workdir,
		env,
		streamStdin: options.interactive,
		onStdout: (data) => {
			process.stdout.write(data);
		},
		onStderr: (data: Uint8Array) => {
			process.stderr.write(data);
		},
	});
	let stdinQueue = Promise.resolve();
	const queueStdin = (operation: () => Promise<void>) => {
		stdinQueue = stdinQueue.then(operation);
		void stdinQueue.catch((error) => {
			const message = error instanceof Error ? error.message : String(error);
			process.stderr.write(`${message}\n`);
		});
	};
	const closeChildStdin = () => {
		queueStdin(async () => {
			try {
				await vm.closeProcessStdin(child.pid);
			} catch {
				// The process may have already exited before host stdin reports EOF.
			}
		});
	};
	const onStdinData = (data: Uint8Array | string) => {
		queueStdin(() => vm.writeProcessStdin(child.pid, data));
	};

	if (!options.interactive) {
		closeChildStdin();
		return vm.waitProcess(child.pid);
	}

	try {
		process.stdin.on("data", onStdinData);
		process.stdin.once("end", closeChildStdin);
		process.stdin.once("error", closeChildStdin);
		process.stdin.resume();
		return await vm.waitProcess(child.pid);
	} finally {
		process.stdin.removeListener("data", onStdinData);
		process.stdin.removeListener("end", closeChildStdin);
		process.stdin.removeListener("error", closeChildStdin);
		process.stdin.pause();
	}
}

async function runTerminalCommand(
	vm: ShellVmHandle,
	options: CliOptions,
	env: Record<string, string>,
): Promise<number> {
	const attempts = buildTerminalCommandAttempts(options);
	for (let index = 0; index < attempts.length; index++) {
		const canFallback = index + 1 < attempts.length;
		const result = await runTerminalAttempt(
			vm,
			options,
			env,
			attempts[index],
			canFallback,
		);
		if (result.backendUnsupported && canFallback) {
			process.stderr.write(
				"agentos-shell: shell build does not support the requested input backend; retrying with --input-backend minimal\n",
			);
			continue;
		}
		return result.exitCode;
	}
	return 1;
}

async function runTerminalAttempt(
	vm: ShellVmHandle,
	options: CliOptions,
	env: Record<string, string>,
	terminalCommand: { command: string; args: string[] },
	canFallback: boolean,
): Promise<{ exitCode: number; backendUnsupported: boolean }> {
	const stripDiagnostics = createShellDiagnosticStripper();
	let backendUnsupported = false;
	const decoder = new TextDecoder();
	const detectBackendError = (data: Uint8Array) => {
		if (decoder.decode(data).includes(BRUSH_BACKEND_UNSUPPORTED_MARKER)) {
			backendUnsupported = true;
		}
	};
	// Suppress the backend error output only when a fallback attempt will run;
	// without one the error must reach the user.
	const suppress = () => backendUnsupported && canFallback;
	const shellOptions = {
		cwd: options.workdir,
		env,
		cols: process.stdout.columns,
		rows: process.stdout.rows,
	};
	const { shellId } = await vm.openShell({
		...shellOptions,
		...terminalCommand,
	});
	let stdinQueue = Promise.resolve();
	const queueShellInput = (data: Uint8Array | string) => {
		stdinQueue = stdinQueue.then(() => vm.writeShell(shellId, data));
		void stdinQueue.catch((error) => {
			const message = error instanceof Error ? error.message : String(error);
			process.stderr.write(`${message}\n`);
		});
	};
	const onStdinData = (data: Uint8Array | string) => {
		queueShellInput(data);
	};
	const onStdinEnd = () => {
		queueShellInput("\u0004");
	};
	const onResize = () => {
		vm.resizeShell(shellId, process.stdout.columns, process.stdout.rows);
	};
	const unsubscribeOutput = vm.onShellData(shellId, (event) => {
		detectBackendError(event.data);
		if (suppress()) return;
		const sanitized = stripDiagnostics(event.data);
		if (sanitized) process.stdout.write(sanitized);
	});
	const canUseRawMode =
		options.interactive &&
		process.stdin.isTTY &&
		typeof process.stdin.setRawMode === "function";
	let rawModeEnabled = false;

	try {
		if (options.interactive) {
			if (canUseRawMode) {
				process.stdin.setRawMode(true);
				rawModeEnabled = true;
			}
			process.stdin.on("data", onStdinData);
			process.stdin.once("end", onStdinEnd);
			process.stdin.once("error", onStdinEnd);
			process.stdin.resume();
		}
		if (process.stdout.isTTY) {
			process.stdout.on("resize", onResize);
			onResize();
		}

		const exitCode = await vm.waitShell(shellId);
		// Give trailing output events a moment to flush before unsubscribing:
		// a fast-failing shell otherwise exits with its error output silently
		// dropped, leaving nothing but a bare exit code to debug from.
		await new Promise((r) => setTimeout(r, 250));
		return { exitCode, backendUnsupported };
	} finally {
		unsubscribeOutput();
		process.stdin.removeListener("data", onStdinData);
		process.stdin.removeListener("end", onStdinEnd);
		process.stdin.removeListener("error", onStdinEnd);
		process.stdin.pause();
		if (rawModeEnabled) {
			process.stdin.setRawMode(false);
		}
		if (process.stdout.isTTY) {
			process.stdout.removeListener("resize", onResize);
		}
	}
}

function coreShellVm(vm: AgentOs): ShellVmHandle {
	return {
		spawn: (command, args, options) => vm.process.spawn(command, args, options),
		writeProcessStdin: (pid, data) => vm.process.writeStdin(pid, data),
		closeProcessStdin: (pid) => vm.process.closeStdin(pid),
		async waitProcess(pid) {
			const exit = await vm.process.wait(pid);
			return exit.exitCode ?? 1;
		},
		openShell: (options) => vm.terminal.open(options),
		writeShell: (shellId, data) => vm.terminal.write(shellId, data),
		resizeShell: (shellId, cols, rows) =>
			vm.terminal.resize(shellId, cols, rows),
		onShellData: (shellId, handler) => vm.onShellData(shellId, handler),
		waitShell: (shellId) => vm.terminal.wait(shellId),
		dispose: () => vm.dispose(),
	};
}

const cli = parseCli(process.argv.slice(2));
const env = buildEnv(cli);
const mounts = buildMounts(cli);

const vm: ShellVmHandle = cli.actor
	? await createActorShellVm({ software, mounts, defaultSoftware: false })
	: coreShellVm(
			await AgentOs.create({
				mounts,
				permissions: allowAll,
				software,
				defaultSoftware: false,
			}),
		);

let exitCode = 1;
try {
	const useTerminal = cli.tty && process.stdin.isTTY && process.stdout.isTTY;
	exitCode = useTerminal
		? await runTerminalCommand(vm, cli, env)
		: await runSpawnedCommand(vm, cli, env);
} finally {
	await vm.dispose();
}
process.exit(exitCode);
