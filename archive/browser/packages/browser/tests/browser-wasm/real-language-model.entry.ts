import {
	createChromeLanguageModelSession,
	getChromeLanguageModelAvailability,
	handleChatCompletion,
} from "../../src/chrome-llm-adapter.js";

interface RealLanguageModelResult {
	ok: boolean;
	usedRealLanguageModel: boolean;
	availability?: string;
	answer?: string;
	error?: string;
	downloadProgress?: number[];
}

const MODEL_CREATE_TIMEOUT_MS = 8 * 60_000;

declare global {
	interface Window {
		__realLanguageModel?: {
			run(prompt: string): Promise<RealLanguageModelResult>;
			prepareUserActivatedRun(prompt: string): void;
			userActivatedResult(): Promise<RealLanguageModelResult>;
		};
	}
}

async function availability(): Promise<string> {
	return getChromeLanguageModelAvailability();
}

async function run(
	prompt: string,
	options: { allowDownload?: boolean } = {},
): Promise<RealLanguageModelResult> {
	const modelAvailability = await availability();
	if (
		modelAvailability !== "available" &&
		!(options.allowDownload && (modelAvailability === "downloadable" || modelAvailability === "downloading"))
	) {
		return {
			ok: false,
			usedRealLanguageModel: false,
			availability: modelAvailability,
			error: `Chrome LanguageModel is not available (${modelAvailability})`,
		};
	}

	const downloadProgress: number[] = [];
	const controller = new AbortController();
	const timeout = options.allowDownload
		? setTimeout(
				() =>
					controller.abort(
						new Error(
							`LanguageModel.create() timed out after ${MODEL_CREATE_TIMEOUT_MS}ms`,
						),
					),
				MODEL_CREATE_TIMEOUT_MS,
			)
		: undefined;
	let session: Awaited<ReturnType<typeof createChromeLanguageModelSession>>;
	try {
		session = await createChromeLanguageModelSession({
			allowDownload: options.allowDownload,
			onDownloadProgress(progress) {
				downloadProgress.push(progress);
			},
			signal: controller.signal,
		});
	} catch (error) {
		return {
			ok: false,
			usedRealLanguageModel: false,
			availability: modelAvailability,
			downloadProgress,
			error: `Chrome LanguageModel session creation failed: ${
				error instanceof Error ? error.message : String(error)
			}`,
		};
	} finally {
		if (timeout) clearTimeout(timeout);
	}
	if (!session) {
		return {
			ok: false,
			usedRealLanguageModel: false,
			availability: modelAvailability,
			downloadProgress,
			error: "Chrome LanguageModel reported available but session creation returned null",
		};
	}

	const body = JSON.stringify({
		model: "chrome-local",
		messages: [{ role: "user", content: prompt }],
	});
	const completion = JSON.parse(await handleChatCompletion(body, session)) as {
		choices?: Array<{ message?: { content?: string } }>;
	};
	const answer = completion.choices?.[0]?.message?.content;
	if (!answer) {
		return {
			ok: false,
			usedRealLanguageModel: true,
			availability: modelAvailability,
			downloadProgress,
			error: "Chrome LanguageModel returned an empty completion",
		};
	}
	return {
		ok: true,
		usedRealLanguageModel: true,
		availability: modelAvailability,
		answer,
		downloadProgress,
	};
}

let userActivatedPrompt = "";
let userActivatedPromise: Promise<RealLanguageModelResult> | undefined;
const button = document.createElement("button");
button.id = "run-language-model";
button.textContent = "Run";
button.addEventListener("click", () => {
	userActivatedPromise = run(userActivatedPrompt, { allowDownload: true });
});
document.body.appendChild(button);

window.__realLanguageModel = {
	run,
	prepareUserActivatedRun(prompt: string) {
		userActivatedPrompt = prompt;
		userActivatedPromise = undefined;
	},
	userActivatedResult() {
		if (!userActivatedPromise) {
			return Promise.resolve({
				ok: false,
				usedRealLanguageModel: false,
				error: "user-activated run was not started",
			});
		}
		return userActivatedPromise;
	},
};

const status = document.getElementById("status");
if (status) status.textContent = "ready";
