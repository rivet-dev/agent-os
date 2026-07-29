// Nightly: projects the complete registry command bundle.
import { existsSync } from "node:fs";
import { resolve } from "node:path";
import codex from "@agentos-software/codex-cli";
import { describe, expect, test } from "vitest";
import { AgentOs } from "../src/agent-os.js";
import {
	type ResponsesFixture,
	startResponsesMock,
} from "./helpers/openai-responses-mock.js";
import { REGISTRY_SOFTWARE } from "./helpers/registry-commands.js";

const codexConfig = `[features]
# Shell snapshots spawn a pre-turn shell subprocess. The real turn coverage
# below does not need that optional context, and disabling it keeps the WASI VM
# focused on the codex-core model/tool path under test.
shell_snapshot = false
`;

// The real codex-exec WASM binary is generated from the external Codex fork by
// `make -C toolchain codex-required`; the ordinary repository build and CI
// intentionally do not produce it. Keep the full-turn coverage mandatory when
// that artifact is present without making unrelated CI fail on a missing tool.
const hasCodexExecArtifact = existsSync(
	resolve(import.meta.dirname, "../../../software/codex/wasm/codex-exec"),
);

/**
 * Run a single `codex-exec --session-turn` against a mock OpenAI Responses server, driving the real
 * codex-core agent inside the VM. `start` is the EE protocol start message; `stdinTail` is any
 * additional newline-JSON written after it (e.g. a permission response).
 */
async function runSessionTurn(
	fixtures: ResponsesFixture[],
	start: Record<string, unknown>,
	stdinTail = "",
) {
	const mock = await startResponsesMock(fixtures);
	const vm = await AgentOs.create({
		loopbackExemptPorts: [mock.port],
		software: [codex as any, ...(REGISTRY_SOFTWARE as any[])] as any,
	});
	try {
		const stdin =
			JSON.stringify({
				type: "start",
				cwd: "/root",
				model: "gpt-5",
				...start,
			}) +
			"\n" +
			stdinTail;
		await vm.execArgv("mkdir", ["-p", "/home/agentos/.codex"]);
		await vm.writeFile(
			"/home/agentos/.codex/config.toml",
			new TextEncoder().encode(codexConfig),
		);
		const r = await vm.execArgv("codex-exec", ["--session-turn"], {
			timeout: 45000,
			stdin,
			env: {
				HOME: "/home/agentos",
				CODEX_HOME: "/home/agentos/.codex",
				OPENAI_API_KEY: "mock-key",
				OPENAI_BASE_URL: `${mock.url}/v1`,
			},
		} as any);
		return {
			stdout: r.stdout ?? "",
			stderr: r.stderr ?? "",
			exitCode: r.exitCode,
			requests: mock.requests,
		};
	} finally {
		await vm.dispose();
		await mock.stop();
	}
}

const finalText = (text: string): ResponsesFixture => ({
	name: "final-text",
	predicate: () => true,
	response: {
		id: "resp_text",
		output: [
			{
				type: "message",
				role: "assistant",
				content: [{ type: "output_text", text }],
			},
		],
	},
});

describe.skipIf(!hasCodexExecArtifact)(
	"codex full turn (real codex agent in the VM, mock OpenAI Responses)",
	() => {
		test("codex-exec --session-turn completes a model turn end-to-end", async () => {
			const { stdout, stderr, exitCode, requests } = await runSessionTurn(
				[finalText("hello from codex")],
				{
					prompt: "say hello",
				},
			);
			expect(stdout).toContain('"type":"start"');
			expect(
				requests.length,
				`codex-exec did not call mock Responses; exitCode=${exitCode}; stderr=${stderr}; stdout=${stdout}`,
			).toBeGreaterThan(0);
			// The engine must surface the assistant text as a text_delta — whether the
			// model streamed deltas or returned a single final AgentMessage — not just
			// reach `done`. (A prior `/(done|text_delta|error)/` regex passed on `done`
			// alone and masked a real gap where non-streamed responses emitted no text.)
			expect(stdout).toContain('"type":"text_delta"');
			expect(stdout).toContain("hello from codex");
			expect(stdout).toContain('"type":"done"');
		}, 70000);

		test("emits only the final unphased assistant snapshot", async () => {
			const { stdout, stderr, exitCode } = await runSessionTurn(
				[
					{
						name: "successive-assistant-snapshots",
						predicate: () => true,
						response: {
							id: "resp_successive_messages",
							output: ["first draft", "second draft", "final answer"].map(
								(text) => ({
									type: "message",
									role: "assistant",
									content: [{ type: "output_text", text }],
								}),
							),
						},
					},
				],
				{ prompt: "give one answer" },
			);
			const events = stdout
				.trim()
				.split("\n")
				.map((line) => JSON.parse(line) as { type: string; delta?: string });
			const text = events
				.filter((event) => event.type === "text_delta")
				.map((event) => event.delta ?? "")
				.join("");
			expect(
				text,
				`codex-exec emitted repeated snapshots; exitCode=${exitCode}; stderr=${stderr}; stdout=${stdout}`,
			).toBe("final answer");
		}, 70000);

		test("runs a shell tool call with on-request approval and reports tool_call updates", async () => {
			const sawToolOutput = (body: Record<string, unknown>) =>
				JSON.stringify(body).includes("function_call_output");
			const { stdout, stderr, exitCode } = await runSessionTurn(
				[
					// Turn 1: model asks to run a shell command.
					{
						name: "shell-call",
						predicate: (body) => !sawToolOutput(body),
						response: {
							id: "resp_shell",
							output: [
								{
									type: "function_call",
									name: "shell",
									arguments: JSON.stringify({
										command: ["echo", "agent-os-tool-ok"],
									}),
									call_id: "call_1",
								},
							],
						},
					},
					// Turn 2: after the tool output is sent back, model finishes.
					{
						name: "final-after-tool",
						predicate: (body) => sawToolOutput(body),
						response: {
							id: "resp_final",
							output: [
								{
									type: "message",
									role: "assistant",
									content: [{ type: "output_text", text: "ran the command" }],
								},
							],
						},
					},
				],
				{ prompt: "run echo agent-os-tool-ok" },
				// Approve the exec when the engine emits permission_request.
				`${JSON.stringify({ decision: "allow" })}\n`,
			);
			expect(
				stdout,
				`codex-exec did not report a tool call; exitCode=${exitCode}; stderr=${stderr}`,
			).toContain('"type":"tool_call_update"');
			expect(stdout).toContain('"type":"done"');
		}, 70000);

		test("shell tool runs a REAL subprocess with an observable filesystem side effect", async () => {
			// Proves codex's exec tool spawns a real subprocess via the agentos
			// host_process bridge (not a mocked/gated stub): the model asks to run a
			// shell command that WRITES A FILE, we approve it, and after the turn we
			// read that file back from the VM and assert its contents. Inlined (not
			// runSessionTurn) so the VM is still alive to verify the side effect.
			const sawToolOutput = (body: Record<string, unknown>) =>
				JSON.stringify(body).includes("function_call_output");
			const marker = "codex-subprocess-real-ok";
			const sourcePath = "/root/codex-source.txt";
			const outPath = "/root/codex-side-effect.txt";
			const mock = await startResponsesMock([
				{
					name: "shell-call",
					predicate: (body) => !sawToolOutput(body),
					response: {
						id: "resp_shell",
						output: [
							{
								type: "function_call",
								name: "shell",
								arguments: JSON.stringify({
									command: ["cp", "-v", sourcePath, outPath],
								}),
								call_id: "call_1",
							},
						],
					},
				},
				{
					name: "final-after-tool",
					predicate: (body) => sawToolOutput(body),
					response: {
						id: "resp_final",
						output: [
							{
								type: "message",
								role: "assistant",
								content: [{ type: "output_text", text: "wrote the file" }],
							},
						],
					},
				},
			]);
			const vm = await AgentOs.create({
				loopbackExemptPorts: [mock.port],
				software: [codex as any, ...(REGISTRY_SOFTWARE as any[])] as any,
			});
			try {
				const stdin =
					JSON.stringify({
						type: "start",
						cwd: "/root",
						model: "gpt-5",
						prompt: `write ${marker} to ${outPath}`,
					}) +
					"\n" +
					JSON.stringify({ decision: "allow" }) +
					"\n";
				await vm.execArgv("mkdir", ["-p", "/home/agentos/.codex"]);
				await vm.writeFile(
					"/home/agentos/.codex/config.toml",
					new TextEncoder().encode(codexConfig),
				);
				await vm.writeFile(sourcePath, new TextEncoder().encode(marker));
				const r = await vm.execArgv("codex-exec", ["--session-turn"], {
					timeout: 45000,
					stdin,
					env: {
						HOME: "/home/agentos",
						CODEX_HOME: "/home/agentos/.codex",
						OPENAI_API_KEY: "mock-key",
						OPENAI_BASE_URL: `${mock.url}/v1`,
					},
				} as any);
				expect(r.stdout ?? "").toContain('"type":"done"');
				// The observable side effect: the real subprocess wrote the file.
				const written = new TextDecoder().decode(await vm.readFile(outPath));
				expect(written).toBe(marker);
			} finally {
				await vm.dispose();
				await mock.stop();
			}
		}, 70000);

		test("replays adapter-supplied history on a resumed multi-turn session", async () => {
			const { stdout, requests } = await runSessionTurn(
				[finalText("the answer is 4")],
				{
					prompt: "and what did I ask before?",
					history: [
						{ role: "user", content: "what is 2+2?" },
						{ role: "assistant", content: "2+2 = 4" },
					],
				},
			);
			expect(stdout).toContain('"type":"done"');
			expect(requests.length).toBeGreaterThan(0);
			// The prior turns must be replayed to the model in the request the agent sends.
			const body = JSON.stringify(requests[0]);
			expect(body).toContain("what is 2+2?");
			expect(body).toContain("2+2 = 4");
		}, 70000);
	},
);
