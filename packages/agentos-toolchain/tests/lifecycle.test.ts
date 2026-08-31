import { execFileSync } from "node:child_process";
import {
	existsSync,
	lstatSync,
	mkdirSync,
	mkdtempSync,
	readFileSync,
	rmSync,
	statSync,
	symlinkSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, test } from "vitest";
import { build } from "../src/build.js";
import { publish, resolveTag } from "../src/publish.js";
import { stage } from "../src/stage.js";

const dirs: string[] = [];
const mkTmp = (p: string) => {
	const d = mkdtempSync(join(tmpdir(), p));
	dirs.push(d);
	return d;
};
afterEach(() => {
	for (const d of dirs.splice(0)) rmSync(d, { recursive: true, force: true });
});

/** A fake compiled-commands dir: real binaries, a symlink alias, and _stubs. */
function makeCommandsDir(): string {
	const dir = mkTmp("agentos-commands-");
	writeFileSync(join(dir, "sh"), "\0asm-sh");
	writeFileSync(join(dir, "cat"), "\0asm-cat");
	writeFileSync(join(dir, "_stubs"), "\0asm-stubs");
	symlinkSync("sh", join(dir, "linked-sh"));
	return dir;
}

function makePackageDir(manifest: object): string {
	const dir = mkTmp("agentos-pkg-");
	writeFileSync(
		join(dir, "package.json"),
		JSON.stringify({ name: "@agentos-software/fake", version: "1.2.3" }),
	);
	writeFileSync(join(dir, "agentos-package.json"), JSON.stringify(manifest));
	return dir;
}

describe("stage", () => {
	test("stages commands, stubs, and aliases as real files", () => {
		const commandsDir = makeCommandsDir();
		const pkg = makePackageDir({
			commands: ["sh", "cat", "linked-sh"],
			aliases: { bash: "sh", more: "cat" },
			stubs: ["id", "df"],
		});
		const result = stage({ packageDir: pkg, commandsDir });
		expect(result.missing).toEqual([]);
		expect(result.staged.sort()).toEqual(
			["bash", "cat", "df", "id", "linked-sh", "more", "sh"].sort(),
		);
		// Symlink sources are dereferenced into real files.
		expect(lstatSync(join(pkg, "bin", "linked-sh")).isSymbolicLink()).toBe(
			false,
		);
		expect(readFileSync(join(pkg, "bin", "linked-sh"), "utf8")).toBe(
			"\0asm-sh",
		);
		expect(readFileSync(join(pkg, "bin", "bash"), "utf8")).toBe("\0asm-sh");
		expect(readFileSync(join(pkg, "bin", "id"), "utf8")).toBe("\0asm-stubs");
		for (const command of result.staged) {
			expect(statSync(join(pkg, "bin", command)).mode & 0o777).toBe(0o755);
		}
	});

	test("re-staging wipes stale entries from bin/", () => {
		const commandsDir = makeCommandsDir();
		const pkg = makePackageDir({ commands: ["sh"] });
		mkdirSync(join(pkg, "bin"));
		writeFileSync(join(pkg, "bin", "stale"), "old");
		stage({ packageDir: pkg, commandsDir });
		expect(existsSync(join(pkg, "bin", "stale"))).toBe(false);
		expect(existsSync(join(pkg, "bin", "sh"))).toBe(true);
	});

	test("missing binary fails by default, warns with if-missing=skip", () => {
		const commandsDir = makeCommandsDir();
		const pkg = makePackageDir({ commands: ["sh", "no-such-cmd"] });
		expect(() => stage({ packageDir: pkg, commandsDir })).toThrow(
			/no-such-cmd/,
		);
		const result = stage({
			packageDir: pkg,
			commandsDir,
			ifMissing: "skip",
		});
		expect(result.staged).toEqual(["sh"]);
		expect(result.missing).toEqual(["no-such-cmd"]);
	});

	test("missing commands dir with if-missing=skip leaves a placeholder", () => {
		const pkg = makePackageDir({ commands: ["sh"] });
		mkdirSync(join(pkg, "bin"));
		writeFileSync(join(pkg, "bin", "stale"), "old");
		const result = stage({
			packageDir: pkg,
			commandsDir: join(pkg, "does-not-exist"),
			ifMissing: "skip",
		});
		expect(result.staged).toEqual([]);
		expect(existsSync(join(pkg, "bin"))).toBe(false);
	});

	test("no declared commands is a no-op", () => {
		const pkg = makePackageDir({ name: "meta-only" });
		const result = stage({
			packageDir: pkg,
			commandsDir: join(pkg, "does-not-exist"),
		});
		expect(result.staged).toEqual([]);
	});
});

describe("build", () => {
	test("assembles dist/package tar with bin/ and runtime manifest", () => {
		const commandsDir = makeCommandsDir();
		const pkg = makePackageDir({
			name: "fake",
			commands: ["sh", "cat"],
			aliases: { bash: "sh" },
		});
		stage({ packageDir: pkg, commandsDir });
		const result = build(pkg);
		expect(result.commands.sort()).toEqual(["bash", "cat", "sh"]);
		expect(result.outTar).toBe(join(pkg, "dist", "package.tar"));
		const tarEntries = execFileSync("tar", ["-tf", result.outTar], {
			encoding: "utf8",
		})
			.trim()
			.split("\n")
			.map((entry) => entry.replace(/^\.\//, ""));
		expect(tarEntries).toContain("agentos-package.json");
		expect(tarEntries).toContain("bin/bash");
		expect(tarEntries).not.toContain("package.json");
		const runtimeManifest = JSON.parse(
			readFileSync(
				join(pkg, "dist", "package", "agentos-package.json"),
				"utf8",
			),
		);
		// Staging fields are build-time only — they must not ship at runtime.
		expect(runtimeManifest).toEqual({ name: "fake", version: "1.2.3" });
		expect(
			readFileSync(join(pkg, "dist", "package", "bin", "bash"), "utf8"),
		).toBe("\0asm-sh");
		for (const command of result.commands) {
			expect(
				statSync(join(pkg, "dist", "package", "bin", command)).mode & 0o777,
			).toBe(0o755);
		}
	});

	test("empty bin/ assembles a valid placeholder", () => {
		const pkg = makePackageDir({ name: "fake" });
		const result = build(pkg);
		expect(result.commands).toEqual([]);
		expect(existsSync(result.outTar)).toBe(true);
		expect(existsSync(join(pkg, "dist", "package", "bin"))).toBe(false);
	});

	test("refuses to assemble a partially staged command package", () => {
		const commandsDir = makeCommandsDir();
		const pkg = makePackageDir({ commands: ["sh", "missing"] });
		stage({ packageDir: pkg, commandsDir, ifMissing: "skip" });
		expect(() => build(pkg)).toThrow(
			/refusing partial command package.*declared=missing,sh staged=sh/,
		);
	});
});

describe("resolveTag", () => {
	test("defaults to dev, never latest", () => {
		expect(resolveTag({})).toBe("dev");
		expect(resolveTag({ tag: "my-branch" })).toBe("my-branch");
	});
	test("latest requires the explicit flag", () => {
		expect(resolveTag({ latest: true })).toBe("latest");
		expect(() => resolveTag({ tag: "latest" })).toThrow(/--latest/);
		expect(() => resolveTag({ latest: true, tag: "dev" })).toThrow(/conflicts/);
	});
});

describe("publish", () => {
	test("refuses a command package without a packed runtime artifact", () => {
		const pkg = makePackageDir({ commands: ["sh"] });
		mkdirSync(join(pkg, "dist"));
		writeFileSync(join(pkg, "dist", "index.js"), "export default {};\n");
		expect(() => publish({ packageDir: pkg, dryRun: true })).toThrow(
			/declares commands but has no built dist\/package\.aospkg/,
		);
	});

	test("refuses a packed artifact missing a newly declared command", () => {
		const commandsDir = makeCommandsDir();
		const pkg = makePackageDir({ commands: ["sh"] });
		stage({ packageDir: pkg, commandsDir });
		build(pkg);
		writeFileSync(join(pkg, "dist", "index.js"), "export default {};\n");
		writeFileSync(
			join(pkg, "agentos-package.json"),
			JSON.stringify({ commands: ["sh", "cat"] }),
		);
		expect(() => publish({ packageDir: pkg, dryRun: true })).toThrow(
			/expected=cat,sh actual=sh/,
		);
	});
});
