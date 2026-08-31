import { AgentOs, type Bindings } from "@rivet-dev/agentos-core";
import { z } from "zod";

// The handler runs on the host, so the API key never enters the VM.
const weather: Bindings = {
	name: "weather",
	description: "Weather data bindings",
	bindings: {
		forecast: {
			description: "Get the weather forecast for a city",
			inputSchema: z.object({ city: z.string() }),
			execute: async ({ city }: { city: string }) => {
				const res = await fetch(
					`https://api.weather.example/forecast?city=${city}&key=${process.env.WEATHER_API_KEY}`,
				);
				return res.json();
			},
		},
	},
};

// The collection is projected into the VM as an `agentos-weather` command, so
// it composes with pipes and redirects like any other program.
const runtime = await AgentOs.create({ bindings: [weather] });

try {
	const result = await runtime.process.exec(
		"agentos-weather forecast --city Paris > /workspace/forecast.json && wc -c < /workspace/forecast.json",
		{ output: { capture: "all" } },
	);
	console.log(result.outcome === "succeeded" ? result.stdout : result.error);
} finally {
	await runtime.dispose();
}
