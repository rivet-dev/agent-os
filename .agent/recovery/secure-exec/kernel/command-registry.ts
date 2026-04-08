/**
 * Command registry.
 *
 * Maps command names to runtime drivers. When a process calls
 * spawn("grep", ...), the registry resolves "grep" to the WasmVM driver.
 * Also populates /bin in the VFS so shell PATH lookup succeeds.
 */

import type { RuntimeDriver } from "./types.js";
import type { VirtualFileSystem } from "./vfs.js";

export class CommandRegistry {
	/** command name → RuntimeDriver */
	private commands: Map<string, RuntimeDriver> = new Map();

	/** Warning log for command overrides. */
	private warnings: string[] = [];

	/**
	 * Register all commands from a driver.
	 * Last-mounted driver wins on conflicts (allows override with warning).
	 */
	register(driver: RuntimeDriver): void {
		for (const cmd of driver.commands) {
			const existing = this.commands.get(cmd);
			if (existing) {
				const msg = `command "${cmd}" overridden: ${existing.name} → ${driver.name}`;
				this.warnings.push(msg);
				console.warn(`[CommandRegistry] ${msg}`);
			}
			this.commands.set(cmd, driver);
		}
	}

	/** Get recorded warnings (for testing). */
	getWarnings(): readonly string[] {
		return this.warnings;
	}

	/**
	 * Register a single command to a driver.
	 * Used for on-demand dynamic registration (e.g. after tryResolve).
	 */
	registerCommand(command: string, driver: RuntimeDriver): void {
		const existing = this.commands.get(command);
		if (existing) {
			const msg = `command "${command}" overridden: ${existing.name} → ${driver.name}`;
			this.warnings.push(msg);
			console.warn(`[CommandRegistry] ${msg}`);
		}
		this.commands.set(command, driver);
	}

	/**
	 * Resolve a command name to a driver. Returns null if unknown.
	 * Supports path-based lookup: '/bin/ls' resolves to the driver for 'ls'.
	 */
	resolve(command: string): RuntimeDriver | null {
		// Direct name lookup
		const direct = this.commands.get(command);
		if (direct) return direct;

		// Path-based: extract basename and retry
		if (command.includes("/")) {
			const basename = command.split("/").pop()!;
			if (basename) return this.commands.get(basename) ?? null;
		}

		return null;
	}

	/** List all registered commands. Returns command → driver name. */
	list(): Map<string, string> {
		const result = new Map<string, string>();
		for (const [cmd, driver] of this.commands) {
			result.set(cmd, driver.name);
		}
		return result;
	}

	/**
	 * Create a single /bin stub entry for a command.
	 * Used for on-demand registration after tryResolve discovers a new command.
	 */
	async populateBinEntry(vfs: VirtualFileSystem, command: string): Promise<void> {
		if (!(await vfs.exists("/bin"))) {
			await vfs.mkdir("/bin", { recursive: true });
		}
		const path = `/bin/${command}`;
		if (!(await vfs.exists(path))) {
			const stub = new TextEncoder().encode("#!/bin/sh\n# kernel command stub\n");
			await vfs.writeFile(path, stub);
			try {
				await vfs.chmod(path, 0o755);
			} catch {
				// chmod may not be supported by all VFS backends
			}
		}
	}

	/**
	 * Populate /bin in the VFS with stub entries for all registered commands.
	 * This enables brush-shell's PATH lookup to find commands.
	 */
	async populateBin(vfs: VirtualFileSystem): Promise<void> {
		// Ensure /bin exists
		if (!(await vfs.exists("/bin"))) {
			await vfs.mkdir("/bin", { recursive: true });
		}

		// Create a stub file for each command
		const stub = new TextEncoder().encode("#!/bin/sh\n# kernel command stub\n");
		for (const cmd of this.commands.keys()) {
			const path = `/bin/${cmd}`;
			if (!(await vfs.exists(path))) {
				await vfs.writeFile(path, stub);
				try {
					await vfs.chmod(path, 0o755);
				} catch {
					// chmod may not be supported by all VFS backends
				}
			}
		}
	}
}
