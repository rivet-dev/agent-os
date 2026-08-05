import { describe, expect, it } from "vitest";
import {
	chatRequestToPrompt,
	handleChatCompletion,
	type LanguageModelSession,
} from "../../src/chrome-llm-adapter.js";

const SENTINEL = "PONG_FROM_CHROME_LLM";

/** Mock on-device model: echoes a fixed sentinel (the `llmock` precedent), so the
 * inference path is deterministically assertable end-to-end. */
function mockModel(reply = SENTINEL): LanguageModelSession {
	return { prompt: async () => reply };
}

describe("chatRequestToPrompt", () => {
	it("flattens OpenAI messages into a prompt", () => {
		const prompt = chatRequestToPrompt({
			messages: [
				{ role: "system", content: "be terse" },
				{ role: "user", content: "hello" },
			],
		});
		expect(prompt).toBe("system: be terse\nuser: hello");
	});

	it("handles Anthropic top-level system + array content parts", () => {
		const prompt = chatRequestToPrompt({
			system: "sys",
			messages: [{ role: "user", content: [{ type: "text", text: "hi" }, { type: "text", text: "!" }] }],
		});
		expect(prompt).toBe("system: sys\nuser: hi!");
	});

	it("passes a bare prompt through", () => {
		expect(chatRequestToPrompt({ prompt: "just this" })).toBe("just this");
	});
});

describe("handleChatCompletion", () => {
	it("returns an OpenAI-shaped completion carrying the model reply", async () => {
		const body = JSON.stringify({ model: "m", messages: [{ role: "user", content: "ping" }] });
		const out = JSON.parse(await handleChatCompletion(body, mockModel()));
		expect(out.object).toBe("chat.completion");
		expect(out.choices[0].message.role).toBe("assistant");
		expect(out.choices[0].message.content).toBe(SENTINEL);
		expect(out.choices[0].finish_reason).toBe("stop");
	});

	it("surfaces an error for an invalid body without calling the model", async () => {
		let called = false;
		const out = JSON.parse(
			await handleChatCompletion("not json", { prompt: async () => ((called = true), "x") }),
		);
		expect(out.error?.type).toBe("invalid_request");
		expect(called).toBe(false);
	});

	it("feeds the flattened prompt to the model", async () => {
		let seen = "";
		const body = JSON.stringify({ messages: [{ role: "user", content: "what is 2+2" }] });
		await handleChatCompletion(body, { prompt: async (p) => ((seen = p), "4") });
		expect(seen).toBe("user: what is 2+2");
	});
});
