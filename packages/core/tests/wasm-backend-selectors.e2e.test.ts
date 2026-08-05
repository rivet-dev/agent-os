import { coreutils } from "@agentos-software/common";
import { afterEach, describe, expect, test, vi } from "vitest";
import { AgentOs } from "../src/index.js";

vi.setConfig({ testTimeout: 120_000 });

const backends = ["v8", "wasmtime", "wasmtime-threads"] as const;
const liveVms: AgentOs[] = [];

afterEach(async () => {
	await Promise.all(liveVms.splice(0).map((vm) => vm.dispose()));
});

describe("public standalone WASM backend selectors", () => {
	test.each(backends)("executes shell children through %s", async (backend) => {
		const vm = await AgentOs.create({
			wasmBackend: backend,
			defaultSoftware: false,
			software: [coreutils],
			permissions: {
				fs: "allow",
				network: "allow",
				childProcess: "allow",
				process: "allow",
				env: "allow",
				binding: "allow",
			},
		});
		liveVms.push(vm);

		const result = await vm.exec(
			`printf 'selector-${backend}\\n' | tr '[:lower:]' '[:upper:]'`,
		);
		expect(result.exitCode, result.stderr).toBe(0);
		expect(result.stdout).toBe(`SELECTOR-${backend.toUpperCase()}\n`);
	});
});
