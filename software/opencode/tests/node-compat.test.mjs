import assert from "node:assert/strict";
import { createServer } from "node:http";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { openCodeHashFast } from "../dist/node-compat.js";

test("matches OpenCode Hash.fast semantics for a remote skill cache download", async () => {
	const server = createServer((request, response) => {
		if (request.url === "/catalog/index.json") {
			response.setHeader("content-type", "application/json");
			response.end(JSON.stringify({ skills: [{ name: "deploy", files: ["SKILL.md"] }] }));
			return;
		}
		if (request.url === "/catalog/deploy/SKILL.md") {
			response.end("# Deploy\n");
			return;
		}
		response.statusCode = 404;
		response.end();
	});
	await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
	const address = server.address();
	assert.ok(address && typeof address !== "string");

	const cache = await mkdtemp(join(tmpdir(), "opencode-node-skill-"));
	try {
		const base = `http://127.0.0.1:${address.port}/catalog/`;
		assert.equal(openCodeHashFast(base), await hashFastReference(base));

		const index = await fetch(new URL("index.json", base)).then((response) => response.json());
		const skill = index.skills[0];
		const root = join(cache, "skills", openCodeHashFast(base), skill.name);
		await mkdir(root, { recursive: true });
		const content = await fetch(new URL(`${skill.name}/SKILL.md`, base)).then((response) => response.text());
		await writeFile(join(root, "SKILL.md"), content);

		assert.equal(await readFile(join(root, "SKILL.md"), "utf8"), "# Deploy\n");
		assert.equal(root, join(cache, "skills", await hashFastReference(base), "deploy"));
	} finally {
		await rm(cache, { recursive: true, force: true });
		await new Promise((resolve, reject) => server.close((error) => (error ? reject(error) : resolve())));
	}
});

async function hashFastReference(value) {
	const bytes = new TextEncoder().encode(value);
	const digest = await crypto.subtle.digest("SHA-1", bytes);
	return Buffer.from(digest).toString("hex");
}
