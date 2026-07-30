import { base64ToBytes, bytesToBase64, toUint8Array } from "./encoding.js";

export type BrowserChildProcessBytes = {
	__agentOSType: "bytes";
	base64: string;
};

export type BrowserChildProcessSpawnOptions = {
	argv0?: string;
	cwd?: string;
	env?: Record<string, string>;
	input?: BrowserChildProcessBytes | string | Uint8Array;
};

export type BrowserChildProcessSpawnRequest = {
	command: string;
	args: string[];
	options?: BrowserChildProcessSpawnOptions;
};

export type BrowserChildProcessPollEvent =
	| { type: "stdout" | "stderr"; data: BrowserChildProcessBytes }
	| {
			type: "exit";
			exitCode: number | null;
			signal: number | string | null;
	  };

export function encodeChildProcessBytes(
	data: Uint8Array,
): BrowserChildProcessBytes {
	return {
		__agentOSType: "bytes",
		base64: bytesToBase64(data),
	};
}

export function decodeChildProcessInput(
	value: unknown,
): Uint8Array | undefined {
	if (value == null) {
		return undefined;
	}
	if (
		typeof value === "object" &&
		(value as { __agentOSType?: unknown }).__agentOSType === "bytes" &&
		typeof (value as { base64?: unknown }).base64 === "string"
	) {
		return base64ToBytes((value as { base64: string }).base64);
	}
	if (typeof value === "string") {
		return new TextEncoder().encode(value);
	}
	return toUint8Array(value);
}

export function parseChildProcessSpawnRequest(
	value: unknown,
	label: string,
): BrowserChildProcessSpawnRequest {
	const parsed = typeof value === "string" ? JSON.parse(value) : value;
	if (!parsed || typeof parsed !== "object") {
		throw new Error(`${label} must be an object`);
	}
	const record = parsed as Record<string, unknown>;
	if (typeof record.command !== "string") {
		throw new Error(`${label}.command must be a string`);
	}
	if (!Array.isArray(record.args)) {
		throw new Error(`${label}.args must be an array`);
	}
	const rawOptions = record.options;
	const optionsRecord =
		rawOptions && typeof rawOptions === "object"
			? (rawOptions as Record<string, unknown>)
			: {};
	const env =
		optionsRecord.env && typeof optionsRecord.env === "object"
			? Object.fromEntries(
					Object.entries(optionsRecord.env as Record<string, unknown>).map(
						([key, envValue]) => [key, String(envValue)],
					),
				)
			: undefined;
	return {
		command: record.command,
		args: record.args.map((entry) => String(entry)),
		options: {
			argv0:
				typeof optionsRecord.argv0 === "string"
					? optionsRecord.argv0
					: undefined,
			cwd:
				typeof optionsRecord.cwd === "string" ? optionsRecord.cwd : undefined,
			env,
			input: optionsRecord.input as BrowserChildProcessSpawnOptions["input"],
		},
	};
}
