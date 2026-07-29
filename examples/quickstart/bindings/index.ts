import { AgentOs, binding, bindings } from "@rivet-dev/agentos";
import { z } from "zod";

const weatherBindings = bindings({
	name: "weather",
	description: "Look up weather information for cities.",
	bindings: {
		get: binding({
			description: "Get the current weather for a city.",
			inputSchema: z.object({
				city: z.string().describe("City name (e.g. 'London')."),
			}),
			execute: async ({ city }) => ({
				city,
				temperature: 18,
				conditions: "partly cloudy",
				humidity: 65,
			}),
			examples: [
				{ description: "Get London weather", input: { city: "London" } },
			],
		}),
	},
});

const calculatorBindings = bindings({
	name: "calc",
	description: "Simple calculator operations.",
	bindings: {
		add: binding({
			description: "Add two numbers.",
			inputSchema: z.object({ a: z.number(), b: z.number() }),
			execute: ({ a, b }) => ({ result: a + b }),
		}),
	},
});

const vm = await AgentOs.create({
	bindings: [weatherBindings, calculatorBindings],
	permissions: {
		fs: "allow",
		network: "allow",
		childProcess: "allow",
		env: "allow",
		binding: "allow",
	},
});

try {
	const weather = await vm.process.exec("agentos-weather get --city London");
	console.log("Weather:", weather.stdout.trim());

	const sum = await vm.process.exec("agentos-calc add --a 10 --b 32");
	console.log("Sum:", sum.stdout.trim());
} finally {
	await vm.dispose();
}
