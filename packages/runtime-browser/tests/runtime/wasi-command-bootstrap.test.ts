import { describe, expect, it } from "vitest";
import { createWasiCommandBootstrapScript } from "../../src/wasi-command-bootstrap.js";

describe("wasi command bootstrap", () => {
	it("generates a production browser guest launcher for the WASI command host", () => {
		const source = createWasiCommandBootstrapScript({
			commandSource: "/commands/sh",
			command: "sh",
			args: ["-i"],
			commands: {
				"/bin/echo": "/commands/echo",
				"/bin/ls": "/commands/ls",
			},
			env: {
				PATH: "/bin:/usr/bin",
				TERM: "xterm-256color",
			},
			cwd: "/",
			bootMessage: "BOOT",
			errorMessagePrefix: "ERR:",
		});

		expect(source).toContain('require("node:wasi")');
		expect(source).toContain('require("agentos:wasi-command-host")');
		expect(source).toContain('const commandSource = "/commands/sh";');
		expect(source).toContain("/commands/sh");
		expect(source).toContain("/commands/echo");
		expect(source).toContain("/commands/ls");
		expect(source).toContain('args: ["sh","-i"]');
		expect(source).toContain("commandHost.installBlockingStdin(process)");
		expect(source).toContain("commandHost.setParentWasi(wasi)");
		expect(source).toContain("commandHost.setMemory(instance.exports.memory)");
		expect(source).toContain("ERR:");
	});
});
