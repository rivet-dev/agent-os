#!/usr/bin/env node

import {
	AgentSideConnection,
	RequestError,
	ndJsonStream,
	type Agent,
	type AuthenticateRequest,
	type AuthenticateResponse,
	type CancelNotification,
	type CloseSessionRequest,
	type InitializeRequest,
	type InitializeResponse,
	type NewSessionRequest,
	type NewSessionResponse,
	type PromptRequest,
	type PromptResponse,
	type ResumeSessionRequest,
	type ResumeSessionResponse,
	type SetSessionConfigOptionRequest,
	type SetSessionConfigOptionResponse,
	type SetSessionModeRequest,
	type SetSessionModeResponse,
} from "@agentclientprotocol/sdk";
import { spawn, type ChildProcess } from "node:child_process";
import { randomUUID } from "node:crypto";
import {
	mkdirSync,
	readFileSync,
	renameSync,
	rmSync,
	writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";

type JsonRecord = Record<string, unknown>;
type SessionModeId = "default" | "plan";
type ReasoningEffort = "low" | "medium" | "high" | "xhigh" | "max" | "ultra";

type SessionState = {
	sessionId: string;
	cwd: string;
	history: JsonRecord[];
	modeId: SessionModeId;
	model: string;
	reasoningEffort: ReasoningEffort;
	activePrompt: ActivePrompt | null;
};

type PersistedSession = Omit<SessionState, "activePrompt">;

type ChildEvent =
	| { type: "start" }
	| { type: "text_delta"; delta?: string; text?: string }
	| { type: "reasoning_delta"; delta?: string; text?: string }
	| {
			type: "tool_call_update";
			tool_call_id: string;
			kind?: "execute" | "edit";
			title?: string;
			command?: string;
			status: "pending" | "in_progress" | "completed" | "failed";
			exit_code?: number;
			stdout?: string;
			stderr?: string;
			locations?: Array<{ path: string; line?: number }>;
	  }
	| {
			type: "permission_request";
			request_id?: string;
			tool_call_id: string;
			kind?: "execute" | "edit";
			title?: string;
			command?: string;
			locations?: Array<{ path: string; line?: number }>;
	  }
	| {
			type: "done";
			stop_reason?: "end_turn" | "cancelled";
			assistant_text?: string;
			history?: JsonRecord[];
	  }
	| { type: "error"; message: string };

const DEFAULT_MODEL = process.env.CODEX_DEFAULT_MODEL ?? "gpt-5.6-sol";
const DEFAULT_REASONING_EFFORT: ReasoningEffort = "low";
// Keep this aligned with native Codex's online `model/list` response. The
// pinned WASI runtime accepts these service model IDs even though its embedded
// offline fallback catalog predates them.
const CODEX_MODELS = [
	"gpt-5.6-sol",
	"gpt-5.6-terra",
	"gpt-5.6-luna",
	"gpt-5.5",
	"gpt-5.3-codex-spark",
] as const;
const REASONING_EFFORTS = ["low", "medium", "high", "xhigh", "max", "ultra"] as const;
const STANDARD_REASONING_EFFORTS = ["low", "medium", "high", "xhigh"] as const;
const CODEX_REASONING_EFFORTS: Record<
	(typeof CODEX_MODELS)[number],
	readonly ReasoningEffort[]
> = {
	"gpt-5.6-sol": REASONING_EFFORTS,
	"gpt-5.6-terra": REASONING_EFFORTS,
	"gpt-5.6-luna": ["low", "medium", "high", "xhigh", "max"],
	"gpt-5.5": STANDARD_REASONING_EFFORTS,
	"gpt-5.3-codex-spark": STANDARD_REASONING_EFFORTS,
};
const traceEnabled = process.env.CODEX_WASM_TRACE_ADAPTER === "1";
const sessionDirectory = join(
	process.env.CODEX_HOME ?? join(homedir(), ".codex"),
	"agentos-sessions",
);

function sessionPath(sessionId: string): string {
	return join(sessionDirectory, `${encodeURIComponent(sessionId)}.json`);
}

function persistSession(session: SessionState): void {
	const persisted: PersistedSession = {
		sessionId: session.sessionId,
		cwd: session.cwd,
		history: session.history,
		modeId: session.modeId,
		model: session.model,
		reasoningEffort: session.reasoningEffort,
	};
	const path = sessionPath(session.sessionId);
	const temporaryPath = `${path}.${process.pid}.${randomUUID()}.tmp`;
	mkdirSync(dirname(path), { recursive: true });
	try {
		writeFileSync(temporaryPath, `${JSON.stringify(persisted)}\n`, {
			encoding: "utf8",
			mode: 0o600,
		});
		renameSync(temporaryPath, path);
	} catch (error) {
		rmSync(temporaryPath, { force: true });
		throw error;
	}
}

function loadSession(sessionId: string): SessionState | null {
	try {
		const value = JSON.parse(
			readFileSync(sessionPath(sessionId), "utf8"),
		) as Partial<PersistedSession>;
		if (
			value.sessionId !== sessionId ||
			typeof value.cwd !== "string" ||
			!Array.isArray(value.history) ||
			(value.modeId !== "default" && value.modeId !== "plan") ||
			typeof value.model !== "string"
		) {
			return null;
		}
		const reasoningEffort = REASONING_EFFORTS.includes(
			value.reasoningEffort as ReasoningEffort,
		)
			? (value.reasoningEffort as ReasoningEffort)
			: DEFAULT_REASONING_EFFORT;
		return { ...value, reasoningEffort, activePrompt: null } as SessionState;
	} catch {
		return null;
	}
}

let appendedInstructions: string | undefined;
for (let index = 2; index < process.argv.length; index++) {
	if (
		process.argv[index] === "--append-developer-instructions" &&
		process.argv[index + 1]
	) {
		appendedInstructions = process.argv[++index];
	}
}

function trace(message: string): void {
	if (traceEnabled) process.stderr.write(`[agentos-codex] ${message}\n`);
}

function modes(currentModeId: SessionModeId) {
	return {
		currentModeId,
		availableModes: [
			{ id: "default", name: "Default" },
			{ id: "plan", name: "Plan" },
		],
	};
}

function configOptions(session: SessionState) {
	const efforts =
		CODEX_REASONING_EFFORTS[
			session.model as (typeof CODEX_MODELS)[number]
		] ?? REASONING_EFFORTS;
	return [
		{
			type: "select" as const,
			id: "model",
			name: "Model",
			category: "model" as const,
			currentValue: session.model,
			options: CODEX_MODELS.flatMap((model) => [
				{ value: model, name: model },
				...CODEX_REASONING_EFFORTS[model].map((effort) => ({
					value: `${model}/${effort}`,
					name: `${model} (${effort === "xhigh" ? "Extra high" : `${effort[0].toUpperCase()}${effort.slice(1)}`})`,
				})),
			]),
		},
		{
			type: "select" as const,
			id: "reasoning_effort",
			name: "Reasoning effort",
			category: "thought_level" as const,
			currentValue: session.reasoningEffort,
			options: efforts.map((effort) => ({
				value: effort,
				name: effort === "xhigh" ? "Extra high" : `${effort[0].toUpperCase()}${effort.slice(1)}`,
			})),
		},
	];
}

function sendLine(stream: NodeJS.WritableStream, value: JsonRecord): void {
	stream.write(`${JSON.stringify(value)}\n`);
}

class ActivePrompt {
	private readonly child: ChildProcess;
	private stdoutBuffer = "";
	private stderr = "";
	private assistantText = "";
	private settled = false;
	private cancelled = false;
	private exited = false;
	private pendingResult: PromptResponse | null = null;
	private forceKillTimer: NodeJS.Timeout | null = null;
	private eventChain = Promise.resolve();
	private readonly result: Promise<PromptResponse>;
	private resolveResult!: (value: PromptResponse) => void;
	private rejectResult!: (reason?: unknown) => void;

	constructor(
		private readonly connection: AgentSideConnection,
		private readonly session: SessionState,
		private readonly promptText: string,
	) {
		this.result = new Promise((resolve, reject) => {
			this.resolveResult = resolve;
			this.rejectResult = reject;
		});
		this.child = spawn(
			process.env.CODEX_EXEC_COMMAND ?? "codex-exec",
			["--session-turn"],
			{
				cwd: session.cwd,
				env: process.env,
				stdio: ["pipe", "pipe", "pipe"],
			},
		);
		this.child.stdout?.on("data", (chunk) => {
			this.stdoutBuffer += Buffer.from(chunk).toString("utf8");
			this.processStdout();
		});
		this.child.stderr?.on("data", (chunk) => {
			const text = Buffer.from(chunk).toString("utf8");
			this.stderr += text;
			trace(`child stderr ${JSON.stringify(text)}`);
		});
		this.child.on("error", (error) => {
			this.fail("failed to spawn codex-exec", { cause: error.message });
		});
		this.child.on("exit", (code, signal) => {
			this.exited = true;
			this.clearKillTimer();
			void this.eventChain.finally(() => {
				if (this.settled) return;
				if (this.pendingResult) {
					this.finish(this.pendingResult);
				} else if (this.cancelled) {
					this.finish({ stopReason: "cancelled" });
				} else {
					this.fail("codex-exec exited before completing the prompt", {
						code,
						signal,
					});
				}
			});
		});

		sendLine(this.child.stdin!, {
			type: "start",
			cwd: session.cwd,
			mode: session.modeId,
			model: session.model,
			developer_instructions: appendedInstructions,
			history: session.history,
			prompt: promptText,
			reasoning_effort: session.reasoningEffort,
		});
	}

	wait(): Promise<PromptResponse> {
		return this.result;
	}

	cancel(): void {
		if (this.cancelled || this.settled) return;
		this.cancelled = true;
		this.child.stdin?.destroy();
		this.child.kill("SIGTERM");
		this.forceKillTimer = setTimeout(() => {
			if (!this.exited) this.child.kill("SIGKILL");
		}, 500);
	}

	private processStdout(): void {
		while (!this.settled) {
			const newline = this.stdoutBuffer.indexOf("\n");
			if (newline === -1) return;
			const line = this.stdoutBuffer.slice(0, newline).trim();
			this.stdoutBuffer = this.stdoutBuffer.slice(newline + 1);
			if (!line) continue;
			let event: ChildEvent;
			try {
				event = JSON.parse(line) as ChildEvent;
			} catch (error) {
				trace(`ignored malformed child event: ${String(error)}`);
				continue;
			}
			this.eventChain = this.eventChain.then(() => this.handleEvent(event));
		}
	}

	private async handleEvent(event: ChildEvent): Promise<void> {
		if (this.settled) return;
		switch (event.type) {
			case "start":
				return;
			case "text_delta": {
				const text = event.delta ?? event.text ?? "";
				this.assistantText += text;
				if (text) {
					await this.connection.sessionUpdate({
						sessionId: this.session.sessionId,
						update: {
							sessionUpdate: "agent_message_chunk",
							content: { type: "text", text },
						},
					});
				}
				return;
			}
			case "reasoning_delta": {
				const text = event.delta ?? event.text ?? "";
				if (text) {
					await this.connection.sessionUpdate({
						sessionId: this.session.sessionId,
						update: {
							sessionUpdate: "agent_thought_chunk",
							content: { type: "text", text },
						},
					});
				}
				return;
			}
			case "tool_call_update":
				await this.connection.sessionUpdate({
					sessionId: this.session.sessionId,
					update: {
						sessionUpdate: "tool_call_update",
						toolCallId: event.tool_call_id,
						kind: event.kind ?? "execute",
						status: event.status,
						title: event.command ?? event.title ?? "Shell",
						rawInput: event.command ? { command: event.command } : undefined,
						rawOutput:
							event.stdout || event.stderr
								? {
										type: "text",
										text: [event.stdout, event.stderr]
											.filter(Boolean)
											.join("\n"),
									}
								: undefined,
						locations: event.locations,
					},
				});
				return;
			case "permission_request": {
				const response = await this.connection.requestPermission({
					sessionId: this.session.sessionId,
					options: [
						{ optionId: "allow_once", kind: "allow_once", name: "Allow once" },
						{ optionId: "allow_always", kind: "allow_always", name: "Always allow" },
						{ optionId: "reject_once", kind: "reject_once", name: "Reject" },
					],
					toolCall: {
						toolCallId: event.tool_call_id,
						kind: event.kind ?? "execute",
						status: "pending",
						title: event.command ?? event.title ?? "Shell",
						rawInput: event.command ? { command: event.command } : undefined,
						locations: event.locations,
					},
				});
				const optionId =
					response.outcome.outcome === "selected"
						? response.outcome.optionId
						: "reject_once";
				sendLine(this.child.stdin!, {
					type: "permission_response",
					request_id: event.request_id ?? event.tool_call_id,
					decision: optionId.startsWith("allow") ? "allow" : "deny",
				});
				return;
			}
			case "done": {
				const assistantText = event.assistant_text ?? this.assistantText;
				this.session.history =
					event.history ?? [
						...this.session.history,
						{ role: "user", content: this.promptText },
						...(assistantText
							? [{ role: "assistant", content: assistantText }]
							: []),
					];
				this.pendingResult = {
					stopReason: event.stop_reason ?? "end_turn",
				};
				this.child.stdin?.end();
				if (this.exited) this.finish(this.pendingResult);
				return;
			}
			case "error":
				this.child.stdin?.end();
				this.fail(event.message);
				return;
		}
	}

	private finish(response: PromptResponse): void {
		if (this.settled) return;
		this.settled = true;
		this.resolveResult(response);
	}

	private fail(message: string, metadata: JsonRecord = {}): void {
		if (this.settled) return;
		this.settled = true;
		this.rejectResult(
			RequestError.internalError(
				{ ...metadata, stderr: this.stderr.trim() },
				message,
			),
		);
	}

	private clearKillTimer(): void {
		if (this.forceKillTimer) clearTimeout(this.forceKillTimer);
		this.forceKillTimer = null;
	}
}

class CodexAgent implements Agent {
	private readonly sessions = new Map<string, SessionState>();

	constructor(private readonly connection: AgentSideConnection) {}

	initialize(params: InitializeRequest): InitializeResponse {
		return {
			protocolVersion: params.protocolVersion,
			agentInfo: {
				name: "codex-wasm-acp",
				title: "Codex WASI ACP adapter",
				version: "0.1.0",
			},
			agentCapabilities: {
				promptCapabilities: { image: false, audio: false, embeddedContext: false },
				sessionCapabilities: { close: {}, resume: {} },
			},
		};
	}

	newSession(params: NewSessionRequest): NewSessionResponse {
		const session: SessionState = {
			sessionId: randomUUID(),
			cwd: params.cwd,
			history: [],
			modeId: "default",
			model: DEFAULT_MODEL,
			reasoningEffort: DEFAULT_REASONING_EFFORT,
			activePrompt: null,
		};
		this.sessions.set(session.sessionId, session);
		persistSession(session);
		return {
			sessionId: session.sessionId,
			modes: modes(session.modeId),
			configOptions: configOptions(session),
		};
	}

	resumeSession(params: ResumeSessionRequest): ResumeSessionResponse {
		const session = this.sessions.get(params.sessionId) ?? loadSession(params.sessionId);
		if (!session) {
			throw RequestError.invalidParams(
				{ sessionId: params.sessionId },
				"unknown session",
			);
		}
		if (session.cwd !== params.cwd) {
			throw RequestError.invalidParams(
				{ expectedCwd: session.cwd, cwd: params.cwd },
				"session cwd does not match",
			);
		}
		this.sessions.set(session.sessionId, session);
		return { modes: modes(session.modeId), configOptions: configOptions(session) };
	}

	closeSession(params: CloseSessionRequest): void {
		const session = this.sessions.get(params.sessionId);
		session?.activePrompt?.cancel();
		this.sessions.delete(params.sessionId);
		rmSync(sessionPath(params.sessionId), { force: true });
	}

	async setSessionMode(
		params: SetSessionModeRequest,
	): Promise<SetSessionModeResponse> {
		const session = this.requireSession(params.sessionId);
		if (params.modeId !== "default" && params.modeId !== "plan") {
			throw RequestError.invalidParams(
				{ modeId: params.modeId },
				"unsupported mode",
			);
		}
		session.modeId = params.modeId;
		persistSession(session);
		await this.connection.sessionUpdate({
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
				{ configId: params.configId, value: params.value },
				"unsupported Codex configuration option",
			);
		}
		if (params.configId === "model") {
			const selectedModel = CODEX_MODELS.find(
				(model) =>
					params.value === model || params.value.startsWith(`${model}/`),
			);
			if (
				!selectedModel
			) {
				throw RequestError.invalidParams(
					{ value: params.value },
					"unsupported Codex model",
				);
			}
			const qualifiedEffort = params.value.slice(selectedModel.length + 1);
			if (qualifiedEffort) {
				if (
					!CODEX_REASONING_EFFORTS[selectedModel].includes(
						qualifiedEffort as ReasoningEffort,
					)
				) {
					throw RequestError.invalidParams(
						{ value: params.value },
						"unsupported Codex model reasoning effort",
					);
				}
				session.reasoningEffort = qualifiedEffort as ReasoningEffort;
			}
			session.model = selectedModel;
			if (
				!CODEX_REASONING_EFFORTS[selectedModel].includes(
					session.reasoningEffort,
				)
			) {
				session.reasoningEffort = CODEX_REASONING_EFFORTS[selectedModel][0];
			}
		} else if (params.configId === "reasoning_effort") {
			const efforts =
				CODEX_REASONING_EFFORTS[
					session.model as (typeof CODEX_MODELS)[number]
				] ?? REASONING_EFFORTS;
			if (!efforts.includes(params.value as ReasoningEffort)) {
				throw RequestError.invalidParams(
					{ value: params.value },
					"unsupported Codex reasoning effort",
				);
			}
			session.reasoningEffort = params.value as ReasoningEffort;
		} else {
			throw RequestError.invalidParams(
				{ configId: params.configId },
				"unsupported Codex configuration option",
			);
		}
		persistSession(session);
		const options = configOptions(session);
		await this.connection.sessionUpdate({
			sessionId: session.sessionId,
			update: { sessionUpdate: "config_option_update", configOptions: options },
		});
		return { configOptions: options };
	}

	authenticate(
		_params: AuthenticateRequest,
	): AuthenticateResponse | void {}

	async prompt(params: PromptRequest): Promise<PromptResponse> {
		const session = this.requireSession(params.sessionId);
		if (session.activePrompt) {
			throw RequestError.invalidRequest(
				{ sessionId: session.sessionId },
				"session already has an active prompt",
			);
		}
		const promptTextParts: string[] = [];
		for (const part of params.prompt) {
			if (part.type !== "text") {
				throw RequestError.invalidParams(
					{ contentType: part.type },
					`Codex does not support ${part.type} prompt content`,
				);
			}
			promptTextParts.push(part.text);
		}
		const promptText = promptTextParts.join("");
		const active = new ActivePrompt(this.connection, session, promptText);
		session.activePrompt = active;
		try {
			return await active.wait();
		} finally {
			session.activePrompt = null;
			persistSession(session);
		}
	}

	cancel(params: CancelNotification): void {
		this.requireSession(params.sessionId).activePrompt?.cancel();
	}

	private requireSession(sessionId: string): SessionState {
		const session = this.sessions.get(sessionId);
		if (!session) {
			throw RequestError.invalidParams({ sessionId }, "unknown session");
		}
		return session;
	}
}

const output = new WritableStream<Uint8Array>({
	write(chunk) {
		return new Promise<void>((resolve) => {
			process.stdout.write(chunk, () => resolve());
		});
	},
});
const input = new ReadableStream<Uint8Array>({
	start(controller) {
		process.stdin.on("data", (chunk: Buffer) => {
			controller.enqueue(new Uint8Array(chunk));
		});
		process.stdin.on("end", () => controller.close());
		process.stdin.on("error", (error) => controller.error(error));
	},
});

const connection = new AgentSideConnection(
	(conn) => new CodexAgent(conn),
	ndJsonStream(output, input),
);
process.stdin.resume();
void connection.closed;
