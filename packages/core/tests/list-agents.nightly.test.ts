import {
	chmodSync,
	mkdirSync,
	mkdtempSync,
	rmSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vitest";
import { AgentOs } from "../src/agent-os.js";

// `listAgents()` is a sidecar ACP RPC: the sidecar enumerates the projected
// `/opt/agentos` packages (those whose `agentos-package.json` carries an
// `agent.acpEntrypoint`). The client parses NO manifests and makes no npm/
// node_modules assumptions. A bare `create()` projects no agent packages.
describe("listAgents()", () => {
	let vm: AgentOs;
	let root: string;

	beforeAll(async () => {
		root = mkdtempSync(join(tmpdir(), "agentos-list-agents-"));
		// Two self-contained /opt/agentos agent packages projected via `software`.
		for (const name of ["alpha-agent", "beta-agent"]) {
			const pkgDir = join(root, name);
			mkdirSync(join(pkgDir, "bin"), { recursive: true });
			writeFileSync(
				join(pkgDir, "package.json"),
				JSON.stringify({ name, version: "1.0.0" }, null, 2),
			);
			writeFileSync(
				join(pkgDir, "agentos-package.json"),
				JSON.stringify(
					{ name, version: "1.0.0", agent: { acpEntrypoint: `${name}-acp` } },
					null,
					2,
				),
			);
			const binPath = join(pkgDir, "bin", `${name}-acp`);
			writeFileSync(binPath, "#!/usr/bin/env node\n");
			chmodSync(binPath, 0o755);
		}
		vm = await AgentOs.create({
			defaultSoftware: false,
			software: [join(root, "alpha-agent"), join(root, "beta-agent")],
		});
	}, 60_000);

	afterAll(async () => {
		await vm?.dispose();
		if (root) rmSync(root, { recursive: true, force: true });
	});

	test("lists the projected agent packages, sorted by id", async () => {
		const agents = await vm.listAgents();
		const ids = agents.map((a) => a.id);
		expect(ids).toContain("alpha-agent");
		expect(ids).toContain("beta-agent");
	});

	test("every projected agent package is installed", async () => {
		const agents = await vm.listAgents();
		for (const agent of agents) {
			expect(agent.installed).toBe(true);
		}
	});
});
