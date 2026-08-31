import { AgentOs, type Bindings } from "@rivet-dev/agentos-core";
import { z } from "zod";

// Binding groups are defined exactly as they are for the actor. Pass them to
// AgentOs.create() and `execute` runs in this host process.
const weather: Bindings = {
	name: "weather",
	description: "Weather data bindings",
	bindings: {
		forecast: {
			description: "Get the weather forecast for a city",
			inputSchema: z.object({ city: z.string().describe("City name") }),
			execute: async (input: { city: string }) => ({
				city: input.city,
				temperature: 22,
			}),
		},
	},
};

const vm = await AgentOs.create({ bindings: [weather] });

// The agent calls it as `agentos-weather forecast --city Paris`.
const result = await vm.process.exec("agentos-weather forecast --city Paris");
console.log(result.stdout);
await vm.dispose();
