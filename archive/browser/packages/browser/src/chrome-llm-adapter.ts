// The chrome-llm host-callback handler core (AGENTOS-WEB-ASYNC-AGENTS.md §6): the
// trusted main-thread adapter that turns a chat-completion request (the shape pi's
// HTTP client sends to its baseUrl) into a Chrome built-in `LanguageModel` call and
// formats the reply back. This is the browser twin of native's `llmock-server.mjs`,
// except backed by on-device inference instead of a mock/remote API.
//
// It is the only NEW trusted code in the inference path: the in-sandbox proxy guest
// reaches it via the existing kernel-brokered host-callback (so it is mediated by
// the policy, not an ambient capability), and pi reaches the proxy over the kernel
// loopback. pi needs ZERO changes beyond a baseUrl.
//
// Pure + injectable: the model is an interface, so a mock (returning a fixed
// sentinel) drives the deterministic CI gate and the real `LanguageModel` drives
// the best-effort Nano smoke. Non-streaming for now (single prompt → single reply).

/** The slice of Chrome's built-in `LanguageModel` we use (a created session). */
export interface LanguageModelSession {
	prompt(input: string): Promise<string>;
}

export const LANGUAGE_MODEL_OPTIONS = {
	expectedInputs: [{ type: "text", languages: ["en"] }],
	expectedOutputs: [{ type: "text", languages: ["en"] }],
};

export interface ChromeLanguageModelSessionOptions {
	allowDownload?: boolean;
	onDownloadProgress?(progress: number): void;
	signal?: AbortSignal;
}

function getLanguageModelGlobal() {
	return (globalThis as unknown as { LanguageModel?: {
		availability(options?: unknown): Promise<string>;
		create(options?: unknown): Promise<LanguageModelSession>;
	} }).LanguageModel;
}

export async function getChromeLanguageModelAvailability(): Promise<string> {
	const LanguageModel = getLanguageModelGlobal();
	if (!LanguageModel) return "missing-global";
	try {
		return await LanguageModel.availability(LANGUAGE_MODEL_OPTIONS);
	} catch (error) {
		return `availability-error:${error instanceof Error ? error.message : String(error)}`;
	}
}

/** Minimal chat request shape — accepts OpenAI (`messages`) and Anthropic
 * (`messages` + top-level `system`) bodies; both are `{role, content}` arrays. */
interface ChatRequest {
	model?: string;
	system?: string;
	messages?: { role: string; content: unknown }[];
	prompt?: string;
}

function contentToText(content: unknown): string {
	if (typeof content === "string") return content;
	// Anthropic/OpenAI content can be an array of parts ({type:"text", text}).
	if (Array.isArray(content)) {
		return content
			.map((part) =>
				part && typeof part === "object" && "text" in part
					? String((part as { text: unknown }).text)
					: typeof part === "string"
						? part
						: "",
			)
			.join("");
	}
	return "";
}

/** Flatten a chat request into a single prompt string for the on-device model. */
export function chatRequestToPrompt(request: ChatRequest): string {
	if (typeof request.prompt === "string") return request.prompt;
	const lines: string[] = [];
	if (request.system) lines.push(`system: ${request.system}`);
	for (const message of request.messages ?? []) {
		lines.push(`${message.role}: ${contentToText(message.content)}`);
	}
	return lines.join("\n");
}

/**
 * Handle one chat-completion request against the on-device model. Returns an
 * OpenAI-chat-completion-shaped JSON string (the shape pi's client decodes).
 */
export async function handleChatCompletion(
	requestBody: string,
	session: LanguageModelSession,
): Promise<string> {
	let request: ChatRequest;
	try {
		request = JSON.parse(requestBody) as ChatRequest;
	} catch {
		return JSON.stringify({ error: { type: "invalid_request", message: "invalid JSON body" } });
	}
	const text = await session.prompt(chatRequestToPrompt(request));
	return JSON.stringify({
		id: "chatcmpl-chrome-local",
		object: "chat.completion",
		model: request.model ?? "chrome-local",
		choices: [
			{
				index: 0,
				message: { role: "assistant", content: text },
				finish_reason: "stop",
			},
		],
	});
}

/**
 * MANUAL-TESTING STAND-IN ONLY — not a real model. Returns a fake
 * `LanguageModelSession` whose `prompt()` yields a deterministic canned reply
 * instead of calling Chrome's on-device model. This exists so the real terminal
 * + pi TUI flow can be driven end-to-end on hosts where `window.LanguageModel`
 * cannot be provisioned (e.g. a headless Linux box). It is OPT-IN and is NEVER
 * used by the strict real-model gates (`verify:real-pi-model` /
 * `verify:real-language-model`), which require a genuine `window.LanguageModel`
 * answer and fail honestly when it is absent. Callers that use this must report
 * `usedRealLanguageModel: false`.
 */
export function createMockLanguageModelSession(
	reply?: (prompt: string) => string,
): LanguageModelSession {
	return {
		async prompt(input: string): Promise<string> {
			if (reply) return reply(input);
			const lastUser = input
				.split("\n")
				.reverse()
				.find((line) => line.trimStart().startsWith("user:"));
			const echo = (lastUser ?? input).replace(/^\s*user:\s*/, "").trim();
			return (
				"[MOCK on-device model — not real] No real window.LanguageModel is " +
				"available on this host, so this is a placeholder reply for manual " +
				`testing. You said: ${echo || "(nothing)"}`
			);
		},
	};
}

/** Create a `LanguageModelSession` from the real Chrome `LanguageModel` global, or
 * return null if it is unavailable (CI / unsupported). Probed lazily so the bundle
 * loads without it. */
export async function createChromeLanguageModelSession(
	options: ChromeLanguageModelSessionOptions = {},
): Promise<LanguageModelSession | null> {
	const LanguageModel = getLanguageModelGlobal();
	if (!LanguageModel) return null;
	const availability = await getChromeLanguageModelAvailability();
	if (
		availability !== "available" &&
		!(options.allowDownload && (availability === "downloadable" || availability === "downloading"))
	) {
		return null;
	}
	return LanguageModel.create({
		...LANGUAGE_MODEL_OPTIONS,
		signal: options.signal,
		monitor(monitor: EventTarget) {
			monitor.addEventListener("downloadprogress", (event) => {
				const progress = Number((event as { loaded?: unknown }).loaded ?? 0);
				options.onDownloadProgress?.(progress);
			});
		},
	});
}
