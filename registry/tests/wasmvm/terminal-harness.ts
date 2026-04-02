/**
 * TerminalHarness — wires openShell() to a headless xterm Terminal for
 * deterministic screen-state assertions in tests.
 *
 * Duplicated from packages/kernel/test/terminal-harness.ts because cross-package
 * test imports aren't supported.
 */

import { Terminal } from "@xterm/headless";
import type { Kernel } from "../helpers.js";

type ShellHandle = ReturnType<Kernel["openShell"]>;

/** Settlement window: resolve type() after this many ms of no new output. */
const SETTLE_MS = 50;
/** Poll interval for waitFor(). */
const POLL_MS = 20;
/** Default waitFor() timeout. */
const DEFAULT_WAIT_TIMEOUT_MS = 5_000;

export class TerminalHarness {
	readonly term: Terminal;
	readonly shell: ShellHandle;
	private typing = false;
	private disposed = false;

	constructor(kernel: Kernel, options?: { cols?: number; rows?: number; env?: Record<string, string>; cwd?: string }) {
		const cols = options?.cols ?? 80;
		const rows = options?.rows ?? 24;

		this.term = new Terminal({ cols, rows, allowProposedApi: true });

		this.shell = kernel.openShell({ cols, rows, env: options?.env, cwd: options?.cwd });

		// Wire shell output → xterm
		this.shell.onData = (data: Uint8Array) => {
			this.term.write(data);
		};
	}

	/**
	 * Send input through the PTY. Resolves after output settles (no new bytes
	 * received for SETTLE_MS).
	 */
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

			const resetTimer = () => {
				if (timer !== null) clearTimeout(timer);
				timer = setTimeout(() => {
					this.shell.onData = originalOnData;
					resolve();
				}, SETTLE_MS);
			};

			const originalOnData = this.shell.onData;
			this.shell.onData = (data: Uint8Array) => {
				this.term.write(data);
				resetTimer();
			};

			resetTimer();
			this.shell.write(input);
		});
	}

	/**
	 * Full screen as a string: viewport rows only (not scrollback), trailing
	 * whitespace trimmed per line, trailing empty lines dropped, joined with '\n'.
	 */
	screenshotTrimmed(): string {
		const buf = this.term.buffer.active;
		const rows = this.term.rows;
		const lines: string[] = [];

		for (let y = 0; y < rows; y++) {
			const line = buf.getLine(buf.viewportY + y);
			lines.push(line ? line.translateToString(true) : "");
		}

		while (lines.length > 0 && lines[lines.length - 1] === "") {
			lines.pop();
		}

		return lines.join("\n");
	}

	/**
	 * Single trimmed row from the screen buffer (0-indexed from viewport top).
	 */
	line(row: number): string {
		const buf = this.term.buffer.active;
		const line = buf.getLine(buf.viewportY + row);
		return line ? line.translateToString(true) : "";
	}

	/**
	 * Poll screen buffer every POLL_MS until `text` is found. Throws a
	 * descriptive error on timeout.
	 */
	async waitFor(
		text: string,
		occurrence: number = 1,
		timeoutMs: number = DEFAULT_WAIT_TIMEOUT_MS,
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

			await new Promise((r) => setTimeout(r, POLL_MS));
		}
	}

	/**
	 * Send ^D on empty line and await shell exit. Returns exit code.
	 */
	async exit(): Promise<number> {
		this.shell.write("\x04");
		return this.shell.wait();
	}

	/**
	 * Kill shell and dispose terminal. Safe to call multiple times.
	 */
	async dispose(): Promise<void> {
		if (this.disposed) return;
		this.disposed = true;

		try {
			this.shell.kill();
			await Promise.race([
				this.shell.wait(),
				new Promise((r) => setTimeout(r, 500)),
			]);
		} catch {
			// Shell may already be dead
		}

		this.term.dispose();
	}
}
