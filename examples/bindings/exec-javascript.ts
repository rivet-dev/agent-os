import { AgentOs, type Bindings } from "@rivet-dev/agentos";
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

// The collection is projected into the VM as an `agentos-weather` command.
const runtime = await AgentOs.create({ bindings: [weather] });

try {
	const result = await runtime.javascript.execute(
		`
		import { execFile } from "node:child_process";
		import { promisify } from "node:util";

		const run = promisify(execFile);
		const { stdout } = await run("agentos-weather", ["forecast", "--city", "Paris"]);
		console.log(JSON.parse(stdout).result);
		`,
		{ output: { capture: "all" } },
	);
	console.log(result.outcome === "succeeded" ? result.stdout : result.error);
} finally {
	await runtime.dispose();
}
