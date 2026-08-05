import { PassThrough } from "node:stream";
import { describe, expect, test } from "vitest";
import {
	SidecarProcessError,
	SidecarProcessExited,
	StdioSidecarProcess,
} from "../src/process.js";

describe("stdio sidecar process monitor", () => {
	test("formats process exit errors with stderr", () => {
		const error = new SidecarProcessExited({
			exitCode: 7,
			signal: null,
			stderr: "boom",
		});

		expect(error.message).toBe(
			"sidecar process exited with code 7\nstderr:\nboom",
		);
		expect(error.exitCode).toBe(7);
		expect(error.signal).toBeNull();
		expect(error.stderr).toBe("boom");
	});

	test("formats process spawn errors with stderr", () => {
		const error = new SidecarProcessError(new Error("missing"), "nope");

		expect(error.message).toBe("sidecar process error: missing\nstderr:\nnope");
		expect(error.childError.message).toBe("missing");
		expect(error.stderr).toBe("nope");
	});

	test("captures stderr chunks from a child-like process", async () => {
		const child = {
			stdin: new PassThrough(),
			stdout: new PassThrough(),
			stderr: new PassThrough(),
			stdio: [null, null, null, new PassThrough()],
			exitCode: null,
			signalCode: null,
			on() {
				return this;
			},
			off() {
				return this;
			},
		} as unknown as Parameters<typeof StdioSidecarProcess.fromChild>[0];
		const monitor = StdioSidecarProcess.fromChild(child);

		child.stderr.write("first");
		child.stderr.write(Buffer.from(" second"));

		expect(monitor.stderrText()).toBe("first second");
	});
});
