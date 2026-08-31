import { AgentOs } from "@rivet-dev/agentos-core";

const runtime = await AgentOs.create({
	permissions: {
		fs: "allow",
		network: "deny",
		childProcess: "allow",
		process: "allow",
		env: "allow",
	},
});

try {
	const result = await runtime.javascript.evaluate<{
		fileContents: string;
		networkBlocked: boolean;
	}>(`(async () => {
		const { writeFileSync, readFileSync } = await import("node:fs");
		writeFileSync("/workspace/note.txt", "inside the VM");
		let networkBlocked = false;
		try { await fetch("https://example.com"); } catch { networkBlocked = true; }
		return {
			fileContents: readFileSync("/workspace/note.txt", "utf8"),
			networkBlocked,
		};
	})()`);

	if (result.outcome !== "succeeded") throw new Error(result.error.message);
	console.log(result.value);
} finally {
	await runtime.dispose();
}
