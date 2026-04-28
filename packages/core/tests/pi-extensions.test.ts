import { resolve } from "node:path";
import pi from "@rivet-dev/agent-os-pi";
import { describe, expect, test } from "vitest";
import { AgentOs } from "../src/agent-os.js";
import {
	createAnthropicFixture,
	startLlmock,
	stopLlmock,
} from "./helpers/llmock-helper.js";

const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");
const HOME_DIR = "/home/user";
const WORKSPACE_DIR = `${HOME_DIR}/workspace`;
const PI_AGENT_DIR = `${HOME_DIR}/.pi/agent`;
const EXTENSIONS_DIR = `${PI_AGENT_DIR}/extensions`;
const EXTENSION_MARKER = "PI_EXTENSION_MARKER:custom-system-prompt";
const EXPECTED_REPLY = "EXTENSION_OK: 4";

function getRequestBody(req: unknown): Record<string, unknown> {
	const direct = req as Record<string, unknown>;
	const body = direct.body;
	return body && typeof body === "object"
		? (body as Record<string, unknown>)
		: direct;
}

function requestIncludesExtensionMarker(req: unknown): boolean {
	return JSON.stringify(getRequestBody(req)).includes(EXTENSION_MARKER);
}

async function createPiVm(mockUrl: string): Promise<AgentOs> {
	return AgentOs.create({
		loopbackExemptPorts: [Number(new URL(mockUrl).port)],
		moduleAccessCwd: MODULE_ACCESS_CWD,
		software: [pi],
	});
}

async function seedPiConfig(vm: AgentOs, mockUrl: string): Promise<void> {
	await vm.mkdir(EXTENSIONS_DIR, { recursive: true });
	await vm.mkdir(WORKSPACE_DIR, { recursive: true });
	await vm.writeFile(
		`${PI_AGENT_DIR}/models.json`,
		JSON.stringify(
			{
				providers: {
					anthropic: {
						baseUrl: mockUrl,
						apiKey: "mock-key",
					},
				},
			},
			null,
			2,
		),
	);
	await vm.writeFile(
		`${EXTENSIONS_DIR}/custom-greeting.js`,
		`
export default function(pi) {
	pi.on("before_agent_start", async (event) => {
		return {
			systemPrompt: event.systemPrompt + "\\n\\n${EXTENSION_MARKER}"
		};
	});
}
`.trimStart(),
	);
}

describe("Pi extensions quickstart truth test", () => {
	test("loads ~/.pi/agent/extensions hooks and sends their prompt changes to llmock", async () => {
		const { mock, url } = await startLlmock([
			createAnthropicFixture(
				{
					predicate: requestIncludesExtensionMarker,
				},
				{ content: EXPECTED_REPLY },
			),
			createAnthropicFixture({}, { content: "MISSING_EXTENSION_MARKER" }),
		]);
		const vm = await createPiVm(url);

		let sessionId: string | undefined;
		try {
			await seedPiConfig(vm, url);
			sessionId = (
				await vm.createSession("pi", {
					cwd: WORKSPACE_DIR,
					env: {
						HOME: HOME_DIR,
						ANTHROPIC_API_KEY: "mock-key",
						ANTHROPIC_BASE_URL: url,
					},
				})
			).sessionId;

			const { response, text } = await vm.prompt(
				sessionId,
				"What is 2 + 2? Reply with just the number.",
			);

			expect(response.error).toBeUndefined();
			expect(text).toContain(EXPECTED_REPLY);
			expect(mock.getRequests().length).toBeGreaterThanOrEqual(1);
			expect(
				mock.getRequests().some(requestIncludesExtensionMarker),
			).toBe(true);
		} finally {
			if (sessionId) {
				vm.closeSession(sessionId);
			}
			await vm.dispose();
			await stopLlmock(mock);
		}
	}, 120_000);
});
