import { Buffer } from "node:buffer";

const DEFAULT_MAX_PENDING_OUTPUT_BYTES = 8 * 1024 * 1024;
const OUTPUT_LIMIT_WARNING_RATIO = 0.8;
const TRAILING_OUTPUT_GRACE_MS = 250;

export interface NodeShellOpenOptions {
	command?: string;
	args?: string[];
	env?: Record<string, string>;
	cwd?: string;
	cols?: number;
	rows?: number;
}

export interface NodeShellConnection {
	openShell(options?: NodeShellOpenOptions): Promise<{ shellId: string }>;
	writeShell(shellId: string, data: string | Uint8Array): Promise<void>;
	resizeShell(shellId: string, cols: number, rows: number): Promise<void>;
	closeShell(shellId: string): Promise<void>;
	waitShell(shellId: string): Promise<number>;
}

export interface NodeShellInput {
	isTTY?: boolean;
	isRaw?: boolean;
	setRawMode?(enabled: boolean): unknown;
	on(event: "data", listener: (data: string | Uint8Array) => void): unknown;
	on(event: "end", listener: () => void): unknown;
	on(event: "error", listener: (error: Error) => void): unknown;
	removeListener(
		event: "data",
		listener: (data: string | Uint8Array) => void,
	): unknown;
	removeListener(event: "end", listener: () => void): unknown;
	removeListener(event: "error", listener: (error: Error) => void): unknown;
	pause(): unknown;
	resume(): unknown;
}

export interface NodeShellOutput {
	isTTY?: boolean;
	columns?: number;
	rows?: number;
	write(data: string | Uint8Array): boolean;
	on(event: "resize", listener: () => void): unknown;
	removeListener(event: "resize", listener: () => void): unknown;
}

export type NodeShellSignal = "SIGINT" | "SIGHUP" | "SIGTERM";

export interface AttachShellOptions extends NodeShellOpenOptions {
	stdin?: NodeShellInput;
	stdout?: NodeShellOutput;
	stderr?: NodeShellOutput;
	rawMode?: boolean;
	resize?: boolean;
	signals?: readonly NodeShellSignal[];
	maxPendingOutputBytes?: number;
	onWarning?: (message: string) => void;
}

interface ShellDataPayload {
	shellId: string;
	data: unknown;
}

interface ShellExitPayload {
	shellId: string;
	exitCode: number;
}

interface ShellEventSource {
	on(event: string, listener: (payload: never) => void): unknown;
	off?(event: string, listener: (payload: never) => void): unknown;
	removeListener?(event: string, listener: (payload: never) => void): unknown;
}

type EarlyOutput = {
	stream: "stdout" | "stderr";
	payload: ShellDataPayload;
	bytes: Uint8Array;
};

type AttachOutcome =
	| { kind: "exit"; exitCode: number }
	| { kind: "failure"; error: Error }
	| { kind: "signal"; signal: NodeShellSignal; exitCode: number };

export class NodeShellOutputLimitError extends Error {
	readonly code = "AGENTOS_NODE_SHELL_OUTPUT_LIMIT";

	constructor(
		readonly limitBytes: number,
		readonly pendingBytes: number,
	) {
		super(
			`AgentOS Node shell pending output exceeded ${limitBytes} bytes ` +
				`(${pendingBytes} bytes pending); raise AttachShellOptions.maxPendingOutputBytes to allow more`,
		);
		this.name = "NodeShellOutputLimitError";
	}
}

export class NodeShellCleanupError extends Error {
	readonly code = "AGENTOS_NODE_SHELL_CLEANUP_FAILED";

	constructor(readonly errors: readonly Error[]) {
		super("AgentOS Node shell and cleanup both failed");
		this.name = "NodeShellCleanupError";
	}
}

/**
 * Open an AgentOS PTY shell and attach it to the current Node process terminal.
 * The connection must expose AgentOS shell actions and its broadcast `on()` API.
 */
export async function attachShell(
	connection: NodeShellConnection,
	options: AttachShellOptions = {},
): Promise<number> {
	const stdin = options.stdin ?? process.stdin;
	const stdout = options.stdout ?? process.stdout;
	const stderr = options.stderr ?? process.stderr;
	const events = connection as unknown as ShellEventSource;
	if (typeof events.on !== "function") {
		throw new TypeError("attachShell requires an AgentOS connection with on()");
	}

	const maxPendingOutputBytes =
		options.maxPendingOutputBytes ?? DEFAULT_MAX_PENDING_OUTPUT_BYTES;
	if (
		!Number.isSafeInteger(maxPendingOutputBytes) ||
		maxPendingOutputBytes <= 0
	) {
		throw new RangeError(
			"AttachShellOptions.maxPendingOutputBytes must be a positive safe integer",
		);
	}

	const warn =
		options.onWarning ?? ((message: string) => stderr.write(`${message}\n`));
	let shellId: string | undefined;
	let shellClosed = false;
	let failure: Error | undefined;
	let resolveFailure: ((outcome: AttachOutcome) => void) | undefined;
	const failurePromise = new Promise<AttachOutcome>((resolve) => {
		resolveFailure = resolve;
	});
	const fail = (error: Error) => {
		if (failure) return;
		failure = error;
		resolveFailure?.({ kind: "failure", error });
	};
	const earlyOutput: EarlyOutput[] = [];
	let earlyOutputBytes = 0;
	let earlyOutputWarned = false;
	let resolveShellExit: (() => void) | undefined;
	const shellExitPromise = new Promise<void>((resolve) => {
		resolveShellExit = resolve;
	});

	const onShellData = (payload: ShellDataPayload) => {
		routeOutput("stdout", payload);
	};
	const onShellStderr = (payload: ShellDataPayload) => {
		routeOutput("stderr", payload);
	};
	const onShellExit = (payload: ShellExitPayload) => {
		if (shellId && payload.shellId === shellId) resolveShellExit?.();
	};
	const unsubscribers = [
		subscribe(events, "shellData", onShellData),
		subscribe(events, "shellStderr", onShellStderr),
		subscribe(events, "shellExit", onShellExit),
	];

	function routeOutput(
		stream: "stdout" | "stderr",
		payload: ShellDataPayload,
	): void {
		try {
			const bytes = toBytes(payload.data);
			if (bytes.byteLength === 0) return;
			if (!shellId) {
				const nextBytes = earlyOutputBytes + bytes.byteLength;
				if (nextBytes > maxPendingOutputBytes) {
					fail(new NodeShellOutputLimitError(maxPendingOutputBytes, nextBytes));
					return;
				}
				earlyOutputBytes = nextBytes;
				earlyOutput.push({ stream, payload, bytes });
				if (
					!earlyOutputWarned &&
					nextBytes >=
						Math.floor(maxPendingOutputBytes * OUTPUT_LIMIT_WARNING_RATIO)
				) {
					earlyOutputWarned = true;
					warn(
						`AgentOS Node shell pending output is near its ${maxPendingOutputBytes}-byte limit ` +
							`(${nextBytes} bytes pending)`,
					);
				}
				return;
			}
			if (payload.shellId !== shellId) return;
			(stream === "stdout" ? stdout : stderr).write(bytes);
		} catch (error) {
			fail(asError(error));
		}
	}

	let inputAttached = false;
	let rawModeChanged = false;
	let previousRawMode = false;
	let inputWrite = Promise.resolve();
	let inputEnded = false;
	let resizeAttached = false;
	let signalOutcomeResolve: ((outcome: AttachOutcome) => void) | undefined;
	const signalPromise = new Promise<AttachOutcome>((resolve) => {
		signalOutcomeResolve = resolve;
	});
	const signalHandlers = new Map<NodeShellSignal, () => void>();

	const onInputData = (data: string | Uint8Array) => {
		stdin.pause();
		inputWrite = inputWrite
			.then(async () => {
				if (shellId) await connection.writeShell(shellId, data);
			})
			.then(() => {
				if (!inputEnded) stdin.resume();
			})
			.catch((error) => fail(asError(error)));
	};
	const onInputEnd = () => {
		inputEnded = true;
		stdin.pause();
		inputWrite = inputWrite
			.then(async () => {
				if (shellId) await connection.writeShell(shellId, "\u0004");
			})
			.catch((error) => fail(asError(error)));
	};
	const onInputError = (error: Error) => fail(error);
	const onResize = () => {
		if (!shellId) return;
		const cols = stdout.columns;
		const rows = stdout.rows;
		if (!positiveInteger(cols) || !positiveInteger(rows)) return;
		void connection.resizeShell(shellId, cols, rows).catch((error) => {
			fail(asError(error));
		});
	};

	try {
		const opened = await connection.openShell({
			command: options.command,
			args: options.args,
			env: options.env,
			cwd: options.cwd,
			cols: options.cols ?? stdout.columns,
			rows: options.rows ?? stdout.rows,
		});
		shellId = opened.shellId;
		for (const item of earlyOutput) {
			if (item.payload.shellId !== shellId) continue;
			(item.stream === "stdout" ? stdout : stderr).write(item.bytes);
		}
		earlyOutput.length = 0;
		earlyOutputBytes = 0;
		if (failure) throw failure;

		previousRawMode = Boolean(stdin.isRaw);
		if (
			(options.rawMode ?? true) &&
			stdin.isTTY &&
			typeof stdin.setRawMode === "function" &&
			!previousRawMode
		) {
			stdin.setRawMode(true);
			rawModeChanged = true;
		}
		stdin.on("data", onInputData);
		stdin.on("end", onInputEnd);
		stdin.on("error", onInputError);
		stdin.resume();
		inputAttached = true;

		if ((options.resize ?? true) && stdout.isTTY) {
			stdout.on("resize", onResize);
			resizeAttached = true;
			onResize();
		}

		for (const signal of options.signals ?? ["SIGINT", "SIGHUP", "SIGTERM"]) {
			const handler = () => {
				signalOutcomeResolve?.({
					kind: "signal",
					signal,
					exitCode: signalExitCode(signal),
				});
			};
			signalHandlers.set(signal, handler);
			process.once(signal, handler);
		}

		const outcome = await Promise.race<AttachOutcome>([
			connection.waitShell(shellId).then((exitCode) => ({
				kind: "exit" as const,
				exitCode,
			})),
			failurePromise,
			signalPromise,
		]);
		if (outcome.kind === "failure") {
			await connection.closeShell(shellId);
			shellClosed = true;
			throw outcome.error;
		}
		if (outcome.kind === "signal") {
			await connection.closeShell(shellId);
			shellClosed = true;
			return outcome.exitCode;
		}

		await Promise.race([
			shellExitPromise,
			new Promise<void>((resolve) =>
				setTimeout(resolve, TRAILING_OUTPUT_GRACE_MS),
			),
		]);
		await inputWrite;
		if (failure) throw failure;
		return outcome.exitCode;
	} catch (error) {
		if (shellId && !shellClosed) {
			try {
				await connection.closeShell(shellId);
				shellClosed = true;
			} catch (closeError) {
				throw new NodeShellCleanupError([asError(error), asError(closeError)]);
			}
		}
		throw error;
	} finally {
		for (const unsubscribe of unsubscribers) unsubscribe();
		if (inputAttached) {
			stdin.removeListener("data", onInputData);
			stdin.removeListener("end", onInputEnd);
			stdin.removeListener("error", onInputError);
			stdin.pause();
		}
		if (resizeAttached) stdout.removeListener("resize", onResize);
		if (rawModeChanged) stdin.setRawMode?.(previousRawMode);
		for (const [signal, handler] of signalHandlers) {
			process.removeListener(signal, handler);
		}
	}
}

function subscribe<TPayload>(
	events: ShellEventSource,
	event: string,
	listener: (payload: TPayload) => void,
): () => void {
	const untypedListener = listener as (payload: never) => void;
	const result = events.on(event, untypedListener);
	if (typeof result === "function") return result as () => void;
	return () => {
		if (events.off) events.off(event, untypedListener);
		else events.removeListener?.(event, untypedListener);
	};
}

function toBytes(data: unknown): Uint8Array {
	if (data instanceof Uint8Array) return data;
	if (
		Array.isArray(data) &&
		data.length === 2 &&
		data[0] === "$Uint8Array" &&
		typeof data[1] === "string"
	) {
		return Buffer.from(data[1], "base64");
	}
	if (typeof data === "string") return Buffer.from(data, "utf8");
	throw new TypeError(
		`Unsupported AgentOS shell data payload: ${String(data)}`,
	);
}

function positiveInteger(value: number | undefined): value is number {
	return Number.isInteger(value) && (value ?? 0) > 0;
}

function signalExitCode(signal: NodeShellSignal): number {
	switch (signal) {
		case "SIGHUP":
			return 129;
		case "SIGINT":
			return 130;
		case "SIGTERM":
			return 143;
	}
}

function asError(error: unknown): Error {
	return error instanceof Error ? error : new Error(String(error));
}
