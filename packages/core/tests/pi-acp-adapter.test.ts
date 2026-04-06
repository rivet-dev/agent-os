import { resolve } from "node:path";
import type { LLMock } from "@copilotkit/llmock";
import piCli from "@rivet-dev/agent-os-pi-cli";
import {
	afterAll,
	afterEach,
	beforeAll,
	beforeEach,
	describe,
	expect,
	test,
} from "vitest";
import { AgentOs } from "../src/agent-os.js";
import {
	DEFAULT_TEXT_FIXTURE,
	startLlmock,
	stopLlmock,
} from "./helpers/llmock-helper.js";

const MODULE_ACCESS_CWD = resolve(
	import.meta.dirname,
	"../../../examples/quickstart",
);

describe("pi-acp adapter createSession", () => {
	let vm: AgentOs;
	let mock: LLMock;
	let mockUrl: string;
	let mockPort: number;

	beforeAll(async () => {
		const result = await startLlmock([DEFAULT_TEXT_FIXTURE]);
		mock = result.mock;
		mockUrl = result.url;
		mockPort = Number(new URL(result.url).port);
	});

	afterAll(async () => {
		await stopLlmock(mock);
	});

	beforeEach(async () => {
		vm = await AgentOs.create({
			loopbackExemptPorts: [mockPort],
			moduleAccessCwd: MODULE_ACCESS_CWD,
			software: [piCli],
		});
	});

	afterEach(async () => {
		await vm.dispose();
	});

	test("createSession('pi-cli') resolves pi-acp relative to the software package", async () => {
		const { sessionId } = await vm.createSession("pi-cli", {
			env: {
				ANTHROPIC_API_KEY: "mock-key",
				ANTHROPIC_BASE_URL: mockUrl,
			},
		});

		try {
			const { response, text } = await vm.prompt(
				sessionId,
				"Reply with exactly: Hello from llmock",
			);

			expect(response.error).toBeUndefined();
			expect((response.result as { stopReason?: string }).stopReason).toBe(
				"end_turn",
			);
			expect(text).toContain("Hello from llmock");
			expect(
				vm
					.listProcesses()
					.some(
						(process) =>
							process.running &&
							process.command === "node" &&
							process.args.some((arg) => arg.includes("pi-acp")),
					),
			).toBe(true);
		} finally {
			vm.closeSession(sessionId);
		}
	}, 90_000);
});
