export interface CodeExecutionSpawnOptions {
	cwd?: string;
	env?: Record<string, string>;
	stdin?: string | Uint8Array;
	streamStdin?: boolean;
	captureStdio?: boolean;
}

export interface CodeExecutionProcessOutput {
	pid: number;
	stream: "stdout" | "stderr";
	data: Uint8Array;
}

export interface CodeExecutionVm {
	spawn(
		command: string,
		args: string[],
		options?: CodeExecutionSpawnOptions,
	): { pid: number };
	onProcessOutput(
		pid: number,
		handler: (event: CodeExecutionProcessOutput) => void,
	): () => void;
	waitProcess(pid: number): Promise<number>;
	writeProcessStdin(pid: number, data: string | Uint8Array): Promise<void>;
	closeProcessStdin(pid: number): Promise<void>;
	killProcess(pid: number): void;
	stopProcess(pid: number): void;
}

export interface CodeExecutionOptions {
	cwd?: string;
	env?: Record<string, string>;
	argv?: string[];
	stdin?: string | Uint8Array;
	timeoutMs?: number;
	signal?: AbortSignal;
	onStdout?: (chunk: string) => void;
	onStderr?: (chunk: string) => void;
}

export interface CodeExecutionResult {
	success: boolean;
	exitCode: number;
	stdout: string;
	stderr: string;
}

export interface GuestError {
	name: string;
	message: string;
	stack?: string;
}

export type CodeEvaluationResult<T> =
	| {
			success: true;
			exitCode: 0;
			value: T;
			stdout: string;
			stderr: string;
	  }
	| {
			success: false;
			exitCode: number;
			error: GuestError;
			stdout: string;
			stderr: string;
	  };

export interface CodeProcess {
	readonly pid: number;
	writeStdin(data: string | Uint8Array): Promise<void>;
	closeStdin(): Promise<void>;
	kill(signal?: "SIGTERM" | "SIGKILL"): void;
	wait(): Promise<CodeExecutionResult>;
}

export interface SpawnCodeProcessOptions extends CodeExecutionOptions {
	onComplete?: () => Promise<void>;
	streamStdin?: boolean;
}

function toSpawnOptions(
	options: SpawnCodeProcessOptions,
): CodeExecutionSpawnOptions {
	return {
		cwd: options.cwd,
		env: options.env,
		stdin: options.stdin,
		streamStdin: options.streamStdin ?? true,
		captureStdio: true,
	};
}

export function spawnCodeProcess(
	vm: CodeExecutionVm,
	command: string,
	args: string[],
	options: SpawnCodeProcessOptions = {},
): CodeProcess {
	if (options.signal?.aborted) {
		throw abortReason(options.signal);
	}
	if (
		options.timeoutMs !== undefined &&
		(!Number.isFinite(options.timeoutMs) || options.timeoutMs < 0)
	) {
		throw new RangeError("timeoutMs must be a finite, non-negative number");
	}

	const { pid } = vm.spawn(command, args, toSpawnOptions(options));
	const stdoutDecoder = new TextDecoder();
	const stderrDecoder = new TextDecoder();
	let stdout = "";
	let stderr = "";
	let callbackError: unknown;
	let aborted = false;
	let finalized = false;
	let waitPromise: Promise<CodeExecutionResult> | undefined;

	const unsubscribeOutput = vm.onProcessOutput(pid, (event) => {
		try {
			if (event.stream === "stdout") {
				const text = stdoutDecoder.decode(event.data, { stream: true });
				stdout += text;
				options.onStdout?.(text);
			} else {
				const text = stderrDecoder.decode(event.data, { stream: true });
				stderr += text;
				options.onStderr?.(text);
			}
		} catch (error) {
			callbackError ??= error;
			vm.killProcess(pid);
		}
	});

	const abort = () => {
		aborted = true;
		vm.killProcess(pid);
	};
	options.signal?.addEventListener("abort", abort, { once: true });
	const timeout =
		options.timeoutMs === undefined
			? undefined
			: setTimeout(() => vm.killProcess(pid), options.timeoutMs);

	const finalize = async () => {
		if (finalized) return;
		finalized = true;
		unsubscribeOutput();
		options.signal?.removeEventListener("abort", abort);
		if (timeout !== undefined) clearTimeout(timeout);
		stdout += stdoutDecoder.decode();
		stderr += stderrDecoder.decode();
		await options.onComplete?.();
	};

	const wait = () => {
		waitPromise ??= (async () => {
			let exitCode: number;
			try {
				exitCode = await vm.waitProcess(pid);
			} catch (waitError) {
				try {
					await finalize();
				} catch (cleanupError) {
					console.error(
						"[agentos] process cleanup also failed after waitProcess rejected:",
						cleanupError,
					);
				}
				throw waitError;
			}
			await finalize();
			if (aborted && options.signal) throw abortReason(options.signal);
			if (callbackError !== undefined) throw callbackError;
			return {
				success: exitCode === 0,
				exitCode,
				stdout,
				stderr,
			};
		})();
		return waitPromise;
	};

	return {
		pid,
		writeStdin: (data) => vm.writeProcessStdin(pid, data),
		closeStdin: () => vm.closeProcessStdin(pid),
		kill: (signal) => {
			if (signal === "SIGKILL") vm.killProcess(pid);
			else vm.stopProcess(pid);
		},
		wait,
	};
}

function abortReason(signal: AbortSignal): unknown {
	return signal.reason ?? new DOMException("Execution aborted", "AbortError");
}

export async function executeArgv(
	vm: CodeExecutionVm,
	command: string,
	args: string[],
	options: SpawnCodeProcessOptions = {},
): Promise<CodeExecutionResult> {
	return spawnCodeProcess(vm, command, args, {
		...options,
		streamStdin: false,
	}).wait();
}
