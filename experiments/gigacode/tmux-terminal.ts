import { execFile } from "node:child_process";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);

export type TerminalKey =
	| "Enter"
	| "Escape"
	| "Up"
	| "Down"
	| "Left"
	| "Right"
	| "Tab"
	| "Backspace"
	| "Ctrl-X"
	| "Ctrl-C";

export type TerminalSnapshot = {
	label: string;
	text: string;
};

function shellQuote(value: string): string {
	return `'${value.replaceAll("'", `'"'"'`)}'`;
}

/** A small Playwright-style driver for full-screen terminal applications. */
export class TmuxTerminal {
	readonly name: string;
	readonly snapshots: TerminalSnapshot[] = [];

	private constructor(name: string) {
		this.name = name;
	}

	static async launch(options: {
		command: string[];
		cwd: string;
		env?: NodeJS.ProcessEnv;
		name?: string;
		width?: number;
		height?: number;
	}): Promise<TmuxTerminal> {
		if (options.command.length === 0)
			throw new Error("command cannot be empty");
		const name =
			options.name ??
			`terminal-e2e-${process.pid}-${Math.random().toString(36).slice(2, 10)}`;
		const environment = Object.entries(options.env ?? {})
			.filter((entry): entry is [string, string] => entry[1] !== undefined)
			.map(([key, value]) => `${key}=${shellQuote(value)}`)
			.join(" ");
		const command = options.command.map(shellQuote).join(" ");
		const shellCommand = `cd ${shellQuote(options.cwd)} && ${environment} exec ${command}`;
		await execFileAsync("tmux", [
			"new-session",
			"-d",
			"-s",
			name,
			"-x",
			String(options.width ?? 120),
			"-y",
			String(options.height ?? 40),
			shellCommand,
		]);
		return new TmuxTerminal(name);
	}

	async type(text: string): Promise<void> {
		await execFileAsync("tmux", ["send-keys", "-t", this.name, "-l", text]);
	}

	async press(key: TerminalKey): Promise<void> {
		const tmuxKey =
			key === "Ctrl-C"
				? "C-c"
				: key === "Ctrl-X"
					? "C-x"
					: key === "Backspace"
						? "BSpace"
						: key;
		await execFileAsync("tmux", ["send-keys", "-t", this.name, tmuxKey]);
	}

	async viewportText(): Promise<string> {
		const { stdout } = await execFileAsync("tmux", [
			"capture-pane",
			"-p",
			"-J",
			"-t",
			this.name,
		]);
		return stdout.replace(/[ \t]+$/gm, "").replace(/\n+$/, "");
	}

	async text(): Promise<string> {
		const { stdout } = await execFileAsync("tmux", [
			"capture-pane",
			"-p",
			"-J",
			"-S",
			"-",
			"-t",
			this.name,
		]);
		return stdout.replace(/[ \t]+$/gm, "").replace(/\n+$/, "");
	}

	async snapshot(label: string): Promise<TerminalSnapshot> {
		const snapshot = { label, text: await this.text() };
		this.snapshots.push(snapshot);
		return snapshot;
	}

	async waitForText(
		expected: string | RegExp,
		options: { timeoutMs?: number; intervalMs?: number } = {},
	): Promise<string> {
		const timeoutMs = options.timeoutMs ?? 30_000;
		const deadline = Date.now() + timeoutMs;
		let latest = "";
		while (Date.now() < deadline) {
			latest = await this.text();
			if (
				typeof expected === "string"
					? latest.includes(expected)
					: expected.test(latest)
			) {
				return latest;
			}
			await new Promise((resolve) =>
				setTimeout(resolve, options.intervalMs ?? 100),
			);
		}
		throw new Error(
			`terminal did not show ${String(expected)} within ${timeoutMs}ms:\n${latest}`,
		);
	}

	async waitForViewportText(
		expected: string | RegExp,
		options: { timeoutMs?: number; intervalMs?: number } = {},
	): Promise<string> {
		const timeoutMs = options.timeoutMs ?? 30_000;
		const deadline = Date.now() + timeoutMs;
		let latest = "";
		while (Date.now() < deadline) {
			latest = await this.viewportText();
			if (
				typeof expected === "string"
					? latest.includes(expected)
					: expected.test(latest)
			) {
				return latest;
			}
			await new Promise((resolve) =>
				setTimeout(resolve, options.intervalMs ?? 100),
			);
		}
		throw new Error(
			`terminal viewport did not show ${String(expected)} within ${timeoutMs}ms:\n${latest}`,
		);
	}

	async waitForViewportTextAbsent(
		unexpected: string | RegExp,
		options: { timeoutMs?: number; intervalMs?: number } = {},
	): Promise<string> {
		const timeoutMs = options.timeoutMs ?? 30_000;
		const deadline = Date.now() + timeoutMs;
		let latest = "";
		while (Date.now() < deadline) {
			latest = await this.viewportText();
			const present =
				typeof unexpected === "string"
					? latest.includes(unexpected)
					: unexpected.test(latest);
			if (!present) return latest;
			await new Promise((resolve) =>
				setTimeout(resolve, options.intervalMs ?? 100),
			);
		}
		throw new Error(
			`terminal viewport still showed ${String(unexpected)} after ${timeoutMs}ms:\n${latest}`,
		);
	}

	async waitForTextAbsent(
		unexpected: string | RegExp,
		options: {
			timeoutMs?: number;
			intervalMs?: number;
			stableMs?: number;
		} = {},
	): Promise<string> {
		const timeoutMs = options.timeoutMs ?? 30_000;
		const deadline = Date.now() + timeoutMs;
		const stableMs = options.stableMs ?? 0;
		let absentSince: number | undefined;
		let latest = "";
		while (Date.now() < deadline) {
			latest = await this.text();
			const present =
				typeof unexpected === "string"
					? latest.includes(unexpected)
					: unexpected.test(latest);
			if (present) {
				absentSince = undefined;
			} else {
				absentSince ??= Date.now();
				if (Date.now() - absentSince >= stableMs) return latest;
			}
			await new Promise((resolve) =>
				setTimeout(resolve, options.intervalMs ?? 100),
			);
		}
		throw new Error(
			`terminal still showed ${String(unexpected)} after ${timeoutMs}ms:\n${latest}`,
		);
	}

	async waitForTextOccurrences(
		expected: string,
		count: number,
		options: { timeoutMs?: number; intervalMs?: number } = {},
	): Promise<string> {
		if (count < 1) throw new Error("count must be at least 1");
		const timeoutMs = options.timeoutMs ?? 30_000;
		const deadline = Date.now() + timeoutMs;
		let latest = "";
		while (Date.now() < deadline) {
			latest = await this.text();
			if (latest.split(expected).length - 1 >= count) return latest;
			await new Promise((resolve) =>
				setTimeout(resolve, options.intervalMs ?? 100),
			);
		}
		throw new Error(
			`terminal did not show ${JSON.stringify(expected)} ${count} times within ${timeoutMs}ms:\n${latest}`,
		);
	}

	async close(): Promise<void> {
		await execFileAsync("tmux", ["kill-session", "-t", this.name]).catch(
			() => undefined,
		);
	}
}
