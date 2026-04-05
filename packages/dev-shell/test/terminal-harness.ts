import { Terminal } from "@xterm/headless";
import type { Kernel } from "@rivet-dev/agent-os/test/runtime";

type ShellHandle = ReturnType<Kernel["openShell"]>;

const SETTLE_MS = 50;
const POLL_MS = 20;
const DEFAULT_WAIT_TIMEOUT_MS = 5_000;

export class TerminalHarness {
	readonly term: Terminal;
	readonly shell: ShellHandle;
	private typing = false;
	private disposed = false;

	constructor(
		kernel: Kernel,
		options?: { cols?: number; rows?: number; env?: Record<string, string>; cwd?: string },
	) {
		const cols = options?.cols ?? 80;
		const rows = options?.rows ?? 24;

		this.term = new Terminal({ cols, rows, allowProposedApi: true });
		this.shell = kernel.openShell({ cols, rows, env: options?.env, cwd: options?.cwd });
		this.shell.onData = (data: Uint8Array) => {
			this.term.write(data);
		};
	}

	async type(input: string): Promise<void> {
		if (this.typing) {
			throw new Error("TerminalHarness.type() called while previous type() is still in-flight");
		}
		this.typing = true;
		try {
			await this.typeInternal(input);
		} finally {
			this.typing = false;
		}
	}

	private typeInternal(input: string): Promise<void> {
		return new Promise<void>((resolve) => {
			let timer: ReturnType<typeof setTimeout> | null = null;

			const originalOnData = this.shell.onData;
			const resetTimer = () => {
				if (timer !== null) clearTimeout(timer);
				timer = setTimeout(() => {
					this.shell.onData = originalOnData;
					resolve();
				}, SETTLE_MS);
			};

			this.shell.onData = (data: Uint8Array) => {
				this.term.write(data);
				resetTimer();
			};

			resetTimer();
			this.shell.write(input);
		});
	}

	screenshotTrimmed(): string {
		const buf = this.term.buffer.active;
		const lines: string[] = [];

		for (let row = 0; row < this.term.rows; row++) {
			const line = buf.getLine(buf.viewportY + row);
			lines.push(line ? line.translateToString(true) : "");
		}

		while (lines.length > 0 && lines[lines.length - 1] === "") {
			lines.pop();
		}

		return lines.join("\n");
	}

	async waitFor(
		text: string,
		occurrence = 1,
		timeoutMs = DEFAULT_WAIT_TIMEOUT_MS,
	): Promise<void> {
		const deadline = Date.now() + timeoutMs;

		while (true) {
			const screen = this.screenshotTrimmed();
			let count = 0;
			let idx = -1;

			while (true) {
				idx = screen.indexOf(text, idx + 1);
				if (idx === -1) break;
				count++;
				if (count >= occurrence) return;
			}

			if (Date.now() >= deadline) {
				throw new Error(
					`waitFor("${text}", ${occurrence}) timed out after ${timeoutMs}ms.\n` +
					`Expected: "${text}" (occurrence ${occurrence})\n` +
					`Screen:\n${screen}`,
				);
			}

			await new Promise((resolve) => setTimeout(resolve, POLL_MS));
		}
	}

	async dispose(): Promise<void> {
		if (this.disposed) return;
		this.disposed = true;

		try {
			this.shell.kill();
			await Promise.race([
				this.shell.wait(),
				new Promise((resolve) => setTimeout(resolve, 500)),
			]);
		} catch {
			// Shell may already be gone.
		}

		this.term.dispose();
	}
}
