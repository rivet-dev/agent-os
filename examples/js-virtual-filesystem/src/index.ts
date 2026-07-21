import { AgentOs } from "@rivet-dev/agentos";

const runtime = await AgentOs.create();

try {
	await runtime.filesystem.writeFile(
		"/workspace/seed.json",
		JSON.stringify({ ok: true }),
	);
	await runtime.filesystem.writeFile(
		"/workspace/note.txt",
		"written from the host\n",
	);
	const result = await runtime.javascript.evaluate<{
		ok: boolean;
		note: string;
	}>(`
		(async () => {
			const { readFile } = await import("node:fs/promises");
			const seed = JSON.parse(await readFile("/workspace/seed.json", "utf8"));
			const note = (await readFile("/workspace/note.txt", "utf8")).trim();
			return { ok: seed.ok, note };
		})()
	`);
	console.log(result);

	const bytes = await runtime.filesystem.readFile("/workspace/seed.json");
	console.log(new TextDecoder().decode(bytes));
} finally {
	await runtime.dispose();
}
