import { describe, expect, it, vi } from "vitest";
import type { CommandExecutor } from "../../src/runtime.js";
import { wrapCommandExecutor } from "../../src/runtime.js";

function fakeExecutor(): CommandExecutor {
	return {
		spawn: vi.fn(() => ({
			wait: async () => 0,
			writeStdin: vi.fn(),
			closeStdin: vi.fn(),
			kill: vi.fn(),
		})),
	};
}

describe("browser command executor permissions", () => {
	it("passes command, argv0, args, cwd, and env to the child_process permission check", () => {
		const inner = fakeExecutor();
		const seen: unknown[] = [];
		const wrapped = wrapCommandExecutor(inner, {
			childProcess(request) {
				seen.push(request);
				return true;
			},
		});

		wrapped.spawn("tool", ["--flag"], {
			argv0: "-tool",
			cwd: "/workspace",
			env: { PATH: "/bin" },
		});

		expect(seen).toEqual([
			{
				command: "tool",
				args: ["--flag"],
				argv0: "-tool",
				cwd: "/workspace",
				env: { PATH: "/bin" },
			},
		]);
		expect(inner.spawn).toHaveBeenCalledOnce();
	});

	it("blocks denied child_process spawns before invoking the executor", () => {
		const inner = fakeExecutor();
		const wrapped = wrapCommandExecutor(inner, {
			childProcess() {
				return { allow: false };
			},
		});

		expect(() => wrapped.spawn("tool", [])).toThrow(/EACCES/);
		expect(inner.spawn).not.toHaveBeenCalled();
	});
});
