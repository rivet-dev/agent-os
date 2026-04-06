import type { AgentOs } from "../../../src/agent-os.js";

const PI_HOME = "/home/user";
export const PI_AGENT_DIR = `${PI_HOME}/.pi/agent`;

export const PI_TEST_HOME = PI_HOME;

export async function writePiAnthropicModelsOverride(
	vm: AgentOs,
	baseUrl: string,
): Promise<void> {
	await vm.mkdir(PI_AGENT_DIR, { recursive: true });
	await vm.writeFile(
		`${PI_AGENT_DIR}/auth.json`,
		JSON.stringify(
			{
				anthropic: {
					type: "api_key",
					key: "mock-key",
				},
			},
			null,
			2,
		),
	);
	await vm.writeFile(
		`${PI_AGENT_DIR}/models.json`,
		JSON.stringify(
			{
				providers: {
					anthropic: {
						baseUrl,
						apiKey: "mock-key",
					},
				},
			},
			null,
			2,
		),
	);
}
