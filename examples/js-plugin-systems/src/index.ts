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
	const input = "hello from plugin land";
	const result = await runtime.javascript.evaluate<{
		manifest: { name: string; version: string };
		output: string;
		networkBlocked: boolean;
	}>(`(async () => {
		function transform(input, prefix = "") {
			return prefix + String(input)
				.split(/\\s+/)
				.filter(Boolean)
				.map((word) => word[0].toUpperCase() + word.slice(1).toLowerCase())
				.join(" ");
		}
		let networkBlocked = false;
		try { await fetch("http://example.com"); } catch { networkBlocked = true; }
		return {
			manifest: { name: "title-case", version: "1.0.0" },
			output: transform(${JSON.stringify(input)}, "Plugin says: "),
			networkBlocked,
		};
	})()`);

	console.log(result);
} finally {
	await runtime.dispose();
}
