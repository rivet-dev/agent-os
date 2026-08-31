// docs:start bindings
import { AgentOs, binding, bindings } from "@rivet-dev/agentos-core";
import { z } from "zod";

const toolBindings = bindings({
	name: "tools",
	description: "Curated host capabilities for generated code.",
	bindings: {
		weather: binding({
			description: "Look up a city's temperature.",
			inputSchema: z.object({ city: z.string() }),
			execute: ({ city }) => ({
				city,
				tempF: city === "San Francisco" ? 61 : 75,
			}),
		}),
	},
});

const runtime = await AgentOs.create({
	bindings: [toolBindings],
	permissions: { binding: "allow" },
});
// docs:end bindings

// docs:start generated-code
const llmGeneratedExpression = `(async () => {
  const { execFileSync } = await import("node:child_process");
  const call = (city) => JSON.parse(
    execFileSync("agentos-tools", ["weather", "--city", city], {
      encoding: "utf8",
    }),
  );
  const [sf, tokyo] = await Promise.all([
    Promise.resolve(call("San Francisco")),
    Promise.resolve(call("Tokyo")),
  ]);
  return {
    sanFrancisco: sf,
    tokyo,
    differenceF: Math.abs(sf.tempF - tokyo.tempF),
  };
})()`;
// docs:end generated-code

// docs:start run
try {
	const result = await runtime.javascript.evaluate<{
		sanFrancisco: { city: string; tempF: number };
		tokyo: { city: string; tempF: number };
		differenceF: number;
	}>(llmGeneratedExpression, { timeoutMs: 5_000 });
	console.log(result);
} finally {
	await runtime.dispose();
}
// docs:end run
