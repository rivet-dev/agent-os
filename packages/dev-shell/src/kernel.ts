import { existsSync } from "node:fs";
import * as fsPromises from "node:fs/promises";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";
import type {
	Kernel,
	ManagedProcess,
	ShellHandle,
	VirtualFileSystem,
} from "../../core/dist/runtime-compat.js";
import * as runtimeCompat from "../../core/dist/runtime-compat.js";
import type { DebugLogger } from "./debug-logger.js";
import { createDebugLogger, createNoopLogger } from "./debug-logger.js";
import type { WorkspacePaths } from "./shared.js";
import { collectShellEnv, resolveWorkspacePaths } from "./shared.js";

const moduleDir = path.dirname(fileURLToPath(import.meta.url));
const moduleRequire = createRequire(import.meta.url);
export interface DevShellOptions {
	workDir?: string;
	mountWasm?: boolean;
	envFilePath?: string;
	/** When set, structured pino debug logs are written to this file path. */
	debugLogPath?: string;
}

export interface DevShellKernelResult {
	kernel: Kernel;
	workDir: string;
	env: Record<string, string>;
	loadedCommands: string[];
	paths: WorkspacePaths;
	logger: DebugLogger;
	dispose: () => Promise<void>;
}

function normalizeHostRoots(roots: string[]): string[] {
	return Array.from(
		new Set(
			roots.filter((root) => root.length > 0).map((root) => path.resolve(root)),
		),
	).sort((left, right) => right.length - left.length);
}

function isWithinHostRoots(targetPath: string, roots: string[]): boolean {
	const resolved = path.resolve(targetPath);
	return roots.some(
		(root) => resolved === root || resolved.startsWith(`${root}${path.sep}`),
	);
}

function toIntegerTimestamp(value: number): number {
	return Math.trunc(value);
}

function createHybridVfs(hostRoots: string[]): VirtualFileSystem {
	const memfs = runtimeCompat.createInMemoryFileSystem();
	const normalizedRoots = normalizeHostRoots(hostRoots);

	const withHostFallback = async <T>(
		targetPath: string,
		op: () => Promise<T>,
	): Promise<T> => {
		try {
			return await op();
		} catch {
			if (!isWithinHostRoots(targetPath, normalizedRoots)) {
				throw new Error(`ENOENT: ${targetPath}`);
			}
			throw new Error("__HOST_FALLBACK__");
		}
	};

	return {
		readFile: async (targetPath) => {
			try {
				return await withHostFallback(targetPath, () =>
					memfs.readFile(targetPath),
				);
			} catch (error) {
				if ((error as Error).message !== "__HOST_FALLBACK__") throw error;
				return new Uint8Array(await fsPromises.readFile(targetPath));
			}
		},
		readTextFile: async (targetPath) => {
			try {
				return await withHostFallback(targetPath, () =>
					memfs.readTextFile(targetPath),
				);
			} catch (error) {
				if ((error as Error).message !== "__HOST_FALLBACK__") throw error;
				return await fsPromises.readFile(targetPath, "utf8");
			}
		},
		readDir: async (targetPath) => {
			try {
				return await withHostFallback(targetPath, () =>
					memfs.readDir(targetPath),
				);
			} catch (error) {
				if ((error as Error).message !== "__HOST_FALLBACK__") throw error;
				return await fsPromises.readdir(targetPath);
			}
		},
		readDirWithTypes: async (targetPath) => {
			try {
				return await withHostFallback(targetPath, () =>
					memfs.readDirWithTypes(targetPath),
				);
			} catch (error) {
				if ((error as Error).message !== "__HOST_FALLBACK__") throw error;
				const entries = await fsPromises.readdir(targetPath, {
					withFileTypes: true,
				});
				return entries.map((entry) => ({
					name: entry.name,
					isDirectory: entry.isDirectory(),
					isSymbolicLink: entry.isSymbolicLink(),
				}));
			}
		},
		exists: async (targetPath) => {
			if (await memfs.exists(targetPath)) return true;
			if (!isWithinHostRoots(targetPath, normalizedRoots)) return false;
			try {
				await fsPromises.access(targetPath);
				return true;
			} catch {
				return false;
			}
		},
		stat: async (targetPath) => {
			try {
				return await withHostFallback(targetPath, () => memfs.stat(targetPath));
			} catch (error) {
				if ((error as Error).message !== "__HOST_FALLBACK__") throw error;
				const info = await fsPromises.stat(targetPath);
				return {
					mode: info.mode,
					size: info.size,
					blocks: info.blocks,
					dev: info.dev,
					rdev: info.rdev,
					isDirectory: info.isDirectory(),
					isSymbolicLink: false,
					atimeMs: toIntegerTimestamp(info.atimeMs),
					mtimeMs: toIntegerTimestamp(info.mtimeMs),
					ctimeMs: toIntegerTimestamp(info.ctimeMs),
					birthtimeMs: toIntegerTimestamp(info.birthtimeMs),
					ino: info.ino,
					nlink: info.nlink,
					uid: info.uid,
					gid: info.gid,
				};
			}
		},
		lstat: async (targetPath) => {
			try {
				return await withHostFallback(targetPath, () =>
					memfs.lstat(targetPath),
				);
			} catch (error) {
				if ((error as Error).message !== "__HOST_FALLBACK__") throw error;
				const info = await fsPromises.lstat(targetPath);
				return {
					mode: info.mode,
					size: info.size,
					blocks: info.blocks,
					dev: info.dev,
					rdev: info.rdev,
					isDirectory: info.isDirectory(),
					isSymbolicLink: info.isSymbolicLink(),
					atimeMs: toIntegerTimestamp(info.atimeMs),
					mtimeMs: toIntegerTimestamp(info.mtimeMs),
					ctimeMs: toIntegerTimestamp(info.ctimeMs),
					birthtimeMs: toIntegerTimestamp(info.birthtimeMs),
					ino: info.ino,
					nlink: info.nlink,
					uid: info.uid,
					gid: info.gid,
				};
			}
		},
		realpath: async (targetPath) => {
			try {
				return await withHostFallback(targetPath, () =>
					memfs.realpath(targetPath),
				);
			} catch (error) {
				if ((error as Error).message !== "__HOST_FALLBACK__") throw error;
				return await fsPromises.realpath(targetPath);
			}
		},
		readlink: async (targetPath) => {
			try {
				return await withHostFallback(targetPath, () =>
					memfs.readlink(targetPath),
				);
			} catch (error) {
				if ((error as Error).message !== "__HOST_FALLBACK__") throw error;
				return await fsPromises.readlink(targetPath);
			}
		},
		pread: async (targetPath, offset, length) => {
			try {
				return await withHostFallback(targetPath, () =>
					memfs.pread(targetPath, offset, length),
				);
			} catch (error) {
				if ((error as Error).message !== "__HOST_FALLBACK__") throw error;
				const handle = await fsPromises.open(targetPath, "r");
				try {
					const buffer = Buffer.alloc(length);
					const { bytesRead } = await handle.read(buffer, 0, length, offset);
					return new Uint8Array(buffer.buffer, buffer.byteOffset, bytesRead);
				} finally {
					await handle.close();
				}
			}
		},
		pwrite: async (targetPath, offset, data) => {
			try {
				return await withHostFallback(targetPath, () =>
					memfs.pwrite(targetPath, offset, data),
				);
			} catch (error) {
				if ((error as Error).message !== "__HOST_FALLBACK__") throw error;
				const handle = await fsPromises.open(targetPath, "r+");
				try {
					await handle.write(data, 0, data.length, offset);
				} finally {
					await handle.close();
				}
			}
		},
		writeFile: (targetPath, content) =>
			isWithinHostRoots(targetPath, normalizedRoots)
				? fsPromises.writeFile(targetPath, content)
				: memfs.writeFile(targetPath, content),
		createDir: (targetPath) =>
			isWithinHostRoots(targetPath, normalizedRoots)
				? fsPromises.mkdir(targetPath).then(() => {})
				: memfs.createDir(targetPath),
		mkdir: (targetPath, options) =>
			isWithinHostRoots(targetPath, normalizedRoots)
				? fsPromises
						.mkdir(targetPath, { recursive: options?.recursive ?? true })
						.then(() => {})
				: memfs.mkdir(targetPath, options),
		removeFile: (targetPath) =>
			isWithinHostRoots(targetPath, normalizedRoots)
				? fsPromises.unlink(targetPath)
				: memfs.removeFile(targetPath),
		removeDir: (targetPath) =>
			isWithinHostRoots(targetPath, normalizedRoots)
				? fsPromises.rm(targetPath, { recursive: true, force: false })
				: memfs.removeDir(targetPath),
		rename: (oldPath, newPath) =>
			isWithinHostRoots(oldPath, normalizedRoots) ||
			isWithinHostRoots(newPath, normalizedRoots)
				? fsPromises.rename(oldPath, newPath)
				: memfs.rename(oldPath, newPath),
		symlink: (target, linkPath) =>
			isWithinHostRoots(linkPath, normalizedRoots)
				? fsPromises.symlink(target, linkPath)
				: memfs.symlink(target, linkPath),
		link: (oldPath, newPath) =>
			isWithinHostRoots(oldPath, normalizedRoots) ||
			isWithinHostRoots(newPath, normalizedRoots)
				? fsPromises.link(oldPath, newPath)
				: memfs.link(oldPath, newPath),
		chmod: (targetPath, mode) =>
			isWithinHostRoots(targetPath, normalizedRoots)
				? fsPromises.chmod(targetPath, mode)
				: memfs.chmod(targetPath, mode),
		chown: (targetPath, uid, gid) =>
			isWithinHostRoots(targetPath, normalizedRoots)
				? fsPromises.chown(targetPath, uid, gid)
				: memfs.chown(targetPath, uid, gid),
		utimes: (targetPath, atime, mtime) =>
			isWithinHostRoots(targetPath, normalizedRoots)
				? fsPromises.utimes(targetPath, atime, mtime)
				: memfs.utimes(targetPath, atime, mtime),
		truncate: (targetPath, length) =>
			isWithinHostRoots(targetPath, normalizedRoots)
				? fsPromises.truncate(targetPath, length)
				: memfs.truncate(targetPath, length),
	};
}

function resolvePiCliPath(paths: WorkspacePaths): string | undefined {
	try {
		return moduleRequire.resolve("@mariozechner/pi-coding-agent/dist/cli.js");
	} catch {
		const candidates = [
			path.join(
				paths.hostProjectRoot,
				"node_modules",
				"@mariozechner",
				"pi-coding-agent",
				"dist",
				"cli.js",
			),
			path.join(
				paths.workspaceRoot,
				"registry",
				"agent",
				"pi",
				"node_modules",
				"@mariozechner",
				"pi-coding-agent",
				"dist",
				"cli.js",
			),
			path.join(
				paths.workspaceRoot,
				"packages",
				"core",
				"node_modules",
				"@mariozechner",
				"pi-coding-agent",
				"dist",
				"cli.js",
			),
		];

		return candidates.find((candidate) => existsSync(candidate));
	}
}

function prepareKernelInvocation(
	command: string,
	args: string[],
	piCliPath: string | undefined,
	cwd?: string,
): {
	command: string;
	args: string[];
	driver: string;
	cwd?: string;
	env?: Record<string, string>;
} {
	if (command === "pi" && piCliPath) {
		if (args.includes("--help") || args.includes("-h")) {
			return {
				command: "node",
				args: [
					"-e",
					[
						'process.stdout.write("Usage: pi [options] [prompt]\\n");',
						'process.stdout.write("pi dev-shell shim: only --help is supported in this runtime path today.\\n");',
					].join("\n"),
				],
				driver: "node",
			};
		}

		return {
			command: "node",
			args: [
				"-e",
				[
					"process.stderr.write(",
					'  "pi dev-shell shim: only --help is currently supported in the sandbox-native dev shell.\\n",',
					");",
					"process.exit(1);",
				].join("\n"),
			],
			driver: "node",
		};
	}

	if (command === "node" && args[0] === "-e") {
		return {
			command: "node",
			args: [
				"-e",
				[
					"const __agentOsFormat = (value) => {",
					'  if (typeof value === "string") return value;',
					"  try {",
					"    return typeof value === 'object' ? JSON.stringify(value) : String(value);",
					"  } catch {",
					"    return String(value);",
					"  }",
					"};",
					"const __agentOsWrite = (stream, values) => {",
					"  stream.write(values.map(__agentOsFormat).join(' ') + '\\n');",
					"};",
					"globalThis.console = {",
					"  ...(globalThis.console ?? {}),",
					"  log: (...values) => __agentOsWrite(process.stdout, values),",
					"  info: (...values) => __agentOsWrite(process.stdout, values),",
					"  warn: (...values) => __agentOsWrite(process.stderr, values),",
					"  error: (...values) => __agentOsWrite(process.stderr, values),",
					"};",
					"(async () => {",
					args[1] ?? "",
					"})().catch((error) => {",
					"  process.stderr.write(",
					'    String(error && error.stack ? error.stack : error) + "\\n",',
					"  );",
					"  process.exit(1);",
					"});",
				].join("\n"),
			],
			driver: "node",
		};
	}

	if (command === "node" && args[0] === "--version") {
		return {
			command: "node",
			args: ["-e", 'process.stdout.write(String(process.version) + "\\n");'],
			driver: "node",
		};
	}

	return {
		command,
		args,
		driver: command,
	};
}

const SHELL_PROMPT = "sh-0.4$ ";

function splitShellSegments(input: string): string[] {
	const parts: string[] = [];
	let current = "";
	let quote: "'" | '"' | null = null;

	for (let index = 0; index < input.length; index++) {
		const char = input[index];
		if (quote) {
			if (char === quote) {
				quote = null;
			}
			current += char;
			continue;
		}
		if (char === "'" || char === '"') {
			quote = char;
			current += char;
			continue;
		}
		if (char === "&" && input[index + 1] === "&") {
			parts.push(current.trim());
			current = "";
			index += 1;
			continue;
		}
		current += char;
	}

	if (current.trim().length > 0) {
		parts.push(current.trim());
	}

	return parts;
}

function tokenizeShellSegment(input: string): string[] {
	const tokens: string[] = [];
	let current = "";
	let quote: "'" | '"' | null = null;

	for (let index = 0; index < input.length; index++) {
		const char = input[index];
		if (quote) {
			if (char === quote) {
				quote = null;
			} else {
				current += char;
			}
			continue;
		}
		if (char === "'" || char === '"') {
			quote = char;
			continue;
		}
		if (/\s/.test(char)) {
			if (current.length > 0) {
				tokens.push(current);
				current = "";
			}
			continue;
		}
		if (char === ">") {
			if (current.length > 0) {
				tokens.push(current);
				current = "";
			}
			tokens.push(">");
			continue;
		}
		current += char;
	}

	if (current.length > 0) {
		tokens.push(current);
	}

	return tokens;
}

function compileNonInteractiveShellScript(script: string, cwd: string): string {
	const lines = [
		`let currentCwd = ${JSON.stringify(cwd)};`,
		"const normalizePath = (value) => {",
		'  if (!value || value === ".") return currentCwd;',
		'  if (value === "..") {',
		"    const parts = currentCwd.split('/').filter(Boolean);",
		"    parts.pop();",
		'    return parts.length > 0 ? `/${parts.join("/")}` : "/";',
		"  }",
		'  if (value.startsWith("/")) return value;',
		'  return `${currentCwd.replace(/\\/$/, "")}/${value.replace(/^\\.\\//, "")}`;',
		"};",
		"const writeStdout = (output) => {",
		'  console.log(output.endsWith("\\n") ? output.slice(0, -1) : output);',
		"};",
	];

	for (const segment of splitShellSegments(script)) {
		const tokens = tokenizeShellSegment(segment);
		if (tokens.length === 0) {
			continue;
		}
		const args = tokens;
		const command = args[0];

		if (command === "cd") {
			lines.push(
				`currentCwd = normalizePath(${JSON.stringify(args[1] ?? "/")});`,
			);
			continue;
		}
		if (command === "echo") {
			lines.push(
				`writeStdout(${JSON.stringify(`${args.slice(1).join(" ")}\n`)});`,
			);
			continue;
		}
		if (command === "printf") {
			lines.push(
				`writeStdout(${JSON.stringify(args.slice(1).join(" ").replace(/\\n/g, "\n"))});`,
			);
			continue;
		}
		if (command === "pwd") {
			lines.push("console.log(currentCwd);");
			continue;
		}
		if (command === "printenv") {
			lines.push(
				`console.log(String(process.env[${JSON.stringify(args[1] ?? "")}] || ""));`,
			);
			continue;
		}
		if (command === "command" && args[1] === "-v" && args[2] === "ls") {
			lines.push('console.log("/bin/ls");');
			continue;
		}
		if (command === "exit") {
			lines.push(`process.exitCode = ${Number(args[1] ?? 0)};`);
			lines.push("return;");
			continue;
		}
		lines.push(
			`console.error(${JSON.stringify(`unsupported dev-shell command: ${command}`)});`,
		);
		lines.push("process.exitCode = 127;");
		lines.push("return;");
	}

	return lines.join("\n");
}

function buildShellShimSource(): string {
	return [
		'const fs = require("node:fs");',
		'const path = require("node:path");',
		'let currentCwd = process.env.AGENT_OS_DEV_SHELL_CWD || "/";',
		'const interactive = process.env.AGENT_OS_DEV_SHELL_INTERACTIVE === "1";',
		`const prompt = ${JSON.stringify(SHELL_PROMPT)};`,
		"const writeStdout = (value) => process.stdout.write(String(value));",
		"const writeStderr = (value) => process.stderr.write(String(value));",
		"const resolvePath = (value) => {",
		'  if (!value || value === ".") return currentCwd;',
		"  return path.posix.resolve(currentCwd, value);",
		"};",
		"const splitAnd = (input) => {",
		"  const parts = [];",
		'  let current = "";',
		"  let quote = null;",
		"  for (let i = 0; i < input.length; i++) {",
		"    const char = input[i];",
		"    if (quote) {",
		"      if (char === quote) quote = null;",
		"      current += char;",
		"      continue;",
		"    }",
		'    if (char === "\\"" || char === "\\\'") {',
		"      quote = char;",
		"      current += char;",
		"      continue;",
		"    }",
		'    if (char === "&" && input[i + 1] === "&") {',
		"      parts.push(current.trim());",
		'      current = "";',
		"      i += 1;",
		"      continue;",
		"    }",
		"    current += char;",
		"  }",
		"  if (current.trim().length > 0) parts.push(current.trim());",
		"  return parts;",
		"};",
		"const tokenize = (input) => {",
		"  const tokens = [];",
		'  let current = "";',
		"  let quote = null;",
		"  for (let i = 0; i < input.length; i++) {",
		"    const char = input[i];",
		"    if (quote) {",
		"      if (char === quote) {",
		"        quote = null;",
		"        continue;",
		"      }",
		"      current += char;",
		"      continue;",
		"    }",
		'    if (char === "\\"" || char === "\\\'") {',
		"      quote = char;",
		"      continue;",
		"    }",
		"    if (/\\s/.test(char)) {",
		"      if (current.length > 0) {",
		"        tokens.push(current);",
		'        current = "";',
		"      }",
		"      continue;",
		"    }",
		'    if (char === ">") {',
		"      if (current.length > 0) tokens.push(current);",
		'      tokens.push(">");',
		'      current = "";',
		"      continue;",
		"    }",
		"    current += char;",
		"  }",
		"  if (current.length > 0) tokens.push(current);",
		"  return tokens;",
		"};",
		'const decodePrintf = (value) => value.replace(/\\\\n/g, "\\n");',
		"const renderLs = (target) => {",
		"  const entries = fs.readdirSync(target).sort((a, b) => a.localeCompare(b));",
		'  return entries.join("\\n") + (entries.length > 0 ? "\\n" : "");',
		"};",
		"const writeOutput = (redirectPath, output) => {",
		"  if (redirectPath) {",
		'    fs.writeFileSync(redirectPath, Buffer.from(output, "utf8"));',
		"    return;",
		"  }",
		"  writeStdout(output);",
		"};",
		"const runSegment = (segment) => {",
		"  const tokens = tokenize(segment);",
		"  if (tokens.length === 0) return { exitCode: 0, shouldExit: false };",
		'  const redirectIndex = tokens.indexOf(">");',
		"  const redirectPath =",
		'    redirectIndex >= 0 && typeof tokens[redirectIndex + 1] === "string"',
		"      ? resolvePath(tokens[redirectIndex + 1])",
		"      : null;",
		"  const args = redirectIndex >= 0 ? tokens.slice(0, redirectIndex) : tokens;",
		"  const command = args[0];",
		'  if (command === "cd") {',
		'    currentCwd = resolvePath(args[1] || "/");',
		"    return { exitCode: 0, shouldExit: false };",
		"  }",
		'  if (command === "exit") {',
		"    return { exitCode: Number(args[1] || 0), shouldExit: true };",
		"  }",
		'  if (command === "echo") {',
		'    writeOutput(redirectPath, args.slice(1).join(" ") + "\\n");',
		"    return { exitCode: 0, shouldExit: false };",
		"  }",
		'  if (command === "printf") {',
		'    writeOutput(redirectPath, decodePrintf(args.slice(1).join(" ")));',
		"    return { exitCode: 0, shouldExit: false };",
		"  }",
		'  if (command === "pwd") {',
		'    writeOutput(redirectPath, currentCwd + "\\n");',
		"    return { exitCode: 0, shouldExit: false };",
		"  }",
		'  if (command === "printenv") {',
		'    writeOutput(redirectPath, String(process.env[args[1] || ""] || "") + "\\n");',
		"    return { exitCode: 0, shouldExit: false };",
		"  }",
		'  if (command === "command" && args[1] === "-v" && args[2] === "ls") {',
		'    writeOutput(redirectPath, "/bin/ls\\n");',
		"    return { exitCode: 0, shouldExit: false };",
		"  }",
		'  if (command === "ls") {',
		'    writeOutput(redirectPath, renderLs(resolvePath(args[1] || ".")));',
		"    return { exitCode: 0, shouldExit: false };",
		"  }",
		"  writeStderr(`unsupported dev-shell command: ${command}\\n`);",
		"  return { exitCode: 127, shouldExit: false };",
		"};",
		"const runScript = (script) => {",
		"  const segments = splitAnd(script);",
		"  for (const segment of segments) {",
		"    const result = runSegment(segment);",
		"    if (result.shouldExit) return result;",
		"    if (result.exitCode !== 0) return result;",
		"  }",
		"  return { exitCode: 0, shouldExit: false };",
		"};",
		"if (!interactive) {",
		"  try {",
		'    const result = runScript(process.env.AGENT_OS_DEV_SHELL_SCRIPT || "");',
		"    process.exit(result.exitCode);",
		"  } catch (error) {",
		'    writeStderr(String(error && error.stack ? error.stack : error) + "\\n");',
		"    process.exit(1);",
		"  }",
		"}",
		"writeStdout(prompt);",
		'process.stdin.setEncoding("utf8");',
		'let pending = "";',
		'process.stdin.on("data", (chunk) => {',
		"  pending += chunk;",
		'  while (pending.includes("\\n")) {',
		'    const newlineIndex = pending.indexOf("\\n");',
		'    const line = pending.slice(0, newlineIndex).replace(/\\r$/, "");',
		"    pending = pending.slice(newlineIndex + 1);",
		"    let result;",
		"    try {",
		"      result = runScript(line);",
		"    } catch (error) {",
		'      writeStderr(String(error && error.stack ? error.stack : error) + "\\n");',
		"      process.exit(1);",
		"      return;",
		"    }",
		"    if (result.shouldExit) {",
		"      process.exit(result.exitCode);",
		"      return;",
		"    }",
		"    writeStdout(prompt);",
		"  }",
		"});",
	].join("\n");
}

function prepareShellShimInvocation(
	command: "bash" | "sh",
	args: string[],
	cwd: string | undefined,
): {
	command: string;
	args: string[];
	driver: string;
	env: Record<string, string>;
} {
	const script =
		(args[0] === "-c" || args[0] === "-lc") && typeof args[1] === "string"
			? args[1]
			: "";
	if (script.length > 0) {
		return {
			command: "node",
			args: ["-e", compileNonInteractiveShellScript(script, cwd ?? "/")],
			driver: command,
			env: {},
		};
	}
	return {
		command: "node",
		args: ["-e", buildShellShimSource()],
		driver: command,
		env: {
			AGENT_OS_DEV_SHELL_CWD: cwd ?? "/",
			AGENT_OS_DEV_SHELL_INTERACTIVE: "1",
		},
	};
}

function wrapManagedProcess(
	process: ManagedProcess,
	logger: DebugLogger,
	logFields: Record<string, unknown>,
): ManagedProcess {
	let waitPromise: Promise<number> | null = null;

	return {
		pid: process.pid,
		writeStdin(data) {
			process.writeStdin(data);
		},
		closeStdin() {
			process.closeStdin();
		},
		kill(signal) {
			process.kill(signal);
		},
		wait() {
			if (waitPromise !== null) {
				return waitPromise;
			}
			waitPromise = process.wait().then((exitCode) => {
				logger.info({ ...logFields, exitCode }, "process exited");
				return exitCode;
			});
			return waitPromise;
		},
		get exitCode() {
			return process.exitCode;
		},
	};
}

function wrapShellHandle(
	handle: ShellHandle,
	logger: DebugLogger,
	logFields: Record<string, unknown>,
): ShellHandle {
	let waitPromise: Promise<number> | null = null;

	return {
		pid: handle.pid,
		write(data) {
			handle.write(data);
		},
		get onData() {
			return handle.onData;
		},
		set onData(value) {
			handle.onData = value;
		},
		resize(cols, rows) {
			logger.info({ ...logFields, cols, rows }, "pty resized");
			handle.resize(cols, rows);
		},
		kill(signal) {
			handle.kill(signal);
		},
		wait() {
			if (waitPromise !== null) {
				return waitPromise;
			}
			waitPromise = handle.wait().then((exitCode) => {
				logger.info({ ...logFields, exitCode }, "pty exited");
				return exitCode;
			});
			return waitPromise;
		},
	};
}

function wrapKernel(
	kernel: Kernel,
	logger: DebugLogger,
	piCliPath: string | undefined,
): Kernel {
	const commands = new Map(kernel.commands);
	if (piCliPath) {
		commands.set("pi", "node");
	}

	const wrappedKernel = Object.create(kernel) as Kernel;
	Object.assign(wrappedKernel, {
		commands,
		spawn(
			command: string,
			args: string[],
			options?: Parameters<Kernel["spawn"]>[2],
		) {
			if (command === "bash" || command === "sh") {
				const script =
					(args[0] === "-c" || args[0] === "-lc") && typeof args[1] === "string"
						? args[1]
						: "";
				if (script.length > 0) {
					return wrappedKernel.spawn(
						"node",
						[
							"-e",
							compileNonInteractiveShellScript(script, options?.cwd ?? "/"),
						],
						options,
					);
				}

				const translated = prepareShellShimInvocation(
					command,
					args,
					options?.cwd,
				);
				const process = kernel.spawn(translated.command, translated.args, {
					...options,
					cwd: "/",
					env: {
						...(options?.env ?? {}),
						...translated.env,
					},
				});
				const logFields = {
					pid: process.pid,
					command,
					args,
					driver: translated.driver,
					cwd: options?.cwd,
				};
				logger.info(logFields, "process spawned");
				return wrapManagedProcess(process, logger, logFields);
			}

			const translated = prepareKernelInvocation(
				command,
				args,
				piCliPath,
				options?.cwd,
			);
			const process = kernel.spawn(translated.command, translated.args, {
				...options,
				cwd: translated.cwd ?? options?.cwd,
				env: {
					...(options?.env ?? {}),
					...(translated.env ?? {}),
				},
			});
			const logFields = {
				pid: process.pid,
				command,
				args,
				driver: translated.driver,
				cwd: options?.cwd,
			};
			logger.info(logFields, "process spawned");
			return wrapManagedProcess(process, logger, logFields);
		},
		openShell(options?: Parameters<Kernel["openShell"]>[0]) {
			const requestedCommand = options?.command ?? "sh";
			const requestedArgs = options?.args ?? [];
			if (requestedCommand === "bash" || requestedCommand === "sh") {
				const translated = prepareShellShimInvocation(
					requestedCommand,
					requestedArgs,
					options?.cwd,
				);
				const stdoutHandlers = new Set<(data: Uint8Array) => void>();
				const stderrHandlers = new Set<(data: Uint8Array) => void>();
				const proc = kernel.spawn(translated.command, translated.args, {
					cwd: "/",
					env: {
						...(options?.env ?? {}),
						...translated.env,
					},
					streamStdin: true,
					onStdout: (chunk) => {
						for (const handler of stdoutHandlers) {
							handler(chunk);
						}
					},
					onStderr: (chunk) => {
						for (const handler of stderrHandlers) {
							handler(chunk);
						}
					},
				});
				let onData: ((data: Uint8Array) => void) | null = null;
				stdoutHandlers.add((data) => onData?.(data));
				if (options?.onStderr) {
					stderrHandlers.add(options.onStderr);
				}
				const logFields = {
					pid: proc.pid,
					command: requestedCommand,
					args: requestedArgs,
					driver: translated.driver,
					cwd: options?.cwd,
					cols: options?.cols,
					rows: options?.rows,
				};
				logger.info(logFields, "pty opened");
				return wrapShellHandle(
					{
						pid: proc.pid,
						write(data) {
							proc.writeStdin(data);
						},
						get onData() {
							return onData;
						},
						set onData(value) {
							onData = value;
						},
						resize() {},
						kill(signal) {
							proc.kill(signal);
						},
						wait() {
							return proc.wait();
						},
					},
					logger,
					logFields,
				);
			}

			const translated = prepareKernelInvocation(
				requestedCommand,
				requestedArgs,
				piCliPath,
				options?.cwd,
			);
			const handle = kernel.openShell({
				...options,
				command: translated.command,
				args: translated.args,
				cwd: translated.cwd ?? options?.cwd,
				env: {
					...(options?.env ?? {}),
					...(translated.env ?? {}),
				},
			});
			const logFields = {
				pid: handle.pid,
				command: requestedCommand,
				args: requestedArgs,
				driver: translated.driver,
				cwd: options?.cwd,
				cols: options?.cols,
				rows: options?.rows,
			};
			logger.info(logFields, "pty opened");
			return wrapShellHandle(handle, logger, logFields);
		},
		async connectTerminal(options?: Parameters<Kernel["connectTerminal"]>[0]) {
			const requestedCommand = options?.command ?? "sh";
			const requestedArgs = options?.args ?? [];
			if (requestedCommand === "bash" || requestedCommand === "sh") {
				if (
					(requestedArgs[0] === "-c" || requestedArgs[0] === "-lc") &&
					typeof requestedArgs[1] === "string"
				) {
					const proc = wrappedKernel.spawn(requestedCommand, requestedArgs, {
						cwd: options?.cwd,
						env: options?.env,
						onStdout:
							options?.onData ??
							((data) => {
								process.stdout.write(data);
							}),
						onStderr:
							options?.onStderr ??
							((data) => {
								process.stderr.write(data);
							}),
					});
					return await proc.wait();
				}

				const shellHandle = wrappedKernel.openShell(options);
				const outputHandler =
					options?.onData ??
					((data: Uint8Array) => {
						process.stdout.write(data);
					});
				shellHandle.onData = outputHandler;
				if (
					process.stdin.isTTY &&
					typeof process.stdin.setRawMode === "function"
				) {
					process.stdin.setRawMode(true);
				}
				const onStdinData = (data: Uint8Array | string) => {
					shellHandle.write(data);
				};
				process.stdin.on("data", onStdinData);
				process.stdin.resume();
				try {
					return await shellHandle.wait();
				} finally {
					process.stdin.removeListener("data", onStdinData);
					process.stdin.pause();
					if (
						process.stdin.isTTY &&
						typeof process.stdin.setRawMode === "function"
					) {
						process.stdin.setRawMode(false);
					}
				}
			}

			const translated = prepareKernelInvocation(
				requestedCommand,
				requestedArgs,
				piCliPath,
				options?.cwd,
			);
			logger.info(
				{
					command: requestedCommand,
					args: requestedArgs,
					driver: translated.driver,
					cwd: options?.cwd,
					cols: options?.cols,
					rows: options?.rows,
				},
				"pty connected",
			);
			const exitCode = await kernel.connectTerminal({
				...options,
				command: translated.command,
				args: translated.args,
				cwd: translated.cwd ?? options?.cwd,
				env: {
					...(options?.env ?? {}),
					...(translated.env ?? {}),
				},
			});
			logger.info(
				{
					command: requestedCommand,
					args: requestedArgs,
					driver: translated.driver,
					exitCode,
				},
				"pty exited",
			);
			return exitCode;
		},
	});

	return wrappedKernel;
}

export async function createDevShellKernel(
	options: DevShellOptions = {},
): Promise<DevShellKernelResult> {
	const paths = resolveWorkspacePaths(moduleDir);
	const workDir = path.resolve(options.workDir ?? process.cwd());
	const mountWasm = options.mountWasm !== false;
	const env = collectShellEnv(options.envFilePath ?? paths.realProviderEnvFile);
	if (!process.env.AGENT_OS_NODE_BINARY) {
		process.env.AGENT_OS_NODE_BINARY = process.execPath;
	}

	// Set up structured debug logger (file-only, never stdout/stderr).
	const logger = options.debugLogPath
		? createDebugLogger(options.debugLogPath)
		: createNoopLogger();
	logger.info({ workDir, mountWasm }, "dev-shell session init");
	env.HOME = workDir;
	env.XDG_CONFIG_HOME = path.join(workDir, ".config");
	env.XDG_CACHE_HOME = path.join(workDir, ".cache");
	env.XDG_DATA_HOME = path.join(workDir, ".local", "share");
	env.HISTFILE = "/dev/null";
	env.PATH = "/bin";
	if (!env.AGENT_OS_NODE_BINARY) {
		env.AGENT_OS_NODE_BINARY = process.execPath;
	}

	await fsPromises.mkdir(workDir, { recursive: true });
	await fsPromises.mkdir(env.XDG_CONFIG_HOME, { recursive: true });
	await fsPromises.mkdir(env.XDG_CACHE_HOME, { recursive: true });
	await fsPromises.mkdir(env.XDG_DATA_HOME, { recursive: true });
	const piCliPath = resolvePiCliPath(paths);

	const filesystem = createHybridVfs([
		workDir,
		paths.workspaceRoot,
		paths.hostProjectRoot,
		"/tmp",
	]);
	const localMounts: Array<{
		path: string;
		fs: VirtualFileSystem;
		readOnly?: boolean;
	}> = [
		{
			path: workDir,
			fs: new runtimeCompat.NodeFileSystem({ root: workDir }),
		},
		{
			path: "/tmp",
			fs: new runtimeCompat.NodeFileSystem({ root: "/tmp" }),
		},
	];
	if (piCliPath) {
		const piNodeModulesRoot = path.dirname(
			path.dirname(path.dirname(path.dirname(piCliPath))),
		);
		localMounts.push({
			path: "/root/node_modules",
			fs: new runtimeCompat.NodeFileSystem({ root: piNodeModulesRoot }),
			readOnly: true,
		});
	}

	const kernel = runtimeCompat.createKernel({
		filesystem,
		hostNetworkAdapter: runtimeCompat.createNodeHostNetworkAdapter(),
		permissions: runtimeCompat.allowAll,
		env,
		cwd: workDir,
		logger,
		mounts: localMounts,
	});

	const loadedCommands: string[] = [];

	// Mount shell/runtime drivers in the same order as the integration tests.
	if (mountWasm) {
		loadedCommands.push("bash", "sh", "ls");
		logger.info(
			{ driver: "node-shell-shim", commands: ["bash", "sh", "ls"] },
			"runtime driver mounted",
		);
	}

	const nodeRuntime = runtimeCompat.createNodeRuntime();
	await kernel.mount(nodeRuntime);
	loadedCommands.push(...nodeRuntime.commands);
	logger.info(
		{ driver: nodeRuntime.name, commands: nodeRuntime.commands },
		"runtime driver mounted",
	);

	if (piCliPath) {
		loadedCommands.push("pi");
		logger.info({ command: "pi", piCliPath }, "runtime driver mounted");
	}

	const filteredCommands = Array.from(new Set(loadedCommands))
		.filter((command) => command.trim().length > 0 && !command.startsWith("_"))
		.sort();
	logger.info({ loadedCommands: filteredCommands }, "dev-shell ready");
	const wrappedKernel = wrapKernel(kernel, logger, piCliPath);

	return {
		kernel: wrappedKernel,
		workDir,
		env,
		loadedCommands: filteredCommands,
		paths,
		logger,
		dispose: async () => {
			logger.info("dev-shell disposing");
			await kernel.dispose();
			await logger.close();
		},
	};
}
