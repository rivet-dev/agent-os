#!/usr/bin/env node

import {
	type Agent,
	AgentSideConnection,
	RequestError,
	type AuthenticateRequest,
	type AuthenticateResponse,
	type CancelNotification,
	type InitializeRequest,
	type InitializeResponse,
	type NewSessionRequest,
	type NewSessionResponse,
	type PromptRequest,
	type PromptResponse,
	type SetSessionConfigOptionRequest,
	type SetSessionConfigOptionResponse,
	type SetSessionModeRequest,
	type SetSessionModeResponse,
	ndJsonStream,
} from "@agentclientprotocol/sdk";
import { randomUUID } from "node:crypto";
import { spawn, type ChildProcess } from "node:child_process";

type JsonRecord = Record<string, unknown>;
type SessionModeId = "default" | "plan";

type CodexSessionState = {
	sessionId: string;
	cwd: string;
	history: JsonRecord[];
	modeId: SessionModeId;
	model: string;
	thoughtLevel: string;
	activePrompt: ActivePrompt | null;
};

type ChildEvent =
	| {
			type: "text_delta";
			text: string;
	  }
	| {
			type: "tool_call_update";
			tool_call_id: string;
			command: string;
			status: "pending" | "in_progress" | "completed" | "failed";
			exit_code?: number;
			stdout?: string;
			stderr?: string;
	  }
	| {
			type: "permission_request";
			request_id: string;
			tool_call_id: string;
			command: string;
	  }
	| {
			type: "done";
			stop_reason: "end_turn" | "cancelled";
			assistant_text: string;
			history: JsonRecord[];
	  }
	| {
			type: "error";
			message: string;
	  };

const DEFAULT_MODEL = "gpt-5-codex";
const DEFAULT_THOUGHT_LEVEL = "medium";
const traceAdapter = process.env.CODEX_WASM_TRACE_ADAPTER === "1";

let appendDeveloperInstructions: string | undefined;
const argv = process.argv.slice(2);
for (let i = 0; i < argv.length; i++) {
	if (argv[i] === "--append-developer-instructions" && i + 1 < argv.length) {
		appendDeveloperInstructions = argv[i + 1];
		i++;
	}
}

function trace(message: string): void {
	if (!traceAdapter) return;
	process.stderr.write(`[agent-os-codex] ${message}\n`);
}

function createModes(currentModeId: SessionModeId) {
	return {
		currentModeId,
		availableModes: [
			{ id: "default", name: "Default", label: "Default" },
			{ id: "plan", name: "Plan", label: "Plan" },
		],
	};
}

function createConfigOptions(session: CodexSessionState) {
	return [
		{
			type: "select",
			id: "model",
			name: "Model",
			category: "model",
			currentValue: session.model,
			options: [
				{ value: DEFAULT_MODEL, name: DEFAULT_MODEL },
				{ value: "gpt-5.4", name: "gpt-5.4" },
			],
		},
		{
			type: "select",
			id: "thought_level",
			name: "Thought Level",
			category: "thought_level",
			currentValue: session.thoughtLevel,
			options: [
				{ value: "low", name: "Low" },
				{ value: "medium", name: "Medium" },
				{ value: "high", name: "High" },
			],
		},
	];
}

function buildPermissionOptions() {
	return [
		{ optionId: "allow_once", kind: "allow_once", name: "Allow once" },
		{ optionId: "allow_always", kind: "allow_always", name: "Always allow" },
		{ optionId: "reject_once", kind: "reject_once", name: "Reject" },
	] as const;
}

function sendLine(stream: NodeJS.WritableStream, value: JsonRecord): void {
	stream.write(`${JSON.stringify(value)}\n`);
}

class ActivePrompt {
	private child: ChildProcess;
	private stdoutBuffer = "";
	private stderr = "";
	private resolved = false;
	private exited = false;
	private forceKillTimer: NodeJS.Timeout | null = null;
	private resolvePrompt!: (value: PromptResponse) => void;
	private rejectPrompt!: (reason?: unknown) => void;
	private readonly promptPromise: Promise<PromptResponse>;
	private cancelled = false;

	constructor(
		private readonly conn: AgentSideConnection,
		private readonly session: CodexSessionState,
		private readonly promptText: string,
	) {
		this.promptPromise = new Promise<PromptResponse>((resolve, reject) => {
			this.resolvePrompt = resolve;
			this.rejectPrompt = reject;
		});

		const execCommand = process.env.CODEX_EXEC_COMMAND ?? "codex-exec";
		this.child = spawn(execCommand, ["--session-turn"], {
			cwd: session.cwd,
			env: process.env,
			stdio: ["pipe", "pipe", "pipe"],
		});

		this.child.stdout?.on("data", (chunk) => {
			this.stdoutBuffer += Buffer.from(chunk).toString("utf8");
			this.processStdoutBuffer();
		});
		this.child.stderr?.on("data", (chunk) => {
			if (this.resolved) return;
			const text = Buffer.from(chunk).toString("utf8");
			this.stderr += text;
			trace(`child stderr ${JSON.stringify(text)}`);
		});
		this.child.on("exit", (code, signal) => {
			this.exited = true;
			this.clearForceKillTimer();
			if (this.resolved) return;
			if (this.cancelled) {
				this.finish({ stopReason: "cancelled" });
				return;
			}
			this.rejectPrompt(
				RequestError.internalError(
					{
						code,
						signal,
						stderr: this.stderr.trim(),
					},
					"codex-exec exited before completing the prompt",
				),
			);
		});
		this.child.on("error", (error) => {
			if (this.resolved) return;
			this.rejectPrompt(
				RequestError.internalError(
					{ cause: error.message, stderr: this.stderr.trim() },
					"failed to spawn codex-exec",
				),
			);
		});

		sendLine(this.child.stdin!, {
			type: "start",
			cwd: session.cwd,
			mode: session.modeId,
			model: session.model,
			thought_level: session.thoughtLevel,
			developer_instructions: appendDeveloperInstructions,
			history: session.history,
			prompt: promptText,
		});
	}

	wait(): Promise<PromptResponse> {
		return this.promptPromise;
	}

	cancel(): void {
		if (this.cancelled || this.resolved) return;
		this.cancelled = true;
		this.finish({ stopReason: "cancelled" });
		this.child.stdin?.destroy();
		this.child.kill("SIGTERM");
		this.forceKillTimer = setTimeout(() => {
			if (this.exited) {
				return;
			}
			this.child.kill("SIGKILL");
		}, 500);
	}

	private finish(result: PromptResponse): void {
		if (this.resolved) return;
		this.resolved = true;
		this.resolvePrompt(result);
	}

	private processStdoutBuffer(): void {
		while (true) {
			if (this.resolved) {
				this.stdoutBuffer = "";
				return;
			}
			const newline = this.stdoutBuffer.indexOf("\n");
			if (newline === -1) break;
			const line = this.stdoutBuffer.slice(0, newline).trim();
			this.stdoutBuffer = this.stdoutBuffer.slice(newline + 1);
			if (!line) continue;

			let event: ChildEvent;
			try {
				event = JSON.parse(line) as ChildEvent;
			} catch (error) {
				trace(`bad child json ${String(error)}`);
				continue;
			}

			void this.handleEvent(event);
		}
	}

	private async handleEvent(event: ChildEvent): Promise<void> {
		if (this.resolved) {
			return;
		}
		switch (event.type) {
			case "text_delta":
				await this.conn.sessionUpdate({
					sessionId: this.session.sessionId,
					update: {
						sessionUpdate: "agent_message_chunk",
						content: {
							type: "text",
							text: event.text,
						},
					},
				});
				return;

			case "tool_call_update":
				await this.conn.sessionUpdate({
					sessionId: this.session.sessionId,
					update: {
						sessionUpdate: "tool_call_update",
						toolCallId: event.tool_call_id,
						kind: "execute",
						status: event.status,
						title: "Shell",
						rawInput: { command: event.command },
						rawOutput:
							event.stdout || event.stderr
								? {
										type: "text",
										text: [event.stdout, event.stderr]
											.filter(Boolean)
											.join("\n"),
									}
								: undefined,
					},
				});
				return;

			case "permission_request": {
				const response = await this.conn.requestPermission({
					sessionId: this.session.sessionId,
					options: buildPermissionOptions() as any,
					toolCall: {
						kind: "execute",
						toolCallId: event.tool_call_id,
						title: "Shell",
						status: "pending",
						rawInput: {
							command: event.command,
						},
					},
				});
				const optionId =
					response.outcome.outcome === "selected"
						? response.outcome.optionId
						: "reject_once";
				sendLine(this.child.stdin!, {
					type: "permission_response",
					request_id: event.request_id,
					option_id: optionId,
				});
				return;
			}

			case "done":
				this.session.history = event.history;
				this.finish({
					stopReason: event.stop_reason,
				});
				return;

			case "error":
				this.rejectPrompt(
					RequestError.internalError(
						{ stderr: this.stderr.trim() },
						event.message,
					),
				);
				return;
		}
	}

	private clearForceKillTimer(): void {
		if (this.forceKillTimer === null) {
			return;
		}
		clearTimeout(this.forceKillTimer);
		this.forceKillTimer = null;
	}
}

class CodexAgent implements Agent {
	private readonly sessions = new Map<string, CodexSessionState>();

	constructor(private readonly conn: AgentSideConnection) {
		this.setSessionMode = this.setSessionMode.bind(this);
		this.setSessionConfigOption = this.setSessionConfigOption.bind(this);
		this.prompt = this.prompt.bind(this);
		this.cancel = this.cancel.bind(this);

		setTimeout(() => {
			void this.conn.closed.then(() => {
				for (const session of this.sessions.values()) {
					session.activePrompt?.cancel();
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
				name: "codex-wasm-acp",
				title: "Codex WASM ACP adapter",
				version: "0.1.0",
			},
			agentCapabilities: {
				permissions: true,
				plan_mode: true,
				tool_calls: true,
				text_messages: true,
				session_lifecycle: true,
				reasoning: true,
				streaming_deltas: true,
				promptCapabilities: {
					audio: false,
					embeddedContext: false,
					image: false,
				},
				sessionCapabilities: {
					close: {},
					resume: {},
				},
			} as any,
		};
	}

	async newSession(
		params: NewSessionRequest,
	): Promise<NewSessionResponse> {
		const sessionId = randomUUID();
		const session: CodexSessionState = {
			sessionId,
			cwd: params.cwd,
			history: [],
			modeId: "default",
			model: DEFAULT_MODEL,
			thoughtLevel: DEFAULT_THOUGHT_LEVEL,
			activePrompt: null,
		};
		this.sessions.set(sessionId, session);

		return {
			sessionId,
			modes: createModes(session.modeId) as any,
			configOptions: createConfigOptions(session) as any,
		};
	}

	async setSessionMode(
		params: SetSessionModeRequest,
	): Promise<SetSessionModeResponse | void> {
		const session = this.requireSession(params.sessionId);
		if (params.modeId !== "default" && params.modeId !== "plan") {
			throw RequestError.invalidParams(
				{ modeId: params.modeId },
				"unsupported mode",
			);
		}

		session.modeId = params.modeId;
		await this.conn.sessionUpdate({
			sessionId: session.sessionId,
			update: {
				sessionUpdate: "current_mode_update",
				currentModeId: session.modeId,
			},
		});
		return {};
	}

	async setSessionConfigOption(
		params: SetSessionConfigOptionRequest,
	): Promise<SetSessionConfigOptionResponse> {
		const session = this.requireSession(params.sessionId);
		if (typeof params.value !== "string") {
			throw RequestError.invalidParams(
				{ value: params.value },
				"codex config options must be strings",
			);
		}
		if (params.configId === "model") {
			session.model = params.value;
		} else if (params.configId === "thought_level") {
			session.thoughtLevel = params.value;
		} else {
			throw RequestError.invalidParams(
				{ configId: params.configId },
				"unsupported config option",
			);
		}

		const configOptions = createConfigOptions(session);
		await this.conn.sessionUpdate({
			sessionId: session.sessionId,
			update: {
				sessionUpdate: "config_option_update",
				configOptions: configOptions as any,
			},
		});
		return { configOptions: configOptions as any };
	}

	async authenticate(
		_params: AuthenticateRequest,
	): Promise<AuthenticateResponse | void> {
	}

	async prompt(params: PromptRequest): Promise<PromptResponse> {
		const session = this.requireSession(params.sessionId);
		if (session.activePrompt) {
			throw RequestError.invalidRequest(
				{ sessionId: session.sessionId },
				"session already has an active prompt",
			);
		}

		const meta =
			params._meta && typeof params._meta === "object"
				? (params._meta as Record<string, unknown>)
				: null;
		const config =
			meta?.agentOsCodexConfig &&
			typeof meta.agentOsCodexConfig === "object" &&
			!Array.isArray(meta.agentOsCodexConfig)
				? (meta.agentOsCodexConfig as Record<string, unknown>)
				: null;
		if (typeof config?.model === "string") {
			session.model = config.model;
		}
		if (typeof config?.thought_level === "string") {
			session.thoughtLevel = config.thought_level;
		}

		const promptText = (params.prompt ?? [])
			.map((part: { type?: string; text?: string }) =>
				part.type === "text" ? (part.text ?? "") : "",
			)
			.join("");

		const execution = new ActivePrompt(this.conn, session, promptText);
		session.activePrompt = execution;
		try {
			const response = await execution.wait();
			return response;
		} finally {
			session.activePrompt = null;
		}
	}

	async cancel(params: CancelNotification): Promise<void> {
		const session = this.requireSession(params.sessionId);
		session.activePrompt?.cancel();
	}

	private requireSession(sessionId: string): CodexSessionState {
		const session = this.sessions.get(sessionId);
		if (!session) {
			throw RequestError.invalidParams(
				{ sessionId },
				"unknown session",
			);
		}
		return session;
	}
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
const connection = new AgentSideConnection(
	(conn: AgentSideConnection) => new CodexAgent(conn),
	stream,
);

process.stdin.resume();
process.stdin.on("end", () => {
	process.exit(0);
});

void connection.closed;
