#!/usr/bin/env node

import {
	type Agent,
	AgentSideConnection,
	type AuthenticateRequest,
	type AuthenticateResponse,
	type CancelNotification,
	type InitializeRequest,
	type InitializeResponse,
	type NewSessionRequest,
	type NewSessionResponse,
	type PromptRequest,
	type PromptResponse,
	type RequestPermissionResponse,
	type SessionUpdate,
	type SetSessionModeRequest,
	type SetSessionModeResponse,
	ndJsonStream,
} from "@agentclientprotocol/sdk";
import {
	type CanUseTool,
	type McpServerConfig,
	type PermissionMode,
	type PermissionResult,
	type PermissionUpdate,
	type Query,
	type SDKUserMessage,
} from "@anthropic-ai/claude-agent-sdk";
import { createHash, randomUUID } from "node:crypto";
import {
	appendFileSync,
	existsSync,
	mkdirSync,
	writeFileSync,
} from "node:fs";
import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, resolve as resolvePath } from "node:path";
import { PassThrough } from "node:stream";
import { fileURLToPath } from "node:url";
import { resolveClaudeCliPath, resolveClaudeSdkPath } from "./patched-cli.js";

type PromptPart = { type?: string; text?: string };
type ClaudeSdkRuntime = Awaited<typeof claudeSdkRuntimePromise>;
type QueryFactory = ClaudeSdkRuntime["query"];

const ACP_MODES: PermissionMode[] = [
	"default",
	"acceptEdits",
	"bypassPermissions",
	"plan",
	"dontAsk",
];

let appendSystemPrompt: string | undefined;
const argv = process.argv.slice(2);
for (let i = 0; i < argv.length; i++) {
	if (argv[i] === "--append-system-prompt" && i + 1 < argv.length) {
		appendSystemPrompt = argv[i + 1];
		i++;
	}
}

const claudeSdkRuntimePromise = loadPatchedClaudeSdkRuntime();
const traceAdapterMessages =
	process.env.CLAUDE_CODE_TRACE_ADAPTER_MESSAGES === "1";
const traceFile = process.env.CLAUDE_CODE_TRACE_FILE;

function traceAdapter(message: string): void {
	if (!traceAdapterMessages) return;
	process.stderr.write(`[agent-os-claude] ${message}\n`);
	if (traceFile) {
		appendFileSync(traceFile, `[agent-os-claude] ${message}\n`);
	}
}

async function loadPatchedClaudeSdkRuntime(): Promise<
	{
		cliPath: string;
		query: typeof import("@anthropic-ai/claude-agent-sdk").query;
	}
> {
	const require = createRequire(import.meta.url);
	const sdkPath = require.resolve("@anthropic-ai/claude-agent-sdk");
	const packageDir = resolvePath(dirname(fileURLToPath(import.meta.url)), "..");
	const cliPath = resolveClaudeCliPath({ packageDir, sdkPath });
	const runtime = await import(resolveClaudeSdkPath({ packageDir, sdkPath }));
	return {
		cliPath,
		query: runtime.query,
	};
}

function ensureClaudeCliWrapper(originalCliPath: string): string {
	const cacheDir = resolvePath(tmpdir(), "agent-os-claude-sdk");
	mkdirSync(cacheDir, { recursive: true });

const wrapperSource = `#!/usr/bin/env node
import { appendFileSync as __agentOsAppendFileSync } from "node:fs";
import { inspect } from "node:util";
import { PassThrough } from "node:stream";

const originalCliPath = ${JSON.stringify(originalCliPath)};
const swapStdio = process.env.CLAUDE_CODE_SWAP_STDIO === "1";
const traceExit = process.env.CLAUDE_CODE_TRACE_EXIT === "1";
const traceStdio = process.env.CLAUDE_CODE_TRACE_STDIO === "1";
const traceFile = process.env.CLAUDE_CODE_TRACE_FILE;
const realStdout = process.stdout;
const realStderr = process.stderr;

function wrapperTrace(message) {
	if (traceFile) {
		try {
			__agentOsAppendFileSync(traceFile, message + "\\n");
		} catch {}
	}
}

if (swapStdio) {
	Object.defineProperty(process, "stdout", {
		configurable: true,
		enumerable: true,
		value: realStderr,
	});
	Object.defineProperty(process, "stderr", {
		configurable: true,
		enumerable: true,
		value: realStdout,
	});
}

process.stderr.write(
	"[agent-os-claude] wrapper_start cli=" + originalCliPath + "\\n",
);
wrapperTrace(
	"[agent-os-claude] wrapper_start cli=" + originalCliPath,
);
process.stderr.write(
	"[agent-os-claude] wrapper_argv " +
		JSON.stringify(process.argv) +
		"\\n",
);
wrapperTrace(
	"[agent-os-claude] wrapper_argv " + JSON.stringify(process.argv),
);

process.on("unhandledRejection", (error) => {
	process.stderr.write(
		"[agent-os-claude] unhandledRejection " + inspect(error, { depth: 6 }) + "\\n",
	);
});

process.on("uncaughtException", (error) => {
	process.stderr.write(
		"[agent-os-claude] uncaughtException " + inspect(error, { depth: 6 }) + "\\n",
	);
});

function writeTrace(label, value) {
	if (!traceExit) return;
	const stack = new Error().stack ?? "";
	process.stderr.write(
		"[agent-os-claude] " +
			label +
			" " +
			inspect(value, { depth: 4, breakLength: Infinity }) +
			"\\n" +
			stack +
			"\\n",
	);
}

let exitCodeValue = process.exitCode;
if (
	process.env.CLAUDE_CODE_IGNORE_STARTUP_EXIT_CODE === "1" &&
	exitCodeValue === 0
) {
	exitCodeValue = undefined;
}
Object.defineProperty(process, "exitCode", {
	configurable: true,
	enumerable: true,
	get() {
		return exitCodeValue;
	},
	set(value) {
		writeTrace("process.exitCode =", value);
		exitCodeValue = value;
	},
});

const originalExit = process.exit.bind(process);
process.exit = (code) => {
	writeTrace("process.exit()", code);
	return originalExit(code);
};

const realStdin = process.stdin;
const bufferedStdin = new PassThrough();
bufferedStdin.isTTY = realStdin.isTTY;
bufferedStdin.fd = realStdin.fd;
if (typeof realStdin.setRawMode === "function") {
	bufferedStdin.setRawMode = realStdin.setRawMode.bind(realStdin);
}
realStdin.on("data", (chunk) => {
	bufferedStdin.write(chunk);
});
realStdin.on("end", () => {
	bufferedStdin.end();
});
realStdin.on("error", (error) => {
	bufferedStdin.destroy(error);
});
realStdin.resume();
Object.defineProperty(process, "stdin", {
	configurable: true,
	enumerable: true,
	value: bufferedStdin,
});

for (const stream of [process.stdout, process.stderr]) {
	if (typeof stream.destroy !== "function") {
		stream.destroy = (error) => {
			if (error) {
				stream.emit("error", error);
				return stream;
			}
			if (typeof stream.end === "function") {
				stream.end();
			}
			return stream;
		};
	}
}

if (traceStdio) {
	realStdin.on("data", (chunk) => {
		process.stderr.write(
			"[agent-os-claude] stdin_chunk " +
				JSON.stringify(String(chunk)).slice(0, 4000) +
				"\\n",
		);
	});

	const originalStdoutWrite = process.stdout.write.bind(process.stdout);
	process.stdout.write = function (chunk, encoding, callback) {
		process.stderr.write(
			"[agent-os-claude] stdout_chunk " +
				JSON.stringify(
					typeof chunk === "string"
						? chunk
						: Buffer.from(chunk).toString(
								typeof encoding === "string" ? encoding : undefined,
							),
				).slice(0, 4000) +
				"\\n",
		);
		return originalStdoutWrite(chunk, encoding, callback);
	};
}

await import(originalCliPath);
`;

	const wrapperHash = createHash("sha256")
		.update(originalCliPath)
		.update(wrapperSource)
		.digest("hex")
		.slice(0, 16);
	const wrapperPath = resolvePath(cacheDir, `cli-wrapper-${wrapperHash}.mjs`);
	if (!existsSync(wrapperPath)) {
		writeFileSync(wrapperPath, wrapperSource, "utf-8");
	}
	return wrapperPath;
}

class AsyncQueue<T> implements AsyncIterable<T> {
	private items: T[] = [];
	private waiters: Array<(result: IteratorResult<T>) => void> = [];
	private closed = false;

	push(item: T): void {
		if (this.closed) {
			throw new Error("Queue is closed");
		}
		const waiter = this.waiters.shift();
		if (waiter) {
			waiter({ value: item, done: false });
			return;
		}
		this.items.push(item);
	}

	end(): void {
		if (this.closed) return;
		this.closed = true;
		for (const waiter of this.waiters.splice(0)) {
			waiter({ value: undefined, done: true });
		}
	}

	[Symbol.asyncIterator](): AsyncIterator<T> {
		return {
			next: () => {
				const item = this.items.shift();
				if (item !== undefined) {
					return Promise.resolve({ value: item, done: false });
				}
				if (this.closed) {
					return Promise.resolve({ value: undefined, done: true });
				}
				return new Promise<IteratorResult<T>>((resolve) => {
					this.waiters.push(resolve);
				});
			},
		};
	}
}

type PendingTurn = {
	resolve: (response: PromptResponse) => void;
	reject: (error: Error) => void;
	sawAssistantText: boolean;
	sawToolCall: boolean;
};

class ClaudeQuerySession {
	private promptQueue = new AsyncQueue<SDKUserMessage>();
	private query: Query;
	private readyPromise: Promise<void>;
	private pendingTurn: PendingTurn | null = null;
	private lastEmit: Promise<void> = Promise.resolve();
	private pendingMcpServers: Record<string, McpServerConfig> | undefined;
	private mcpServersApplied = false;
	private activeToolCalls = new Map<
		string,
		{ toolName: string; rawInput?: Record<string, unknown> }
	>();
	private reader: Promise<void>;
	private closed = false;
	private cancelled = false;

	constructor(
		private readonly conn: AgentSideConnection,
		readonly sessionId: string,
		private readonly cwd: string,
		private mode: PermissionMode,
		params: NewSessionRequest,
		private readonly pathToClaudeCodeExecutable: string,
		queryFactory: QueryFactory,
	) {
		traceAdapter(
			`session_ctor id=${sessionId} cwd=${cwd} mode=${mode} cli=${pathToClaudeCodeExecutable}`,
		);
		this.query = queryFactory({
			prompt: this.promptQueue,
			options: {
				canUseTool: this.createPermissionHandler(),
				cwd,
				env: {
					...process.env,
					CLAUDE_CODE_SHELL:
						process.env.CLAUDE_CODE_SHELL ?? "/bin/sh",
					CLAUDE_CODE_IGNORE_STARTUP_EXIT_CODE:
						process.env.CLAUDE_CODE_IGNORE_STARTUP_EXIT_CODE ?? "1",
					CLAUDE_CODE_DISABLE_DEV_NULL_REDIRECT:
						process.env.CLAUDE_CODE_DISABLE_DEV_NULL_REDIRECT ?? "1",
					CLAUDE_CODE_DISABLE_CWD_PERSIST:
						process.env.CLAUDE_CODE_DISABLE_CWD_PERSIST ?? "1",
					CLAUDE_CODE_SIMPLE_SHELL_EXEC:
						process.env.CLAUDE_CODE_SIMPLE_SHELL_EXEC ?? "1",
					CLAUDE_CODE_SIMPLE: process.env.CLAUDE_CODE_SIMPLE ?? "1",
					CLAUDE_CODE_NODE_SHELL_WRAPPER:
						process.env.CLAUDE_CODE_NODE_SHELL_WRAPPER ?? "1",
					CLAUDE_CODE_SKIP_INITIAL_MESSAGES:
						process.env.CLAUDE_CODE_SKIP_INITIAL_MESSAGES ?? "1",
					CLAUDE_CODE_SKIP_SPECIAL_ENTRYPOINTS:
						process.env.CLAUDE_CODE_SKIP_SPECIAL_ENTRYPOINTS ?? "1",
					CLAUDE_CODE_USE_PIPE_OUTPUT:
						process.env.CLAUDE_CODE_USE_PIPE_OUTPUT ?? "1",
					CLAUDE_CODE_TRACE_EXIT:
						process.env.CLAUDE_CODE_TRACE_EXIT ?? "0",
					CLAUDE_CODE_TRACE_STARTUP:
						process.env.CLAUDE_CODE_TRACE_STARTUP ?? "0",
					SHELL: process.env.SHELL ?? "/bin/sh",
				},
				extraArgs: {
					bare: null,
				},
				includePartialMessages: true,
				pathToClaudeCodeExecutable,
				permissionMode: normalizeClaudePermissionMode(mode),
				persistSession: false,
				sandbox: { enabled: false },
				settingSources: ["project"],
				spawnClaudeCodeProcess: ({ command, args, cwd, env }) => {
					traceAdapter(
						`spawn_child command=${command} args=${JSON.stringify(args)} cwd=${cwd}`,
					);
					const childEnv: NodeJS.ProcessEnv = {
						...env,
						CLAUDE_CODE_SWAP_STDIO:
							env.CLAUDE_CODE_SWAP_STDIO ?? "0",
					};
					const traceChildIo =
						childEnv.CLAUDE_CODE_TRACE_CHILD_IO === "1" ||
						(traceAdapterMessages && Boolean(traceFile));
					const child = spawn(command, args, {
						cwd,
						env: childEnv,
						stdio: ["pipe", "pipe", "pipe"],
					});
					const stdout = new PassThrough();
					const lineBuffers: Record<"stdout" | "stderr", string> = {
						stdout: "",
						stderr: "",
					};
					let openStreams = 2;

					const looksLikeProtocolLine = (line: string): boolean => {
						const trimmed = line.trim();
						if (!trimmed.startsWith("{")) {
							return false;
						}
						try {
							const parsed = JSON.parse(trimmed) as {
								type?: unknown;
								subtype?: unknown;
								response?: { request_id?: unknown } | unknown;
								message?: unknown;
							};
							return (
								typeof parsed.type === "string" ||
								typeof parsed.subtype === "string" ||
								(typeof parsed.response === "object" &&
									parsed.response !== null &&
									"request_id" in parsed.response) ||
								typeof parsed.message === "string"
							);
						} catch {
							return false;
						}
					};

					const writeSideLog = (source: "stdout" | "stderr", text: string) => {
						if (!text) return;
						if (traceChildIo) {
							traceAdapter(
								`child_side_${source} ${JSON.stringify(text).slice(0, 4000)}`,
							);
						}
						process.stderr.write(text);
					};

					const flushBuffer = (
						source: "stdout" | "stderr",
						final = false,
					): void => {
						const buffer = lineBuffers[source];
						const lines = buffer.split("\n");
						if (!final) {
							lineBuffers[source] = lines.pop() ?? "";
						} else {
							lineBuffers[source] = "";
						}
						for (const line of lines) {
							const text = `${line}\n`;
							if (looksLikeProtocolLine(line)) {
								if (traceChildIo) {
									traceAdapter(
										`child_protocol_${source} ${JSON.stringify(text).slice(0, 4000)}`,
									);
								}
								stdout.write(text);
							} else {
								writeSideLog(source, text);
							}
						}
						if (final && lineBuffers[source]) {
							const text = lineBuffers[source];
							if (looksLikeProtocolLine(text)) {
								if (traceChildIo) {
									traceAdapter(
										`child_protocol_${source} ${JSON.stringify(text).slice(0, 4000)}`,
									);
								}
								stdout.write(text);
							} else {
								writeSideLog(source, text);
							}
							lineBuffers[source] = "";
						}
					};

					const attachStream = (
						source: "stdout" | "stderr",
						stream: NodeJS.ReadableStream | null,
					): void => {
						if (!stream) {
							openStreams -= 1;
							if (openStreams <= 0) {
								stdout.end();
							}
							return;
						}
						stream.on("data", (chunk) => {
							lineBuffers[source] += String(chunk);
							flushBuffer(source);
						});
						stream.on("end", () => {
							flushBuffer(source, true);
							openStreams -= 1;
							if (openStreams <= 0) {
								stdout.end();
							}
						});
						stream.on("close", () => {
							flushBuffer(source, true);
						});
						stream.on("error", (error) => {
							stdout.destroy(error);
						});
					};

					attachStream("stdout", child.stdout);
					attachStream("stderr", child.stderr);

					return {
						stdin: child.stdin,
						stdout,
						get killed() {
							return child.killed;
						},
						get exitCode() {
							return child.exitCode;
						},
						kill: child.kill.bind(child),
						on: child.on.bind(child),
						once: child.once.bind(child),
						off: child.off.bind(child),
					};
				},
				stderr: (data) => process.stderr.write(data),
				systemPrompt: appendSystemPrompt
					? {
							type: "preset",
							preset: "claude_code",
							append: appendSystemPrompt,
						}
					: {
							type: "preset",
							preset: "claude_code",
						},
				tools: { type: "preset", preset: "claude_code" },
			},
		});
		this.readyPromise = this.initialize(params);
		this.reader = this.consume();
	}

	get currentMode(): PermissionMode {
		return this.mode;
	}

	async ready(): Promise<void> {
		await this.readyPromise;
	}

	async prompt(params: PromptRequest): Promise<PromptResponse> {
		if (this.closed) {
			throw new Error("Session is closed");
		}
		if (this.pendingTurn) {
			throw new Error("A Claude prompt is already running");
		}
		await this.readyPromise;

		const text = joinPromptText(params.prompt as PromptPart[] | undefined);
		traceAdapter(
			`prompt_start session=${this.sessionId} textLength=${text.length}`,
		);
		return new Promise<PromptResponse>((resolve, reject) => {
			this.cancelled = false;
			this.pendingTurn = {
				resolve,
				reject,
				sawAssistantText: false,
				sawToolCall: false,
			};
			void this.applyPendingMcpServers()
				.then(() => {
					traceAdapter(`prompt_queue_push session=${this.sessionId}`);
					this.promptQueue.push({
						type: "user",
						session_id: "",
						message: {
							role: "user",
							content: [{ type: "text", text }],
						},
						parent_tool_use_id: null,
					});
					traceAdapter(`prompt_queue_pushed session=${this.sessionId}`);
				})
				.catch((error) => {
					traceAdapter(
						`prompt_setup_error session=${this.sessionId} error=${formatError(error)}`,
					);
					if (this.pendingTurn) {
						this.pendingTurn.reject(asError(error));
						this.pendingTurn = null;
					}
				});
		});
	}

	async cancel(): Promise<void> {
		if (this.closed) return;
		this.cancelled = true;
		await this.readyPromise.catch(() => {});
		await this.query.interrupt();
	}

	async setMode(mode: PermissionMode): Promise<void> {
		await this.readyPromise;
		this.mode = mode;
		await this.query.setPermissionMode(mode);
		await this.emit({
			sessionUpdate: "current_mode_update" as const,
			currentModeId: mode,
		});
	}

	close(): void {
		if (this.closed) return;
		this.closed = true;
		this.promptQueue.end();
		this.query.close();
		if (this.pendingTurn) {
			this.pendingTurn.reject(new Error("Claude session closed"));
			this.pendingTurn = null;
		}
	}

	private async initialize(params: NewSessionRequest): Promise<void> {
		this.pendingMcpServers = toSdkMcpServers(params.mcpServers);
		traceAdapter(
			`session_initialize id=${this.sessionId} mcpServers=${Object.keys(
				this.pendingMcpServers ?? {},
			).length}`,
		);
	}

	private async applyPendingMcpServers(): Promise<void> {
		if (this.mcpServersApplied || !this.pendingMcpServers) {
			traceAdapter(
				`apply_mcp_skip session=${this.sessionId} applied=${String(this.mcpServersApplied)} pending=${String(Boolean(this.pendingMcpServers))}`,
			);
			return;
		}
		traceAdapter(`apply_mcp_start session=${this.sessionId}`);
		await this.query.setMcpServers(this.pendingMcpServers);
		this.mcpServersApplied = true;
		traceAdapter(`apply_mcp_done session=${this.sessionId}`);
	}

	private async consume(): Promise<void> {
		traceAdapter(`consume_start session=${this.sessionId}`);
		try {
			for await (const message of this.query) {
				traceAdapter(
					`consume_message session=${this.sessionId} type=${String(message.type ?? "")}`,
				);
				await this.handleMessage(message);
			}
			traceAdapter(`consume_done session=${this.sessionId}`);
		} catch (error) {
			traceAdapter(
				`consume_error session=${this.sessionId} error=${formatError(error)}`,
			);
			if (this.pendingTurn) {
				this.pendingTurn.reject(asError(error));
				this.pendingTurn = null;
			}
			if (!this.closed) {
				process.stderr.write(`${formatError(error)}\n`);
			}
		} finally {
			traceAdapter(`consume_finally session=${this.sessionId}`);
			if (this.pendingTurn) {
				this.pendingTurn.reject(
					new Error("Claude query ended before producing a result"),
				);
				this.pendingTurn = null;
			}
		}
	}

	private async handleMessage(message: Record<string, unknown>): Promise<void> {
		if (traceAdapterMessages) {
			const details =
				message.type === "result"
					? ` result=${JSON.stringify({
							subtype: message.subtype,
							result: message.result,
							error: message.error,
							errors: message.errors,
						})}`
					: "";
			traceAdapter(
				`adapter_message type=${String(message.type ?? "")} subtype=${String(
					message.subtype ?? "",
				)} pendingTurn=${String(Boolean(this.pendingTurn))}${details}`,
			);
		}
		switch (message.type) {
			case "stream_event":
				await this.handleStreamEvent(message);
				return;
			case "assistant":
				await this.handleAssistantMessage(message);
				return;
			case "tool_progress":
				await this.handleToolProgress(message);
				return;
			case "system":
				await this.handleSystemMessage(message);
				return;
			case "result":
				await this.handleResult(message);
				return;
			case "tool_use_summary":
				await this.emitText(String(message.summary ?? ""));
				return;
			default:
				return;
		}
	}

	private async handleStreamEvent(message: Record<string, unknown>): Promise<void> {
		if (!this.pendingTurn) return;

		const event = (message.event ?? {}) as Record<string, unknown>;
		const type = String(event.type ?? "");

		if (type === "content_block_delta") {
			const delta = (event.delta ?? {}) as Record<string, unknown>;
			if (delta.type === "text_delta" && typeof delta.text === "string") {
				if (this.pendingTurn) this.pendingTurn.sawAssistantText = true;
				await this.emit({
					sessionUpdate: "agent_message_chunk" as const,
					content: { type: "text" as const, text: delta.text },
				});
				return;
			}
			if (
				delta.type === "thinking_delta" &&
				typeof delta.thinking === "string"
			) {
				await this.emit({
					sessionUpdate: "agent_thought_chunk" as const,
					content: { type: "text" as const, text: delta.thinking },
				});
				return;
			}
			if (
				delta.type === "input_json_delta" &&
				typeof event.index === "number"
			) {
				const toolCall = this.findToolCallByIndex(Number(event.index));
				if (!toolCall) return;
				const rawInput = {
					...(toolCall.rawInput ?? {}),
					partial_json: String(delta.partial_json ?? ""),
				};
				toolCall.rawInput = rawInput;
				await this.emitToolCallUpdate(toolCall.toolName, toolCall.toolUseId, {
					rawInput,
					status: "pending",
				});
			}
			return;
		}

		if (type === "content_block_start") {
			const block = (event.content_block ?? {}) as Record<string, unknown>;
			if (block.type !== "tool_use") return;

			const toolUseId = String(block.id ?? "");
			if (!toolUseId) return;

			const toolName = String(block.name ?? "tool");
			const rawInput = isRecord(block.input) ? block.input : undefined;
			this.activeToolCalls.set(toolUseId, { toolName, rawInput });
			if (this.pendingTurn) this.pendingTurn.sawToolCall = true;

			await this.emit({
				sessionUpdate: "tool_call" as const,
				toolCallId: toolUseId,
				kind: toToolKind(toolName),
				locations: toToolCallLocations(rawInput, this.cwd),
				rawInput,
				status: "pending" as const,
				title: toolName,
			});
		}
	}

	private async handleAssistantMessage(
		message: Record<string, unknown>,
	): Promise<void> {
		if (!this.pendingTurn) return;

		const content = Array.isArray((message.message as Record<string, unknown>)?.content)
			? (((message.message as Record<string, unknown>).content ??
					[]) as Array<Record<string, unknown>>)
			: [];

		for (const block of content) {
			if (block.type === "tool_use") {
				const toolUseId = String(block.id ?? "");
				if (!toolUseId) continue;
				const toolName = String(block.name ?? "tool");
				const rawInput = isRecord(block.input) ? block.input : undefined;
				this.activeToolCalls.set(toolUseId, { toolName, rawInput });
				if (this.pendingTurn) this.pendingTurn.sawToolCall = true;

				await this.emit({
					sessionUpdate: "tool_call" as const,
					toolCallId: toolUseId,
					kind: toToolKind(toolName),
					locations: toToolCallLocations(rawInput, this.cwd),
					rawInput,
					status: "pending" as const,
					title: toolName,
				});
			}
		}

		const text = extractTextContent(content);
		if (text && this.pendingTurn && !this.pendingTurn.sawAssistantText) {
			this.pendingTurn.sawAssistantText = true;
			await this.emitText(text);
		}
	}

	private async handleToolProgress(
		message: Record<string, unknown>,
	): Promise<void> {
		if (!this.pendingTurn) return;

		const toolUseId = String(message.tool_use_id ?? "");
		const toolName = String(message.tool_name ?? "tool");
		if (!toolUseId) return;

		const existing = this.activeToolCalls.get(toolUseId);
		this.activeToolCalls.set(toolUseId, {
			toolName,
			rawInput: existing?.rawInput,
		});

		await this.emitToolCallUpdate(toolName, toolUseId, {
			status: "in_progress",
		});
	}

	private async handleSystemMessage(
		message: Record<string, unknown>,
	): Promise<void> {
		if (!this.pendingTurn) return;

		if (message.subtype === "local_command_output") {
			await this.emitText(String(message.content ?? ""));
		}
	}

	private async handleResult(message: Record<string, unknown>): Promise<void> {
		const turn = this.pendingTurn;
		if (!turn) return;

		const subtype = String(message.subtype ?? "success");
		const resultText =
			typeof message.result === "string" ? message.result : undefined;
		if (resultText && !turn.sawAssistantText) {
			await this.emitText(resultText);
		}

		for (const [toolUseId, info] of this.activeToolCalls) {
			await this.emitToolCallUpdate(info.toolName, toolUseId, {
				status: subtype === "success" ? "completed" : "failed",
			});
		}
		this.activeToolCalls.clear();

		await this.lastEmit;

		this.pendingTurn = null;
		turn.resolve({
			stopReason: this.cancelled ? "cancelled" : "end_turn",
		});
		this.cancelled = false;
	}

	private async emitText(text: string): Promise<void> {
		if (!text) return;
		await this.emit({
			sessionUpdate: "agent_message_chunk" as const,
			content: {
				type: "text" as const,
				text,
			},
		});
	}

	private emit(update: SessionUpdate): Promise<void> {
		this.lastEmit = this.lastEmit
			.then(() =>
				this.conn.sessionUpdate({
					sessionId: this.sessionId,
					update,
				}),
			)
			.catch(() => {});
		return this.lastEmit;
	}

	private async emitToolCallUpdate(
		toolName: string,
		toolUseId: string,
		update: {
			status: "pending" | "in_progress" | "completed" | "failed";
			rawInput?: Record<string, unknown>;
		},
	): Promise<void> {
		await this.emit({
			sessionUpdate: "tool_call_update" as const,
			toolCallId: toolUseId,
			kind: toToolKind(toolName),
			locations: toToolCallLocations(update.rawInput, this.cwd),
			rawInput: update.rawInput,
			status: update.status,
		});
	}

	private createPermissionHandler(): CanUseTool {
		return async (toolName, input, options) => {
			traceAdapter(
				`permission_request_start session=${this.sessionId} tool=${toolName} toolUseId=${options.toolUseID}`,
			);
			const request = {
				options: buildPermissionOptions(),
				sessionId: this.sessionId,
				toolCall: {
					kind: toToolKind(toolName),
					locations: toToolCallLocations(input, this.cwd),
					rawInput: input,
					status: "pending" as const,
					title: options.title ?? toolName,
					toolCallId: options.toolUseID,
				},
			};
			const permissionFallbackMs = Number.parseInt(
				process.env.CLAUDE_CODE_PERMISSION_FALLBACK_MS ?? "2000",
				10,
			);
			const response = await Promise.race([
				this.conn.requestPermission(request),
				new Promise<RequestPermissionResponse>((resolve) => {
					setTimeout(() => {
						traceAdapter(
							`permission_request_fallback session=${this.sessionId} tool=${toolName} toolUseId=${options.toolUseID}`,
						);
						resolve({
							outcome: {
								outcome: "selected",
								optionId: "allow_once",
							},
						});
					}, Number.isFinite(permissionFallbackMs) ? permissionFallbackMs : 2000);
				}),
			]);
			traceAdapter(
				`permission_request_done session=${this.sessionId} tool=${toolName} toolUseId=${options.toolUseID}`,
			);
			return toPermissionResult(
				response,
				options.suggestions,
				options.toolUseID,
				input,
			);
		};
	}

	private findToolCallByIndex(index: number): {
		toolUseId: string;
		toolName: string;
		rawInput?: Record<string, unknown>;
	} | null {
		const entry = [...this.activeToolCalls.entries()][index];
		if (!entry) return null;
		const [toolUseId, value] = entry;
		return { toolUseId, toolName: value.toolName, rawInput: value.rawInput };
	}
}

class ClaudeSdkAgent implements Agent {
	private sessions = new Map<string, ClaudeQuerySession>();

	constructor(private readonly conn: AgentSideConnection) {
		setTimeout(() => {
			void this.conn.closed.then(() => {
				for (const session of this.sessions.values()) {
					session.close();
				}
				this.sessions.clear();
			});
		}, 0);
	}

	async initialize(
		_params: InitializeRequest,
	): Promise<InitializeResponse> {
		return {
			protocolVersion: 1,
			agentInfo: {
				name: "claude-sdk-acp",
				title: "Claude Agent SDK ACP adapter",
				version: "0.1.0",
			},
			agentCapabilities: {
				promptCapabilities: {
					audio: false,
					embeddedContext: false,
					image: true,
				},
			},
		};
	}

	async newSession(params: NewSessionRequest): Promise<NewSessionResponse> {
		const sessionId = randomUUID();
		const sdk = await claudeSdkRuntimePromise;
		const session = new ClaudeQuerySession(
			this.conn,
			sessionId,
			params.cwd,
			"default",
			params,
			sdk.cliPath,
			sdk.query,
		);
		await session.ready();
		this.sessions.set(sessionId, session);
		return {
			sessionId,
			modes: {
				currentModeId: "default",
				availableModes: ACP_MODES.map((id) => ({
					id,
					name: id,
				})),
			},
		};
	}

	async prompt(params: PromptRequest): Promise<PromptResponse> {
		const session = this.requireSession(params.sessionId);
		return session.prompt(params);
	}

	async cancel(params: CancelNotification): Promise<void> {
		const session = this.requireSession(params.sessionId);
		await session.cancel();
	}

	async setSessionMode(
		params: SetSessionModeRequest,
	): Promise<SetSessionModeResponse | void> {
		const session = this.requireSession(params.sessionId);
		await session.setMode(params.modeId as PermissionMode);
		return {};
	}

	async authenticate(
		_params: AuthenticateRequest,
	): Promise<AuthenticateResponse | void> {
	}

	private requireSession(sessionId: string): ClaudeQuerySession {
		const session = this.sessions.get(sessionId);
		if (!session) {
			throw new Error(`Unknown Claude session: ${sessionId}`);
		}
		return session;
	}
}

function joinPromptText(prompt: PromptPart[] | undefined): string {
	return (prompt ?? [])
		.map((part) => (part.type === "text" ? (part.text ?? "") : ""))
		.join("");
}

function extractTextContent(content: Array<Record<string, unknown>>): string {
	return content
		.filter((block) => block.type === "text" && typeof block.text === "string")
		.map((block) => String(block.text))
		.join("");
}

function toSdkMcpServers(
	servers: NewSessionRequest["mcpServers"],
): Record<string, McpServerConfig> | undefined {
	if (!Array.isArray(servers) || servers.length === 0) {
		return undefined;
	}

	return Object.fromEntries(
		servers.map((server, index) => {
			const name = `mcp-${index + 1}`;
			const record = server as Record<string, unknown>;
			if (record.type === "local") {
				return [
					name,
					{
						args: Array.isArray(record.args)
							? (record.args as string[])
							: undefined,
						command: String(record.command ?? ""),
						env: isRecord(record.env)
							? (record.env as Record<string, string>)
							: undefined,
						type: "stdio",
					} satisfies McpServerConfig,
				];
			}

			return [
				name,
				{
					headers: isRecord(record.headers)
						? (record.headers as Record<string, string>)
						: undefined,
					type: "http",
					url: String(record.url ?? ""),
				} satisfies McpServerConfig,
			];
		}),
	);
}

function toToolKind(
	toolName: string,
): "read" | "edit" | "execute" | "search" | "fetch" | "think" | "other" {
	switch (toolName) {
		case "Read":
			return "read";
		case "Edit":
		case "Write":
		case "MultiEdit":
		case "NotebookEdit":
			return "edit";
		case "Bash":
		case "Monitor":
			return "execute";
		case "Grep":
		case "Glob":
		case "LS":
			return "search";
		case "WebFetch":
		case "WebSearch":
			return "fetch";
		case "Think":
			return "think";
		default:
			return "other";
	}
}

function toToolCallLocations(
	rawInput: Record<string, unknown> | undefined,
	cwd: string,
): Array<{ path: string; line?: number }> | undefined {
	const path =
		typeof rawInput?.file_path === "string"
			? rawInput.file_path
			: typeof rawInput?.path === "string"
				? rawInput.path
				: undefined;
	if (!path) return undefined;

	return [
		{
			path: isAbsolute(path) ? path : resolvePath(cwd, path),
		},
	];
}

function buildPermissionOptions(): Array<{
	kind: "allow_once" | "allow_always" | "reject_once" | "reject_always";
	name: string;
	optionId: string;
}> {
	return [
		{ kind: "allow_once", name: "Allow once", optionId: "allow_once" },
		{ kind: "allow_always", name: "Always allow", optionId: "allow_always" },
		{ kind: "reject_once", name: "Reject", optionId: "reject_once" },
	];
}

function toPermissionResult(
	response: RequestPermissionResponse,
	suggestions: PermissionUpdate[] | undefined,
	toolUseID: string,
	input: Record<string, unknown>,
): PermissionResult {
	if (response.outcome.outcome === "cancelled") {
		return {
			behavior: "deny",
			decisionClassification: "user_reject",
			interrupt: true,
			message: "Permission request cancelled",
			toolUseID,
		};
	}

	switch (response.outcome.optionId) {
		case "allow_always":
			return {
				behavior: "allow",
				decisionClassification: "user_permanent",
				toolUseID,
				updatedInput: input,
				updatedPermissions: suggestions,
			};
		case "allow_once":
			return {
				behavior: "allow",
				decisionClassification: "user_temporary",
				toolUseID,
				updatedInput: input,
			};
		default:
			return {
				behavior: "deny",
				decisionClassification: "user_reject",
				message: "Permission denied",
				toolUseID,
			};
	}
}

function normalizeClaudePermissionMode(mode: PermissionMode): PermissionMode {
	// Claude Code refuses bypassPermissions when running as root, which is the
	// normal VM user in this workspace. Fall back to the standard interactive
	// mode instead of letting startup abort.
	return mode === "bypassPermissions" ? "default" : mode;
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null;
}

function asError(error: unknown): Error {
	return error instanceof Error ? error : new Error(String(error));
}

function formatError(error: unknown): string {
	return asError(error).stack ?? asError(error).message;
}

const input = new WritableStream<Uint8Array>({
	write(chunk) {
		return new Promise<void>((resolve) => {
			process.stdout.write(chunk, () => resolve());
		});
	},
});

const output = new ReadableStream<Uint8Array>({
	start(controller) {
		process.stdin.on("data", (chunk: Buffer) => {
			controller.enqueue(new Uint8Array(chunk));
		});
		process.stdin.on("end", () => controller.close());
		process.stdin.on("error", (error: Error) => controller.error(error));
	},
});

const stream = ndJsonStream(input, output);
const _connection = new AgentSideConnection(
	(conn) => new ClaudeSdkAgent(conn),
	stream,
);

process.stdin.resume();
process.stdin.on("end", () => {
	process.exit(0);
});
