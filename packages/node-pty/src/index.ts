import { spawn as spawnChild } from "node:child_process";
import { constants as osConstants } from "node:os";
import { StringDecoder } from "node:string_decoder";

export interface IDisposable {
	dispose(): void;
}

export interface IPtyForkOptions {
	name?: string;
	cols?: number;
	rows?: number;
	cwd?: string;
	env?: Record<string, string | undefined>;
	encoding?: BufferEncoding | null;
	handleFlowControl?: boolean;
	flowControlPause?: string;
	flowControlResume?: string;
	uid?: number;
	gid?: number;
}

export type IWindowsPtyForkOptions = IPtyForkOptions;

export interface IPty {
	readonly pid: number;
	readonly cols: number;
	readonly rows: number;
	readonly process: string;
	handleFlowControl: boolean;
	readonly onData: (listener: (data: string) => unknown) => IDisposable;
	readonly onExit: (
		listener: (event: { exitCode: number; signal?: number }) => unknown,
	) => IDisposable;
	resize(columns: number, rows: number): void;
	clear(): void;
	write(data: string | Buffer): void;
	kill(signal?: string): void;
	pause(): void;
	resume(): void;
}

type AgentOSChild = ReturnType<typeof spawnChild> & {
	resizePty?: (cols: number, rows: number) => unknown;
};

type AgentOSSpawnOptions = Parameters<typeof spawnChild>[2] & {
	agentosPty: { cols: number; rows: number };
};

const DEFAULT_COLS = 80;
const DEFAULT_ROWS = 24;
const DEFAULT_MAX_LISTENERS = 100;

function positiveInteger(
	value: number | undefined,
	fallback: number,
	label: string,
) {
	const result = value ?? fallback;
	if (!Number.isInteger(result) || result <= 0) {
		throw Object.assign(new RangeError(`${label} must be a positive integer`), {
			code: "EINVAL",
		});
	}
	return result;
}

function maxListeners() {
	const configured = Number(process.env.AGENTOS_NODE_PTY_MAX_LISTENERS);
	return Number.isInteger(configured) && configured > 0
		? configured
		: DEFAULT_MAX_LISTENERS;
}

function addBoundedListener<T>(
	listeners: Set<(event: T) => unknown>,
	listener: (event: T) => unknown,
) {
	if (listeners.size >= maxListeners()) {
		throw Object.assign(
			new Error(
				`node-pty listener limit exceeded (${maxListeners()}); raise AGENTOS_NODE_PTY_MAX_LISTENERS to allow more`,
			),
			{ code: "ERR_AGENTOS_NODE_PTY_LISTENER_LIMIT" },
		);
	}
	listeners.add(listener);
	return { dispose: () => listeners.delete(listener) };
}

function signalNumber(signal: NodeJS.Signals | null): number | undefined {
	return signal ? osConstants.signals[signal] : undefined;
}

export function spawn(
	file: string,
	args: string[] | string,
	options: IPtyForkOptions | IWindowsPtyForkOptions = {},
): IPty {
	if (typeof args === "string") {
		throw Object.assign(
			new Error(
				"pre-escaped Windows command lines are not supported by AgentOS node-pty",
			),
			{ code: "ENOSYS" },
		);
	}
	if (options.uid !== undefined || options.gid !== undefined) {
		throw Object.assign(
			new Error("node-pty uid/gid overrides are not supported by AgentOS"),
			{
				code: "ENOSYS",
			},
		);
	}

	let cols = positiveInteger(options.cols, DEFAULT_COLS, "cols");
	let rows = positiveInteger(options.rows, DEFAULT_ROWS, "rows");
	const encoding = options.encoding === undefined ? "utf8" : options.encoding;
	if (encoding === null) {
		throw Object.assign(new Error("binary node-pty output is not supported"), {
			code: "ENOSYS",
		});
	}
	const decoder = new StringDecoder(encoding);
	const dataListeners = new Set<(data: string) => unknown>();
	const exitListeners = new Set<
		(event: { exitCode: number; signal?: number }) => unknown
	>();
	let exited = false;
	let handleFlowControl = options.handleFlowControl ?? false;
	const flowControlPause = options.flowControlPause ?? "\x13";
	const flowControlResume = options.flowControlResume ?? "\x11";

	const child = spawnChild(file, args, {
		cwd: options.cwd,
		env: {
			...process.env,
			...options.env,
			TERM: options.name ?? "xterm-256color",
			COLUMNS: String(cols),
			LINES: String(rows),
		},
		stdio: ["pipe", "pipe", "pipe"],
		agentosPty: { cols, rows },
	} as AgentOSSpawnOptions) as AgentOSChild;

	const emitData = (chunk: Buffer | string) => {
		const text = typeof chunk === "string" ? chunk : decoder.write(chunk);
		if (!text) return;
		for (const listener of [...dataListeners]) listener(text);
	};
	const emitExit = (exitCode: number, signal?: number) => {
		if (exited) return;
		exited = true;
		const trailing = decoder.end();
		if (trailing) {
			for (const listener of [...dataListeners]) listener(trailing);
		}
		for (const listener of [...exitListeners]) listener({ exitCode, signal });
		dataListeners.clear();
		exitListeners.clear();
	};

	child.stdout?.on("data", emitData);
	child.stderr?.on("data", emitData);
	child.once("error", (error) => {
		console.error("AgentOS node-pty child process failed", error);
		emitExit(1);
	});
	child.once("exit", (code, signal) =>
		emitExit(code ?? 0, signalNumber(signal)),
	);

	return {
		get pid() {
			return child.pid ?? -1;
		},
		get cols() {
			return cols;
		},
		get rows() {
			return rows;
		},
		get process() {
			return file;
		},
		get handleFlowControl() {
			return handleFlowControl;
		},
		set handleFlowControl(value: boolean) {
			handleFlowControl = value;
		},
		onData(listener) {
			return addBoundedListener(dataListeners, listener);
		},
		onExit(listener) {
			return addBoundedListener(exitListeners, listener);
		},
		resize(columns, nextRows) {
			const nextCols = positiveInteger(columns, DEFAULT_COLS, "columns");
			const normalizedRows = positiveInteger(nextRows, DEFAULT_ROWS, "rows");
			if (typeof child.resizePty !== "function") {
				throw Object.assign(
					new Error("AgentOS child process PTY resize bridge is unavailable"),
					{
						code: "ENOTTY",
					},
				);
			}
			child.resizePty(nextCols, normalizedRows);
			cols = nextCols;
			rows = normalizedRows;
		},
		clear() {},
		write(data) {
			if (handleFlowControl && data === flowControlPause) {
				child.stdout?.pause();
				child.stderr?.pause();
				return;
			}
			if (handleFlowControl && data === flowControlResume) {
				child.stdout?.resume();
				child.stderr?.resume();
				return;
			}
			child.stdin?.write(data);
		},
		kill(signal = "SIGHUP") {
			child.kill(signal as NodeJS.Signals);
		},
		pause() {
			child.stdout?.pause();
			child.stderr?.pause();
		},
		resume() {
			child.stdout?.resume();
			child.stderr?.resume();
		},
	};
}
