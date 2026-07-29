/**
 * NodeRuntime — ergonomic façade for running guest JavaScript end-to-end.
 *
 * Boots a fully virtualized VM (via the native sidecar) and runs guest Node
 * programs with minimal boilerplate. All of the sidecar spawn, session
 * handshake, VM creation, root filesystem bootstrap, runtime-driver mounting,
 * and lifecycle waiting are hidden behind `NodeRuntime.create()`.
 *
 * ```ts
 * const rt = await NodeRuntime.create();
 * const { stdout, exitCode } = await rt.exec("console.log('hi', 1 + 1)");
 * await rt.dispose();
 * ```
 *
 * Guest code is written to an ESM module inside the VM and executed as
 * `node <file>` through the kernel, so all execution stays inside the kernel
 * isolation boundary — no host escapes, no real Node.js builtins for guest
 * work.
 */

import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import type {
	ExecResult,
	BindingDefinition,
	Kernel,
	KernelBootTiming,
	Permissions,
	VirtualDirEntry,
	VirtualFileSystem,
} from "./test-runtime.js";
import type { JsRuntimeConfig } from "./generated/JsRuntimeConfig.js";
import type { VmUserConfig } from "./generated/VmUserConfig.js";
import type { SidecarProcess } from "./sidecar-process.js";
import {
	createKernel,
	createNodeRuntime,
	createWasmVmRuntime,
	NodeFileSystem,
} from "./test-runtime.js";
import { parseNodeRuntimeCreateOptions } from "./node-runtime-options-schema.js";

export type {
	BindingDefinition,
	BindingExample,
	VirtualDirEntry,
} from "./test-runtime.js";
export { resolveNodeRuntimeSidecarBinary } from "./test-runtime.js";

export type NodeRuntimeBootTimingPhase =
	| KernelBootTiming["phase"]
	| "runtime_mount_wasm"
	| "runtime_mount_node"
	| "bindings";

export interface NodeRuntimeBootTiming {
	phase: NodeRuntimeBootTimingPhase;
	durationMs: number;
}

/** Repository root, used to locate the in-repo WASM command build output. */
const REPO_ROOT = fileURLToPath(new URL("../../..", import.meta.url));

/**
 * In-repo build output for the WASM coreutils/shell command binaries, produced
 * by the Rust command build (`make -C toolchain wasm`). Only present in a
 * developer checkout; preferred when it exists so local edits are picked up
 * without re-vendoring.
 */
const REPO_COMMANDS_DIR = path.join(
	REPO_ROOT,
	"toolchain/target/wasm32-wasip1/release/commands",
);

/**
 * Commands vendored into the published `@rivet-dev/agentos-runtime-core` package by
 * `scripts/copy-wasm-commands.mjs` (listed in `files` as `commands`). This is
 * the directory a real `npm install agentos` resolves: from the compiled
 * `dist/node-runtime.js` it sits at `<package>/commands`. This is the analogue
 * of how the sidecar binary ships inside `@rivet-dev/agentos-runtime-sidecar`.
 */
const BUNDLED_COMMANDS_DIR = fileURLToPath(
	new URL("../commands", import.meta.url),
);

/**
 * Resolve the directory holding the WASM command binaries (the source of the
 * guest `sh` the kernel needs to spawn any process). Precedence:
 *
 *   1. explicit `commandsDir` option,
 *   2. `AGENTOS_WASM_COMMANDS_DIR` env var,
 *   3. the in-repo build output (developer checkout), when present,
 *   4. the commands vendored into the installed package (published installs).
 *
 * The in-repo path wins over the bundled copy so local development picks up
 * freshly built commands without re-vendoring. A fresh `npm install` has no
 * in-repo path, so it falls through to the bundled copy.
 */
export function resolveNodeRuntimeCommandsDir(explicit?: string): string {
	if (explicit !== undefined) {
		return explicit;
	}
	const fromEnv = process.env.AGENTOS_WASM_COMMANDS_DIR;
	if (fromEnv) {
		return fromEnv;
	}
	if (existsSync(REPO_COMMANDS_DIR)) {
		return REPO_COMMANDS_DIR;
	}
	return BUNDLED_COMMANDS_DIR;
}

/**
 * Secure-by-default permission policy applied when the caller passes no
 * `permissions`. Outward-facing capabilities are denied: there is **no network
 * access** (and no host callbacks) by default — guest code cannot reach the
 * network until you opt in. The filesystem, child-process, process, and env
 * scopes are allowed because they are fully virtualized (the guest only ever
 * sees the VM's in-memory filesystem and kernel-managed processes, never the
 * real host) and are required for the runtime to execute a guest program at
 * all. Tighten or widen any scope by passing your own `permissions`.
 */
const DEFAULT_PERMISSIONS: Permissions = {
	fs: "allow",
	childProcess: "allow",
	process: "allow",
	env: "allow",
	network: "deny",
};

/**
 * Options for {@link NodeRuntime.create}.
 *
 * Keep this public interface in sync with
 * `packages/core/src/node-runtime-options-schema.ts::nodeRuntimeCreateOptionsSchema`.
 * Options that translate into sidecar VM JSON must also stay aligned with
 * `crates/vm-config/src/lib.rs::CreateVmConfig`.
 */
export interface NodeRuntimeCreateOptions {
	/**
	 * Caller-owned filesystem used only by this low-level compatibility runtime.
	 * AgentOS clients do not create a TypeScript filesystem implicitly; normal
	 * product code should use the sidecar-owned VFS through `AgentOs`.
	 */
	filesystem: VirtualFileSystem;
	/** Environment variables visible to guest processes. */
	env?: Record<string, string>;
	/** Initial working directory for guest processes. Defaults to `/workspace`. */
	cwd?: string;
	/** Initial virtual Linux credentials and account record. Defaults to `1000:1000` (`agentos`). */
	user?: VmUserConfig;
	/**
	 * Permission policy for the VM. Merged over a secure default that **denies
	 * network access** (guest code cannot reach the network until you opt in);
	 * the virtualized filesystem and processes stay enabled so programs run.
	 * Because it merges, a partial policy works: `{ network: "allow" }` grants
	 * the network while keeping the execution essentials. Pass a fuller policy
	 * (rule sets) to further sandbox individual scopes.
	 */
	permissions?: Permissions;
	/**
	 * Override the directory containing the WASM command binaries (the source of
	 * the guest `sh`). When unset, resolution falls back through the
	 * `AGENTOS_WASM_COMMANDS_DIR` environment variable, the in-repo build
	 * output (developer checkouts), then the commands vendored into the installed
	 * `@rivet-dev/agentos-runtime-core` package (published installs).
	 */
	commandsDir?: string;
	/**
	 * Additional directories of wasm32-wasip1 commands to register in the VM.
	 * Intended for low-level tooling and benchmark harnesses; normal callers use
	 * the bundled shell/coreutils command directory resolved by `commandsDir`.
	 */
	wasmCommandDirs?: string[];
	/**
	 * Existing native sidecar process to use for this runtime. Omit this to use
	 * the default shared sidecar behavior. When provided, the runtime owns only
	 * its VM and leaves sidecar process disposal to the caller.
	 */
	sidecar?: SidecarProcess;
	/** Receives coarse boot phase timings for benchmarks and diagnostics. */
	onBootTiming?: (timing: NodeRuntimeBootTiming) => void;
	/**
	 * Files to seed into the VM's virtual filesystem before the guest runs,
	 * keyed by absolute guest path. Parent directories are created as needed.
	 * Use this to project host assets, npm packages, or fixtures into the
	 * sandbox so guest code can `import`/`require`/read them. The bytes are
	 * copied into the VM's in-memory filesystem; the host filesystem is never
	 * exposed to the guest.
	 *
	 * ```ts
	 * const rt = await NodeRuntime.create({
	 *   files: { "/root/data.json": '{"ok":true}' },
	 * });
	 * ```
	 */
	files?: Record<string, string | Uint8Array>;
	/**
	 * Host directories to project into the VM's virtual filesystem, Docker-style.
	 * Each mount makes a host directory readable at a guest path. Files are read
	 * lazily from the host as the guest accesses them, so large trees (for
	 * example a `node_modules` package such as the TypeScript compiler) are
	 * projected without copying their bytes up front. The guest sees only the
	 * mounted subtree, never the wider host filesystem.
	 *
	 * ```ts
	 * const rt = await NodeRuntime.create({
	 *   mounts: [
	 *     {
	 *       guestPath: "/root/node_modules/typescript",
	 *       hostPath: "/abs/path/to/node_modules/typescript",
	 *       readOnly: true,
	 *     },
	 *   ],
	 * });
	 * ```
	 */
	mounts?: HostDirectoryMount[];
	/**
	 * Mount a host `node_modules` directory into the VM in one call so guest
	 * `import`/`require` resolve real, host-installed npm packages.
	 *
	 * Pass the absolute host path to a `node_modules` directory (or an object
	 * with that path and an explicit guest location). The whole directory is
	 * projected lazily, Docker-style, at a guest `node_modules` on the resolution
	 * path, so any package inside it resolves the way Node would over a real
	 * filesystem (ancestor `node_modules` walk, `exports`/conditions, symlinks).
	 * This is the ergonomic alternative to wiring up individual `mounts` entries
	 * per package.
	 *
	 * By default the directory is mounted at `/tmp/node_modules`, which is where
	 * the resolution walk for a program run by {@link NodeRuntime.exec} /
	 * {@link NodeRuntime.run} begins (each program is written under `/tmp`). Pass
	 * the object form with `guestPath` to mount it elsewhere on a different
	 * module's resolution path.
	 *
	 * ```ts
	 * const rt = await NodeRuntime.create({
	 *   nodeModules: "/abs/path/to/project/node_modules",
	 * });
	 * await rt.exec(`
	 *   import isNumber from "is-number";
	 *   console.log(isNumber(42));
	 * `);
	 * ```
	 *
	 * The host filesystem is never exposed beyond the mounted `node_modules`
	 * subtree. The mount is read-only.
	 */
	nodeModules?: string | NodeModulesMount;
	/**
	 * Host-side bindings the guest can invoke as shell commands. Each entry is
	 * registered as a named guest command; when the guest runs it, the
	 * invocation round-trips back to the host and runs the binding's `handler`,
	 * whose return value is delivered back to the guest. This is how you give
	 * sandboxed guest code controlled, named host capabilities (the kind an AI
	 * agent calls as tools) without granting it the underlying access directly.
	 *
	 * The guest invokes a binding by name with JSON input:
	 *
	 * ```ts
	 * const rt = await NodeRuntime.create({
	 *   bindings: {
	 *     add: {
	 *       description: "Add two numbers",
	 *       inputSchema: {
	 *         type: "object",
	 *         properties: { a: { type: "number" }, b: { type: "number" } },
	 *         required: ["a", "b"],
	 *       },
	 *       handler: ({ a, b }: { a: number; b: number }) => ({ sum: a + b }),
	 *     },
	 *   },
	 * });
	 * await rt.exec(`
	 *   import { execFileSync } from "node:child_process";
	 *   const out = execFileSync("add", ["add", "--json", JSON.stringify({ a: 2, b: 3 })]);
	 *   console.log(out.toString());
	 * `);
	 * ```
	 *
	 * When `bindings` is provided and no `binding` permission scope is set, the
	 * `binding` scope is granted so the registered bindings are invocable; pass
	 * your own `permissions.binding` policy to gate individual bindings.
	 */
	bindings?: Record<string, BindingDefinition>;
	/**
	 * Guest-bound ports that may accept non-loopback connections. By default a
	 * guest server is reachable only over loopback inside the VM; listing a port
	 * here lifts that restriction for that port, letting connections from outside
	 * the loopback interface reach it. Use this for guests that run servers which
	 * must accept external connections (for example a dev server you expose
	 * beyond loopback).
	 *
	 * ```ts
	 * const rt = await NodeRuntime.create({
	 *   permissions: { network: "allow" },
	 *   loopbackExemptPorts: [3000],
	 * });
	 * ```
	 */
	loopbackExemptPorts?: number[];
	/**
	 * Low-level guest JavaScript runtime configuration. Most callers should leave
	 * this unset. Benchmarks may opt in to `highResolutionTime`, which disables
	 * the default 1ms timer quantization and should not be enabled for untrusted
	 * workloads.
	 */
	jsRuntime?: Partial<JsRuntimeConfig>;
}

/** A host directory projected into the VM's virtual filesystem. */
export interface HostDirectoryMount {
	/** Absolute guest path the directory appears at inside the VM. */
	guestPath: string;
	/** Absolute host directory to project (read through the VFS, lazily). */
	hostPath: string;
	/** Mount read-only (the default). Pass `false` to allow guest writes. */
	readOnly?: boolean;
}

/** Guest path a `nodeModules` mount is projected at by default. */
const DEFAULT_NODE_MODULES_GUEST_PATH = "/tmp/node_modules";

/**
 * Object form of the `nodeModules` create option: a host `node_modules`
 * directory to project, optionally at an explicit guest path. The string form
 * (`nodeModules: "/abs/node_modules"`) is shorthand for `{ hostPath }`.
 */
export interface NodeModulesMount {
	/** Absolute host `node_modules` directory to project (read lazily). */
	hostPath: string;
	/**
	 * Absolute guest path to mount it at. Defaults to `/tmp/node_modules`, where
	 * the resolution walk for {@link NodeRuntime.exec} / {@link NodeRuntime.run}
	 * programs begins. Override to put it on a different module's resolution path.
	 */
	guestPath?: string;
}

/** Result of {@link NodeRuntime.exec}. */
export interface NodeRuntimeExecResult {
	stdout: string;
	stderr: string;
	exitCode: number;
}

/** Options for a single {@link NodeRuntime.exec} call. */
export interface NodeRuntimeExecOptions {
	/** Extra environment variables for this run, merged over the VM env. */
	env?: Record<string, string>;
	/** Working directory for this run. */
	cwd?: string;
	/** Data piped to the guest program's stdin. */
	stdin?: string | Uint8Array;
	/** Abort the run after this many milliseconds. */
	timeout?: number;
	/**
	 * Cancel the run when this signal aborts. On abort the guest process is
	 * killed inside the VM (the kernel delivers `SIGTERM`) and the call rejects
	 * with the signal's abort reason. Use this to cancel an in-flight run from
	 * the outside, for example to enforce your own deadline or stop work when a
	 * request is canceled.
	 *
	 * ```ts
	 * const controller = new AbortController();
	 * const pending = rt.exec("while (true) {}", { signal: controller.signal });
	 * controller.abort();
	 * await pending.catch((err) => console.log(err.name)); // "AbortError"
	 * ```
	 */
	signal?: AbortSignal;
	/**
	 * Called with each chunk the guest writes to stdout as it is produced,
	 * letting you observe output incrementally instead of waiting for the run to
	 * finish. Chunks arrive as raw bytes; decode with a `TextDecoder` for text.
	 * The complete output is still returned as `result.stdout` when the run ends.
	 */
	onStdout?: (chunk: Uint8Array) => void;
	/**
	 * Called with each chunk the guest writes to stderr as it is produced,
	 * letting you observe output incrementally instead of waiting for the run to
	 * finish. Chunks arrive as raw bytes; decode with a `TextDecoder` for text.
	 * The complete output is still returned as `result.stderr` when the run ends.
	 */
	onStderr?: (chunk: Uint8Array) => void;
}

/** The HTTP request {@link NodeRuntime.fetch} drives into the VM. */
export interface NodeRuntimeFetchInput {
	/** HTTP method. Defaults to `GET`. */
	method?: string;
	/** Request path (and query), e.g. `/api/users?limit=10`. */
	path: string;
	/** Request headers. */
	headers?: Record<string, string>;
	/** Request body. */
	body?: string | Uint8Array;
}

/** The HTTP response {@link NodeRuntime.fetch} returns from the VM. */
export interface NodeRuntimeFetchResponse {
	/** HTTP status code, e.g. `200`. */
	status: number;
	/** HTTP status text, e.g. `OK`. */
	statusText: string;
	/** Response headers, lower-cased by name. */
	headers: Record<string, string>;
	/** Response body decoded as UTF-8 text. */
	body: string;
}

/**
 * Options for {@link NodeRuntime.spawn}. Inherits the streaming `onStdout` /
 * `onStderr` hooks from {@link NodeRuntimeExecOptions}.
 */
export interface NodeRuntimeSpawnOptions extends NodeRuntimeExecOptions {}

/**
 * Describes the guest TCP listener to wait for with
 * {@link NodeRuntime.waitForListener} or look up with
 * {@link NodeRuntime.findListener}. A listener matches when a guest process is
 * accepting connections on the given `port` (and `host`/`path` when supplied).
 */
export interface NodeRuntimeListenerQuery {
	/** TCP port the guest listener is bound to, e.g. `3000`. */
	port: number;
	/** Bind host to match, e.g. `127.0.0.1`. Omit to match any host. */
	host?: string;
	/** Unix socket path to match, for path-bound listeners. */
	path?: string;
}

/**
 * A matched guest listener returned by {@link NodeRuntime.waitForListener} and
 * {@link NodeRuntime.findListener}. `processId` identifies the guest process
 * that owns the listening socket; the `host`/`port`/`path` it is bound to are
 * reported when known.
 */
export interface NodeRuntimeListener {
	/** The guest process id that owns the listening socket. */
	processId: string;
	/** The host the listener is bound to, when reported. */
	host?: string;
	/** The port the listener is bound to, when reported. */
	port?: number;
	/** The unix socket path the listener is bound to, when reported. */
	path?: string;
}

/** Options for {@link NodeRuntime.waitForListener}. */
export interface NodeRuntimeWaitForListenerOptions {
	/**
	 * Give up after this many milliseconds and reject. Defaults to 10000. The
	 * wait also rejects if the bound `signal` aborts first.
	 */
	timeoutMs?: number;
	/** Abort the wait early; the returned promise rejects when it fires. */
	signal?: AbortSignal;
	/**
	 * How long to wait between listener lookups while polling, in milliseconds.
	 * Defaults to 50.
	 */
	pollIntervalMs?: number;
}

/**
 * A live handle to a guest process started with {@link NodeRuntime.spawn}.
 *
 * Unlike {@link NodeRuntime.exec}, which runs a program to completion and
 * returns its captured output, a handle is returned immediately while the
 * process keeps running. Use it to stream stdout/stderr, feed stdin, signal or
 * kill the process, and await its exit. This is the building block for
 * long-running guests such as dev servers: start one here, then drive requests
 * into it with {@link NodeRuntime.fetch}.
 */
export interface NodeRuntimeProcess {
	/** The guest process id. */
	readonly pid: number;
	/** Write data to the guest process's stdin. */
	writeStdin(data: string | Uint8Array): void;
	/** Close the guest process's stdin, signalling end-of-input. */
	closeStdin(): void;
	/**
	 * Send a signal to the guest process. Defaults to `SIGTERM`. Accepts a
	 * signal name (e.g. `"SIGKILL"`) or a raw signal number.
	 */
	kill(signal?: NodeJS.Signals | number): void;
	/** Resolve with the guest process's exit code once it terminates. */
	wait(): Promise<number>;
	/** The exit code once the process has exited, or `null` while it runs. */
	readonly exitCode: number | null;
}

export interface NodeRuntimeResourceSnapshot {
	runningProcesses: number;
	exitedProcesses: number;
	fdTables: number;
	openFds: number;
	pipes: number;
	pipeBufferedBytes: number;
	ptys: number;
	ptyBufferedInputBytes: number;
	ptyBufferedOutputBytes: number;
	sockets: number;
	socketListeners: number;
	socketConnections: number;
	socketBufferedBytes: number;
	socketDatagramQueueLen: number;
	queueSnapshots: Array<{
		name: string;
		category: string;
		depth: number;
		highWater: number;
		capacity: number;
		fillPercent: number;
	}>;
}

export interface NodeRuntimeResidentRunnerExecOptions {
	/** Abort the guest eval after this many milliseconds. */
	timeout?: number;
}

export type NodeRuntimeResidentRunnerOptions = {};

export interface NodeRuntimeResidentRunner {
	exec(
		code: string,
		options?: NodeRuntimeResidentRunnerExecOptions,
	): Promise<NodeRuntimeExecResult>;
	dispose(): Promise<void>;
}

/** Result of {@link NodeRuntime.run}. */
export interface NodeRuntimeRunResult<T = unknown> {
	/** The JSON-decoded value the guest produced, when the run succeeded. */
	value?: T;
	stdout: string;
	stderr: string;
	exitCode: number;
}

let nextProgramId = 0;
let nextResidentRequestId = 0;

/**
 * Guest preamble exposing `globalThis.callBinding(name, input?)`: an ergonomic
 * async wrapper over the binding invocation path. It runs the registered binding
 * as the guest would by hand (`<binding> --json <input>` through
 * `node:child_process`), so it inherits every security property of that path:
 * the `binding` permission scope, the binding's input-schema validation, and the
 * host-side handler all still apply. It adds no new trust surface; it only
 * removes the manual `execFile`/JSON boilerplate so guest and agent code can do
 * `const out = await callBinding("add", { a, b })`. The value is a single line
 * so it shifts guest source line numbers by at most one in stack traces.
 *
 * Note: the binding still runs through a guest process. Eliminating that spawn
 * would require a dedicated async guest-to-host binding channel (the synchronous
 * sync-RPC path cannot be used: it runs on the sidecar's main sync-RPC thread and
 * a host round-trip would block it); that is a separate, test-gated change.
 */
const BINDING_PREAMBLE = `globalThis.callBinding = (name, input = {}) => import("node:child_process").then(({ execFile }) => new Promise((resolve, reject) => { execFile(name, [name, "--json", JSON.stringify(input)], { maxBuffer: 64 * 1024 * 1024 }, (error, stdout, stderr) => { if (error) { reject(new Error(String(stderr || "").trim() || error.message)); return; } const text = String(stdout ?? "").trim(); let reply; try { reply = text ? JSON.parse(text) : undefined; } catch { reject(new Error("binding returned invalid JSON: " + text)); return; } if (reply && reply.ok === false) { reject(new Error(reply.error || "binding failed")); return; } resolve(reply && typeof reply === "object" && "result" in reply ? reply.result : reply); }); }));`;

/** Prepend the binding helper preamble to guest program source. */
function withBindingPreamble(code: string): string {
	return `${BINDING_PREAMBLE}\n${code}`;
}

const RESIDENT_READY_PREFIX = "__AGENTOS_RESIDENT_READY__";
const RESIDENT_RESULT_PREFIX = "__AGENTOS_RESIDENT_RESULT__";

/**
 * Ergonomic, batteries-included runtime for executing guest JavaScript.
 *
 * Construct one with {@link NodeRuntime.create}, run programs with
 * {@link NodeRuntime.exec} / {@link NodeRuntime.run}, and release the VM with
 * {@link NodeRuntime.dispose}. A single instance can run many programs; each
 * call executes a fresh guest process.
 */
export class NodeRuntime {
	private constructor(private readonly kernel: Kernel) {}

	/**
	 * Boot a VM and return a ready-to-use runtime. Spawns the sidecar, opens a
	 * session, creates the VM with a bootstrapped root filesystem, mounts the
	 * shell and Node runtimes, and waits for the VM to report ready.
	 */
	static async create(
		options: NodeRuntimeCreateOptions,
	): Promise<NodeRuntime> {
		options = parseNodeRuntimeCreateOptions(options);
		const commandsDir = resolveNodeRuntimeCommandsDir(options.commandsDir);

		// Seed caller-provided files into the VM's in-memory filesystem before
		// boot so they are part of the root filesystem snapshot the guest sees
		// (e.g. projected npm packages or fixtures). The host filesystem is
		// never exposed; only these bytes are copied in.
		const filesystem = options.filesystem;
		if (options.files) {
			for (const [filePath, content] of Object.entries(options.files)) {
				await filesystem.writeFile(filePath, content);
			}
		}

		// Project host directories into the VM, Docker-style. NodeFileSystem
		// reads lazily through the VFS so large trees never traverse the
		// protocol frame as a single blob.
		const hostMounts: HostDirectoryMount[] = [...(options.mounts ?? [])];

		// The `nodeModules` helper is sugar over a single host directory mount:
		// project the whole host `node_modules` at a guest `node_modules` on the
		// resolution path so any package inside resolves like real Node would.
		if (options.nodeModules !== undefined) {
			const nodeModules =
				typeof options.nodeModules === "string"
					? { hostPath: options.nodeModules }
					: options.nodeModules;
			hostMounts.push({
				guestPath: nodeModules.guestPath ?? DEFAULT_NODE_MODULES_GUEST_PATH,
				hostPath: nodeModules.hostPath,
				readOnly: true,
			});
		}

		const mounts = hostMounts.map((mount) => ({
			path: mount.guestPath,
			fs: new NodeFileSystem({ root: mount.hostPath }),
			readOnly: mount.readOnly ?? true,
		}));

		// Grant the `binding` scope when the caller registers bindings but does not
		// set their own binding policy, so the registered bindings are invocable.
		const bindingDefaults =
			options.bindings &&
			Object.keys(options.bindings).length > 0 &&
			options.permissions?.binding === undefined
				? { binding: "allow" as const }
				: {};

		const kernel = createKernel({
			filesystem,
			mounts: mounts.length > 0 ? mounts : undefined,
			// Merge the caller's policy over the secure default so partial
			// opt-ins work: `{ network: "allow" }` enables the network while the
			// execution essentials (fs/childProcess/process/env) stay granted.
			permissions: {
				...DEFAULT_PERMISSIONS,
				...bindingDefaults,
				...options.permissions,
			},
			env: options.env,
			cwd: options.cwd,
			user: options.user,
			sidecar: options.sidecar,
			onBootTiming: (timing) => options.onBootTiming?.(timing),
			loopbackExemptPorts: options.loopbackExemptPorts,
			jsRuntime: options.jsRuntime,
		});

		try {
			// The shell runtime provides `sh` plus coreutils; the Node runtime
			// provides the real V8-backed `node`. `sh` is REQUIRED to spawn any
			// process: the kernel runs every command through a shell, so without
			// `sh` nothing can be spawned, including the guest `node` program we
			// run here and any child the guest spawns via node:child_process.
			await measureBootTiming("runtime_mount_wasm", options.onBootTiming, () =>
				kernel.mount(
					createWasmVmRuntime({
						commandDirs: [commandsDir, ...(options.wasmCommandDirs ?? [])],
					}),
				),
			);
			await measureBootTiming("runtime_mount_node", options.onBootTiming, () =>
				kernel.mount(createNodeRuntime()),
			);

			// Register bindings after the runtimes are mounted so they are
			// installed as guest commands the moment the VM is ready.
			const bindings = options.bindings;
			if (bindings && Object.keys(bindings).length > 0) {
				await measureBootTiming("bindings", options.onBootTiming, () =>
					kernel.registerBindings(bindings),
				);
			}
		} catch (error) {
			await kernel.dispose().catch(() => {});
			throw error;
		}

		return new NodeRuntime(kernel);
	}

	async createResidentRunner(
		_options: NodeRuntimeResidentRunnerOptions = {},
	): Promise<NodeRuntimeResidentRunner> {
		return ResidentNodeRunner.create(this);
	}

	/**
	 * Run `code` as a guest Node program and capture its output.
	 *
	 * The source is written to an ES module inside the VM and executed with
	 * `node <file>`; it runs as standard ESM (top-level `await`, `import`).
	 */
	async exec(
		code: string,
		options: NodeRuntimeExecOptions = {},
	): Promise<NodeRuntimeExecResult> {
		const programPath = `/tmp/agentos-program-${nextProgramId++}.mjs`;
		await this.kernel.writeFile(programPath, withBindingPreamble(code));
		return this.runProgram(programPath, options);
	}

	/**
	 * Run an already-written guest program file to completion and capture its
	 * output, honoring a caller-supplied `signal` for cancellation.
	 *
	 * Without a `signal`, this runs the program through the shell (`node <file>`)
	 * exactly as before. With a `signal`, it starts the program as a guest
	 * process so the run can be cancelled: when the signal aborts, the process is
	 * killed inside the VM (the kernel delivers `SIGTERM`) and the call rejects
	 * with the signal's abort reason.
	 */
	private async runProgram(
		programPath: string,
		options: NodeRuntimeExecOptions,
	): Promise<NodeRuntimeExecResult> {
		const signal = options.signal;
		if (!signal) {
			const result = await this.kernel.exec(`node ${programPath}`, {
				env: options.env,
				cwd: options.cwd,
				stdin: options.stdin,
				timeout: options.timeout,
				onStdout: options.onStdout,
				onStderr: options.onStderr,
			});
			return toExecResult(result);
		}

		if (signal.aborted) {
			throw toAbortError(signal);
		}

		// A signal was supplied, so run the program as a guest process we can
		// kill: aborting the signal maps to a kernel kill of the underlying
		// process. Aggregate the streamed output ourselves to reproduce the
		// run-to-completion result that the shell path returns.
		const stdoutChunks: Uint8Array[] = [];
		const stderrChunks: Uint8Array[] = [];
		const proc = this.kernel.spawn("node", [programPath], {
			env: options.env,
			cwd: options.cwd,
			onStdout: (chunk) => {
				stdoutChunks.push(chunk);
				options.onStdout?.(chunk);
			},
			onStderr: (chunk) => {
				stderrChunks.push(chunk);
				options.onStderr?.(chunk);
			},
			streamStdin: options.stdin !== undefined,
		});

		if (options.stdin !== undefined) {
			proc.writeStdin(options.stdin);
			proc.closeStdin();
		}

		const onAbort = () => {
			// Deliver SIGTERM to cancel the in-flight run inside the VM.
			proc.kill(toSignalNumber("SIGTERM"));
		};
		signal.addEventListener("abort", onAbort, { once: true });

		let timer: ReturnType<typeof setTimeout> | undefined;
		if (options.timeout !== undefined) {
			timer = setTimeout(() => {
				proc.kill(toSignalNumber("SIGKILL"));
			}, options.timeout);
		}

		try {
			const exitCode = await proc.wait();
			if (signal.aborted) {
				throw toAbortError(signal);
			}
			return {
				stdout: decodeChunks(stdoutChunks),
				stderr: decodeChunks(stderrChunks),
				exitCode,
			};
		} finally {
			if (timer !== undefined) {
				clearTimeout(timer);
			}
			signal.removeEventListener("abort", onAbort);
		}
	}

	/**
	 * Start `code` as a long-running guest Node program and return a live handle
	 * to it, without waiting for it to finish.
	 *
	 * The source is written to an ES module inside the VM and started with
	 * `node <file>`; it runs as standard ESM (top-level `await`, `import`). The
	 * returned {@link NodeRuntimeProcess} lets you stream output, write to stdin,
	 * signal or kill the process, and await its exit. Pass `onStdout`/`onStderr`
	 * to receive output chunks as they are produced.
	 *
	 * Use this for guests that do not run to completion, such as a dev server you
	 * later drive with {@link NodeRuntime.fetch}:
	 *
	 * ```ts
	 * const server = await rt.spawn(`
	 *   import http from "node:http";
	 *   http.createServer((_, res) => res.end("ok")).listen(3000);
	 * `);
	 * const res = await rt.fetch(3000, { path: "/" });
	 * server.kill();
	 * await server.wait();
	 * ```
	 */
	async spawn(
		code: string,
		options: NodeRuntimeSpawnOptions = {},
	): Promise<NodeRuntimeProcess> {
		const programPath = `/tmp/agentos-program-${nextProgramId++}.mjs`;
		await this.kernel.writeFile(programPath, withBindingPreamble(code));
		const proc = this.kernel.spawn("node", [programPath], {
			env: options.env,
			cwd: options.cwd,
			onStdout: options.onStdout,
			onStderr: options.onStderr,
			// Keep stdin open so callers can stream input via writeStdin and signal
			// end-of-input with closeStdin.
			streamStdin: true,
		});
		return {
			pid: proc.pid,
			writeStdin(data) {
				proc.writeStdin(data);
			},
			closeStdin() {
				proc.closeStdin();
			},
			kill(signal) {
				proc.kill(toSignalNumber(signal));
			},
			wait() {
				return proc.wait();
			},
			get exitCode() {
				return proc.exitCode;
			},
		};
	}

	/**
	 * Start an arbitrary guest command and return a live handle. This is the
	 * command-level companion to {@link spawn}, used by benchmark harnesses that
	 * need to measure kernel process spawning directly instead of running a source
	 * string through the ergonomic Node wrapper.
	 */
	spawnCommand(
		command: string,
		args: string[] = [],
		options: NodeRuntimeSpawnOptions = {},
	): NodeRuntimeProcess {
		const proc = this.kernel.spawn(command, args, {
			env: options.env,
			cwd: options.cwd,
			onStdout: options.onStdout,
			onStderr: options.onStderr,
			streamStdin: true,
		});
		return {
			pid: proc.pid,
			writeStdin(data) {
				proc.writeStdin(data);
			},
			closeStdin() {
				proc.closeStdin();
			},
			kill(signal) {
				proc.kill(toSignalNumber(signal));
			},
			wait() {
				return proc.wait();
			},
			get exitCode() {
				return proc.exitCode;
			},
		};
	}

	/**
	 * Run an arbitrary guest command to completion and capture stdout/stderr.
	 * Unlike {@link exec}, this does not write a JavaScript source file first.
	 */
	async execCommand(
		command: string,
		args: string[] = [],
		options: NodeRuntimeExecOptions = {},
	): Promise<NodeRuntimeExecResult> {
		const stdoutChunks: Uint8Array[] = [];
		const stderrChunks: Uint8Array[] = [];
		const proc = this.spawnCommand(command, args, {
			env: options.env,
			cwd: options.cwd,
			onStdout: (chunk) => {
				stdoutChunks.push(chunk);
				options.onStdout?.(chunk);
			},
			onStderr: (chunk) => {
				stderrChunks.push(chunk);
				options.onStderr?.(chunk);
			},
		});
		if (options.stdin !== undefined) {
			proc.writeStdin(options.stdin);
		}
		proc.closeStdin();

		let timer: ReturnType<typeof setTimeout> | undefined;
		if (options.timeout !== undefined) {
			timer = setTimeout(() => proc.kill("SIGKILL"), options.timeout);
		}
		try {
			const exitCode = await proc.wait();
			return {
				stdout: decodeChunks(stdoutChunks),
				stderr: decodeChunks(stderrChunks),
				exitCode,
			};
		} finally {
			if (timer !== undefined) {
				clearTimeout(timer);
			}
		}
	}

	/**
	 * Run `code` and return the JSON-serializable value it produces.
	 *
	 * The guest exposes a `__return(value)` function; call it with a
	 * JSON-serializable value and that value is decoded on the host as
	 * `result.value`. If `__return` is never called the value is `undefined`.
	 * stdout/stderr/exitCode are still captured.
	 */
	async run<T = unknown>(
		code: string,
		options: NodeRuntimeExecOptions = {},
	): Promise<NodeRuntimeRunResult<T>> {
		const id = nextProgramId++;
		const resultPath = `/tmp/agentos-result-${id}.json`;
		const programPath = `/tmp/agentos-program-${id}.mjs`;
		// Inject the __return helper as a module-level preamble, then the user
		// code at module top level. Import declarations (preamble's and the
		// user's) are hoisted, so __return is defined before the user's
		// executable code runs — and the user keeps full ESM semantics
		// (top-level `import` and top-level `await` both work). Do NOT wrap the
		// user code in an IIFE: that would push their top-level `import`
		// statements inside a function and make them a SyntaxError.
		const wrapped = [
			`import { writeFileSync as __writeFileSync } from "node:fs";`,
			BINDING_PREAMBLE,
			`globalThis.__return = (value) => {`,
			`  __writeFileSync(${JSON.stringify(resultPath)}, JSON.stringify(value === undefined ? null : value));`,
			`};`,
			code,
		].join("\n");
		await this.kernel.writeFile(programPath, wrapped);
		const exec = await this.runProgram(programPath, options);

		let value: T | undefined;
		if (exec.exitCode === 0) {
			try {
				const bytes = await this.kernel.readFile(resultPath);
				value = JSON.parse(new TextDecoder().decode(bytes)) as T;
			} catch {
				// No __return() call (or unreadable result): leave value undefined.
			}
		}

		return { ...exec, value };
	}

	/**
	 * Drive an HTTP request to a guest HTTP server listening inside the VM and
	 * read the response back on the host.
	 *
	 * Point this at a port a guest program is serving, for example a dev server
	 * started with {@link NodeRuntime.exec}. The
	 * request and response never leave the VM: the connection is made to the
	 * guest's loopback listener through the kernel socket table, so this works
	 * even when guest network egress is denied.
	 *
	 * ```ts
	 * const res = await rt.fetch(3000, { path: "/health" });
	 * console.log(res.status, res.body);
	 * ```
	 */
	async fetch(
		port: number,
		input: NodeRuntimeFetchInput,
	): Promise<NodeRuntimeFetchResponse> {
		const body =
			input.body === undefined
				? undefined
				: typeof input.body === "string"
					? input.body
					: new TextDecoder().decode(input.body);
		const responseJson = await this.kernel.vmFetch({
			port,
			method: input.method ?? "GET",
			path: input.path,
			headersJson: JSON.stringify(input.headers ?? {}),
			body,
		});
		return parseFetchResponse(responseJson);
	}

	/**
	 * Look up a guest TCP listener once and return it, or `null` when nothing is
	 * listening yet.
	 *
	 * This is the immediate, non-blocking check behind
	 * {@link NodeRuntime.waitForListener}: it asks the kernel socket table
	 * whether a guest process is accepting connections on the requested `port`
	 * (optionally narrowed by `host`/`path`) and returns the match, or `null` if
	 * none is up. Use {@link NodeRuntime.waitForListener} when you want to block
	 * until one appears.
	 *
	 * ```ts
	 * const listener = rt.findListener({ port: 3000 });
	 * if (listener) console.log("up on pid", listener.processId);
	 * ```
	 */
	findListener(query: NodeRuntimeListenerQuery): NodeRuntimeListener | null {
		const match = this.kernel.socketTable.findListener({
			port: query.port,
			...(query.host !== undefined ? { host: query.host } : {}),
			...(query.path !== undefined ? { path: query.path } : {}),
		}) as NodeRuntimeListener | null;
		return match ?? null;
	}

	/**
	 * Block until a guest TCP listener is accepting connections on the requested
	 * `port` (optionally narrowed by `host`/`path`), then resolve with it.
	 *
	 * This is the companion to {@link NodeRuntime.spawn} and
	 * {@link NodeRuntime.fetch} for dev-server scenarios: start a server, wait
	 * until it is actually listening, then drive requests into it. The kernel
	 * socket table is polled until a matching listener appears or the wait is
	 * cut short. If `timeoutMs` elapses (default 10000) or the supplied `signal`
	 * aborts first, the returned promise rejects.
	 *
	 * ```ts
	 * const server = await rt.spawn(`
	 *   import http from "node:http";
	 *   http.createServer((_, res) => res.end("ok")).listen(3000);
	 * `);
	 * const listener = await rt.waitForListener({ port: 3000 });
	 * const res = await rt.fetch(listener.port ?? 3000, { path: "/" });
	 * server.kill();
	 * await server.wait();
	 * ```
	 */
	async waitForListener(
		query: NodeRuntimeListenerQuery,
		options: NodeRuntimeWaitForListenerOptions = {},
	): Promise<NodeRuntimeListener> {
		const timeoutMs = options.timeoutMs ?? 10_000;
		const pollIntervalMs = options.pollIntervalMs ?? 50;
		const signal = options.signal;
		const deadline = Date.now() + timeoutMs;

		for (;;) {
			if (signal?.aborted) {
				throw toAbortError(signal);
			}

			// Await a fresh lookup rather than reading the synchronous cache,
			// which starts null and would otherwise let this loop poll a stale
			// null even after the listener is up (issue #92).
			const match = (await this.kernel.socketTable.findListenerAsync({
				port: query.port,
				...(query.host !== undefined ? { host: query.host } : {}),
				...(query.path !== undefined ? { path: query.path } : {}),
			})) as NodeRuntimeListener | null;
			if (match) {
				return match;
			}

			if (Date.now() >= deadline) {
				throw new Error(
					`Timed out after ${timeoutMs}ms waiting for a listener on port ${query.port}`,
				);
			}

			await delayUntil(
				Math.min(pollIntervalMs, Math.max(0, deadline - Date.now())),
				signal,
			);
		}
	}

	/**
	 * Register host-side bindings the guest can invoke as shell commands, after
	 * the VM is already running. Each entry becomes a named guest command; when
	 * the guest runs it, the invocation round-trips back to the host and runs the
	 * binding's `handler`, whose return value is delivered back to the guest. This
	 * is the same capability as the `bindings` create option, exposed for adding
	 * bindings to a live runtime. See `bindings` on {@link NodeRuntime.create} for
	 * the invocation shape and permission behavior.
	 *
	 * When registering bindings this way, make sure the `binding` permission scope
	 * is granted (for example `permissions: { binding: "allow" }` on
	 * {@link NodeRuntime.create}) so the bindings are invocable.
	 */
	async registerBindings(
		bindings: Record<string, BindingDefinition>,
	): Promise<void> {
		await this.kernel.registerBindings(bindings);
	}

	/**
	 * Write a file into the VM's virtual filesystem, creating parent
	 * directories as needed. Use this to project assets or npm packages into
	 * the sandbox after boot; the host filesystem is never touched.
	 */
	async writeFile(
		filePath: string,
		content: string | Uint8Array,
	): Promise<void> {
		await this.kernel.writeFile(filePath, content);
	}

	/** Read a file from the VM's virtual filesystem as raw bytes. */
	async readFile(filePath: string): Promise<Uint8Array> {
		return this.kernel.readFile(filePath);
	}

	/** Read directory entry names from the VM's virtual filesystem. */
	async readDir(dirPath: string): Promise<string[]> {
		return this.kernel.readdir(dirPath);
	}

	/** Read typed directory entries from the VM's virtual filesystem. */
	async readDirWithTypes(dirPath: string): Promise<VirtualDirEntry[]> {
		return this.kernel.vfs.readDirWithTypes(dirPath);
	}

	async getResourceSnapshot(): Promise<NodeRuntimeResourceSnapshot> {
		return this.kernel.getResourceSnapshot();
	}

	/** Tear down the VM and release the sidecar. */
	async dispose(): Promise<void> {
		await this.kernel.dispose();
	}
}

const RESIDENT_RUNNER_SOURCE = `
import { Buffer } from "node:buffer";
import { createInterface } from "node:readline";

const readyPrefix = ${JSON.stringify(RESIDENT_READY_PREFIX)};
const resultPrefix = ${JSON.stringify(RESIDENT_RESULT_PREFIX)};
console.log(readyPrefix);

const rl = createInterface({ input: process.stdin, crlfDelay: Infinity });
for await (const line of rl) {
	let request;
	try {
		request = JSON.parse(line);
		const source = Buffer.from(String(request.code), "utf8").toString("base64");
		await import(\`data:text/javascript;base64,\${source}#\${request.id}\`);
		process.stdout.write(resultPrefix + JSON.stringify({
			id: request.id,
			exitCode: 0,
			stderr: "",
		}) + "\\n");
	} catch (error) {
		process.stdout.write(resultPrefix + JSON.stringify({
			id: request?.id,
			exitCode: 1,
			stderr: error instanceof Error ? (error.stack ?? error.message) : String(error),
		}) + "\\n");
	}
}
`;

class ResidentNodeRunner implements NodeRuntimeResidentRunner {
	private proc: NodeRuntimeProcess | null = null;
	private stdoutBuffer = "";
	private active: {
		id: number;
		stdout: Uint8Array[];
		stderr: Uint8Array[];
		resolve: (result: NodeRuntimeExecResult) => void;
		reject: (error: Error) => void;
		timer?: ReturnType<typeof setTimeout>;
	} | null = null;
	private readonly readyPromise: Promise<void>;
	private resolveReady!: () => void;
	private rejectReady!: (error: Error) => void;

	private constructor() {
		this.readyPromise = new Promise((resolve, reject) => {
			this.resolveReady = resolve;
			this.rejectReady = reject;
		});
	}

	static async create(runtime: NodeRuntime): Promise<ResidentNodeRunner> {
		const runner = new ResidentNodeRunner();
		runner.proc = await runtime.spawn(RESIDENT_RUNNER_SOURCE, {
			onStdout: (chunk) => runner.handleStdout(chunk),
			onStderr: (chunk) => runner.handleStderr(chunk),
		});
		runner.proc.wait().then(
			(exitCode) => {
				const error = new Error(
					`resident runner exited before completing request: ${exitCode}`,
				);
				runner.rejectReady(error);
				runner.active?.reject(error);
				runner.active = null;
			},
			(error) => {
				const normalized =
					error instanceof Error ? error : new Error(String(error));
				runner.rejectReady(normalized);
				runner.active?.reject(normalized);
				runner.active = null;
			},
		);
		await runner.readyPromise;
		return runner;
	}

	async exec(
		code: string,
		options: NodeRuntimeResidentRunnerExecOptions = {},
	): Promise<NodeRuntimeExecResult> {
		await this.readyPromise;
		if (!this.proc) {
			throw new Error("resident runner is not running");
		}
		if (this.active) {
			throw new Error("resident runner supports one in-flight exec");
		}
		const proc = this.proc;
		const id = nextResidentRequestId++;
		return new Promise((resolve, reject) => {
			const active = {
				id,
				stdout: [],
				stderr: [],
				resolve,
				reject,
				timer: undefined as ReturnType<typeof setTimeout> | undefined,
			};
			if (options.timeout !== undefined) {
				active.timer = setTimeout(() => {
					proc.kill("SIGKILL");
					this.active = null;
					reject(
						new Error(`resident runner timed out after ${options.timeout}ms`),
					);
				}, options.timeout);
			}
			this.active = active;
			proc.writeStdin(`${JSON.stringify({ id, code })}\n`);
		});
	}

	async dispose(): Promise<void> {
		const proc = this.proc;
		this.proc = null;
		this.active = null;
		if (!proc) {
			return;
		}
		proc.kill("SIGTERM");
		await proc.wait().catch(() => {});
	}

	private handleStdout(chunk: Uint8Array): void {
		this.stdoutBuffer += new TextDecoder().decode(chunk);
		while (true) {
			const newlineIndex = this.stdoutBuffer.indexOf("\n");
			if (newlineIndex < 0) {
				break;
			}
			const rawLine = this.stdoutBuffer.slice(0, newlineIndex);
			this.stdoutBuffer = this.stdoutBuffer.slice(newlineIndex + 1);
			const line = rawLine.endsWith("\r") ? rawLine.slice(0, -1) : rawLine;
			if (line === RESIDENT_READY_PREFIX) {
				this.resolveReady();
				continue;
			}
			if (line.startsWith(RESIDENT_RESULT_PREFIX)) {
				this.finishRequest(line.slice(RESIDENT_RESULT_PREFIX.length));
				continue;
			}
			this.active?.stdout.push(new TextEncoder().encode(`${line}\n`));
		}
	}

	private handleStderr(chunk: Uint8Array): void {
		this.active?.stderr.push(chunk);
	}

	private finishRequest(payload: string): void {
		const active = this.active;
		if (!active) {
			return;
		}
		let parsed: { id?: number; exitCode?: number; stderr?: string };
		try {
			parsed = JSON.parse(payload) as {
				id?: number;
				exitCode?: number;
				stderr?: string;
			};
		} catch (error) {
			active.reject(error instanceof Error ? error : new Error(String(error)));
			this.active = null;
			return;
		}
		if (parsed.id !== active.id) {
			active.reject(
				new Error(`resident runner response id mismatch: ${parsed.id}`),
			);
			this.active = null;
			return;
		}
		if (active.timer !== undefined) {
			clearTimeout(active.timer);
		}
		this.active = null;
		const stderr = `${decodeChunks(active.stderr)}${parsed.stderr ?? ""}`;
		active.resolve({
			stdout: decodeChunks(active.stdout),
			stderr,
			exitCode: parsed.exitCode ?? 1,
		});
	}
}

async function measureBootTiming<T>(
	phase: NodeRuntimeBootTimingPhase,
	onBootTiming: ((timing: NodeRuntimeBootTiming) => void) | undefined,
	fn: () => Promise<T>,
): Promise<T> {
	const start = performance.now();
	try {
		return await fn();
	} finally {
		onBootTiming?.({ phase, durationMs: performance.now() - start });
	}
}

/**
 * Common POSIX signal numbers, used to translate a signal name passed to
 * {@link NodeRuntimeProcess.kill} into the numeric signal the kernel expects.
 */
const SIGNAL_NUMBERS: Record<string, number> = {
	SIGHUP: 1,
	SIGINT: 2,
	SIGQUIT: 3,
	SIGKILL: 9,
	SIGUSR1: 10,
	SIGUSR2: 12,
	SIGTERM: 15,
	SIGSTOP: 19,
	SIGCONT: 18,
};

/**
 * Normalize a signal passed to {@link NodeRuntimeProcess.kill} into the numeric
 * signal the kernel expects. Accepts a signal name (e.g. `"SIGKILL"`) or a raw
 * number; defaults to `SIGTERM` when omitted.
 */
function toSignalNumber(signal?: NodeJS.Signals | number): number {
	if (signal === undefined) {
		return SIGNAL_NUMBERS.SIGTERM;
	}
	if (typeof signal === "number") {
		return signal;
	}
	const resolved = SIGNAL_NUMBERS[signal];
	if (resolved === undefined) {
		throw new Error(`Unknown signal: ${signal}`);
	}
	return resolved;
}

/**
 * Build the error a {@link NodeRuntime.waitForListener} wait rejects with when
 * its abort signal fires, preferring the signal's own `reason` when present.
 */
function toAbortError(signal: AbortSignal): Error {
	const reason = (signal as { reason?: unknown }).reason;
	if (reason instanceof Error) {
		return reason;
	}
	const error = new Error("The listener wait was aborted");
	error.name = "AbortError";
	return error;
}

/**
 * Resolve after `ms` milliseconds, or reject early if `signal` aborts. Used to
 * pace the polling loop in {@link NodeRuntime.waitForListener} without blocking
 * past an abort.
 */
function delayUntil(ms: number, signal?: AbortSignal): Promise<void> {
	return new Promise((resolve, reject) => {
		if (signal?.aborted) {
			reject(toAbortError(signal));
			return;
		}
		const timer = setTimeout(() => {
			signal?.removeEventListener("abort", onAbort);
			resolve();
		}, ms);
		const onAbort = () => {
			clearTimeout(timer);
			reject(toAbortError(signal as AbortSignal));
		};
		signal?.addEventListener("abort", onAbort, { once: true });
	});
}

/**
 * Concatenate streamed stdout/stderr chunks and decode them as UTF-8 text,
 * reproducing the aggregated `stdout`/`stderr` strings the shell-backed
 * {@link NodeRuntime.exec} path returns when a run is driven as a process for
 * cancellation support.
 */
function decodeChunks(chunks: Uint8Array[]): string {
	if (chunks.length === 0) {
		return "";
	}
	let total = 0;
	for (const chunk of chunks) {
		total += chunk.length;
	}
	const merged = new Uint8Array(total);
	let offset = 0;
	for (const chunk of chunks) {
		merged.set(chunk, offset);
		offset += chunk.length;
	}
	return new TextDecoder().decode(merged);
}

function toExecResult(result: ExecResult): NodeRuntimeExecResult {
	return {
		stdout: result.stdout,
		stderr: result.stderr,
		exitCode: result.exitCode,
	};
}

/**
 * Decode the raw JSON the kernel returns for a VM HTTP request into a
 * structured response. The wire shape carries `status`, an optional
 * `statusText`, `headers` (either an array of `[name, value]` pairs or an
 * object), and a `body` that is base64-encoded when `bodyEncoding` is
 * `"base64"`.
 */
function parseFetchResponse(responseJson: string): NodeRuntimeFetchResponse {
	const parsed = JSON.parse(responseJson) as {
		status?: number;
		statusText?: string;
		headers?: Array<[string, string]> | Record<string, string>;
		body?: string;
		bodyEncoding?: string;
	};

	const headers: Record<string, string> = {};
	if (Array.isArray(parsed.headers)) {
		for (const [name, value] of parsed.headers) {
			headers[name.toLowerCase()] = value;
		}
	} else if (parsed.headers) {
		for (const [name, value] of Object.entries(parsed.headers)) {
			headers[name.toLowerCase()] = value;
		}
	}

	let body = parsed.body ?? "";
	if (parsed.bodyEncoding === "base64" && body.length > 0) {
		body = new TextDecoder().decode(
			Uint8Array.from(globalThis.atob(body), (char) => char.charCodeAt(0)),
		);
	}

	return {
		status: parsed.status ?? 0,
		statusText: parsed.statusText ?? "",
		headers,
		body,
	};
}
