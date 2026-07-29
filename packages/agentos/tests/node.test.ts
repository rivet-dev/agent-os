import { EventEmitter } from "node:events";
import { describe, expect, test } from "vitest";
import {
	attachShell,
	type NodeShellConnection,
	NodeShellOutputLimitError,
} from "../src/node.js";

class TestInput extends EventEmitter {
	isTTY = true;
	isRaw = false;
	readonly rawModes: boolean[] = [];
	paused = true;

	setRawMode(enabled: boolean) {
		this.isRaw = enabled;
		this.rawModes.push(enabled);
	}

	pause() {
		this.paused = true;
		return this;
	}

	resume() {
		this.paused = false;
		return this;
	}
}

class TestOutput extends EventEmitter {
	isTTY = true;
	columns = 120;
	rows = 40;
	readonly chunks: Uint8Array[] = [];

	write(data: string | Uint8Array): boolean {
		this.chunks.push(
			typeof data === "string" ? Buffer.from(data, "utf8") : data,
		);
		return true;
	}

	text(): string {
		return Buffer.concat(this.chunks).toString("utf8");
	}
}

class TestConnection extends EventEmitter implements NodeShellConnection {
	readonly shellId = "shell-test";
	readonly writes: Array<string | Uint8Array> = [];
	readonly resizes: Array<[number, number]> = [];
	closeCount = 0;
	private resolveExit: ((exitCode: number) => void) | undefined;
	private readonly exitPromise = new Promise<number>((resolve) => {
		this.resolveExit = resolve;
	});

	async openShell() {
		this.emit("shellData", {
			shellId: this.shellId,
			data: Buffer.from("early output\n"),
		});
		return { shellId: this.shellId };
	}

	async writeShell(_shellId: string, data: string | Uint8Array) {
		this.writes.push(data);
	}

	async resizeShell(_shellId: string, cols: number, rows: number) {
		this.resizes.push([cols, rows]);
	}

	async closeShell() {
		this.closeCount++;
		this.resolveExit?.(137);
	}

	waitShell() {
		return this.exitPromise;
	}

	finish(exitCode: number) {
		this.emit("shellExit", { shellId: this.shellId, exitCode });
		this.resolveExit?.(exitCode);
	}
}

describe("Node shell attachment", () => {
	test("attaches PTY events to Node streams and restores terminal state", async () => {
		const connection = new TestConnection();
		const stdin = new TestInput();
		const stdout = new TestOutput();
		const stderr = new TestOutput();
		const attached = attachShell(connection, {
			stdin,
			stdout,
			stderr,
			signals: [],
		});

		await new Promise((resolve) => setTimeout(resolve, 0));
		expect(stdout.text()).toContain("early output");
		expect(connection.resizes).toContainEqual([120, 40]);

		stdin.emit("data", Buffer.from("echo hello\n"));
		await new Promise((resolve) => setTimeout(resolve, 0));
		connection.emit("shellData", {
			shellId: connection.shellId,
			data: ["$Uint8Array", Buffer.from("hello\n").toString("base64")],
		});
		connection.emit("shellStderr", {
			shellId: connection.shellId,
			data: "warning\n",
		});
		stdout.columns = 90;
		stdout.rows = 30;
		stdout.emit("resize");
		await new Promise((resolve) => setTimeout(resolve, 0));
		connection.finish(7);

		await expect(attached).resolves.toBe(7);
		expect(connection.writes).toEqual([Buffer.from("echo hello\n")]);
		expect(connection.resizes).toContainEqual([90, 30]);
		expect(stdout.text()).toContain("hello");
		expect(stderr.text()).toContain("warning");
		expect(stdin.rawModes).toEqual([true, false]);
		expect(stdin.paused).toBe(true);
		expect(connection.closeCount).toBe(0);
	});

	test("fails with a typed error when early output exceeds its bound", async () => {
		const connection = new TestConnection();
		connection.openShell = async () => {
			connection.emit("shellData", {
				shellId: connection.shellId,
				data: Buffer.from("too much output"),
			});
			return { shellId: connection.shellId };
		};

		await expect(
			attachShell(connection, {
				stdin: new TestInput(),
				stdout: new TestOutput(),
				stderr: new TestOutput(),
				signals: [],
				maxPendingOutputBytes: 4,
			}),
		).rejects.toMatchObject({
			name: NodeShellOutputLimitError.name,
			code: "AGENTOS_NODE_SHELL_OUTPUT_LIMIT",
			limitBytes: 4,
		});
		expect(connection.closeCount).toBe(1);
	});
});
