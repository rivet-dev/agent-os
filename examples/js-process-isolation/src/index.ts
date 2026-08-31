import { AgentOs } from "@rivet-dev/agentos-core";

const runtimeA = await AgentOs.create();
const runtimeB = await AgentOs.create();

try {
	await runtimeA.javascript.execute(`
		import { writeFileSync } from "node:fs";
		writeFileSync("/tmp/value.txt", "A");
	`);
	await runtimeB.javascript.execute(`
		import { writeFileSync } from "node:fs";
		writeFileSync("/tmp/value.txt", "B");
	`);

	const read = `(async () => {
		const { readFile } = await import("node:fs/promises");
		return readFile("/tmp/value.txt", "utf8");
	})()`;
	const [valueA, valueB] = await Promise.all([
		runtimeA.javascript.evaluate<string>(read),
		runtimeB.javascript.evaluate<string>(read),
	]);
	console.log({ valueA, valueB });

	// Each evaluate() launches a fresh process, so guest globals do not persist.
	const globalValue = `(() => {
		globalThis.counter = (globalThis.counter ?? 0) + 1;
		return globalThis.counter;
	})()`;
	console.log(await runtimeA.javascript.evaluate<number>(globalValue));
	console.log(await runtimeA.javascript.evaluate<number>(globalValue));
} finally {
	await runtimeA.dispose();
	await runtimeB.dispose();
}
