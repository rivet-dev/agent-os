import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import {
	copyFileSync,
	mkdirSync,
	mkdtempSync,
	readFileSync,
	rmSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const generator = fileURLToPath(new URL("./gen-registry.mjs", import.meta.url));

test("classifies the current agent descriptor as an agent registry entry", () => {
	const root = mkdtempSync(join(tmpdir(), "agentos-registry-generator-"));
	try {
		const website = join(root, "website");
		mkdirSync(join(website, "scripts"), { recursive: true });
		mkdirSync(join(website, "src", "generated"), { recursive: true });
		copyFileSync(generator, join(website, "scripts", "gen-registry.mjs"));

		const packageRoot = join(root, "software", "example-agent");
		mkdirSync(packageRoot, { recursive: true });
		writeFileSync(
			join(packageRoot, "agentos-package.json"),
			JSON.stringify({
				name: "example",
				agent: { acpEntrypoint: "example-acp" },
				registry: {
					category: "agents",
					title: "Example",
					description: "Example agent registry entry.",
				},
			}),
		);
		writeFileSync(
			join(packageRoot, "package.json"),
			JSON.stringify({ name: "@agentos-software/example-agent" }),
		);

		execFileSync(process.execPath, [join(website, "scripts", "gen-registry.mjs")]);
		const generated = JSON.parse(
			readFileSync(join(website, "src", "generated", "registry.json"), "utf8"),
		);
		assert.deepEqual(generated.entries, [
			{
				slug: "example-agent",
				title: "Example",
				description: "Example agent registry entry.",
				types: ["agent"],
				category: "agents",
				priority: 0,
				package: "@agentos-software/example-agent",
				status: "available",
				docsHref: "/docs/agents/example",
				agentId: "example",
			},
		]);
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
});
