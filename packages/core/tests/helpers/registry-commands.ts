/**
 * Registry software packages for tests — STRICT, no silent skips.
 *
 * Every `@agentos-software/*` package exports `{ packagePath }` pointing at its
 * packed `.aospkg` (`dist/package.aospkg`). Importing this helper THROWS with
 * build instructions when a standard package is not built, instead of letting
 * suites silently skip: with the committed file-linked deps, "not built"
 * always means the sibling registry needs building.
 *
 * Built-ness is checked against the `.aospkg` itself (present, non-trivial,
 * correct magic) plus the sibling `dist/package/` transition dir's bin map
 * when it exists (local registry builds produce both), so a stale or empty
 * command set still fails loudly.
 *
 * The only sanctioned exception is the C-sysroot package set (curl, duckdb,
 * sqlite3, zip, unzip): those need the patched wasi C sysroot
 * that most checkouts don't have, so `cSysrootPackageSkipReason` reports a
 * skip reason instead of throwing. Everything else is load-or-throw.
 */

import {
	copyFileSync,
	existsSync,
	mkdirSync,
	openSync,
	readdirSync,
	readSync,
	closeSync,
	statSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import codex from "@agentos-software/codex-cli";
import coreutils from "@agentos-software/coreutils";
import curl from "@agentos-software/curl";
import diffutils from "@agentos-software/diffutils";
import fd from "@agentos-software/fd";
import file from "@agentos-software/file";
import findutils from "@agentos-software/findutils";
import gawk from "@agentos-software/gawk";
import grep from "@agentos-software/grep";
import gzip from "@agentos-software/gzip";
import jq from "@agentos-software/jq";
import ripgrep from "@agentos-software/ripgrep";
import sed from "@agentos-software/sed";
import tar from "@agentos-software/tar";
import tree from "@agentos-software/tree";
import yq from "@agentos-software/yq";

export interface RegistryPackageRef {
	packagePath: string;
}

const BUILD_INSTRUCTIONS =
	"Build the registry:\n" +
	"  just toolchain-build   # native wasm binaries, once per checkout (slow)\n" +
	"  just software-build    # stage bin/ + pack every dist/package.aospkg\n" +
	"See software/README.md.";

/** `.aospkg` container magic (crates/vfs-core/package-format/v1.bare). */
const AOSPKG_MAGIC = Buffer.from([0x89, 0x41, 0x4f, 0x53]);

/** True when the path is a plausible packed `.aospkg` (magic + header size). */
function isPackedAospkg(path: string): boolean {
	try {
		if (statSync(path).size <= 16) return false;
		const fd = openSync(path, "r");
		try {
			const head = Buffer.alloc(4);
			readSync(fd, head, 0, 4, 0);
			return head.equals(AOSPKG_MAGIC);
		} finally {
			closeSync(fd);
		}
	} catch {
		return false;
	}
}

/**
 * The unpacked manifest dir for a ref, when one is locally available: the
 * transition dir itself, or the `dist/package/` sibling of a packed
 * `dist/package.aospkg` (registry builds stage both).
 */
function manifestDir(pkg: RegistryPackageRef): string | null {
	const path = pkg.packagePath;
	const dir = path.endsWith(".aospkg")
		? join(dirname(path), "package")
		: path;
	return existsSync(dir) ? dir : null;
}

/**
 * A built package's staged commands, from its `bin/` directory listing (the
 * transition dir carries only `agentos-package.json` + `bin/`; the command
 * set is the packed `bin/` contents). Maps name -> package-relative path.
 */
function readBinMap(dir: string): Record<string, string> | null {
	const binDir = join(dir, "bin");
	if (!existsSync(binDir)) return null;
	try {
		const bin: Record<string, string> = {};
		for (const entry of readdirSync(binDir)) {
			bin[entry] = `bin/${entry}`;
		}
		return bin;
	} catch {
		return null;
	}
}

function builtState(pkg: RegistryPackageRef): {
	built: boolean;
	bin: Record<string, string> | null;
	missing: string[];
} {
	const packed = pkg.packagePath.endsWith(".aospkg");
	const built = packed
		? isPackedAospkg(pkg.packagePath)
		: existsSync(join(pkg.packagePath, "agentos-package.json"));
	const dir = manifestDir(pkg);
	const bin = dir ? readBinMap(dir) : null;
	const missing =
		dir && bin
			? Object.entries(bin)
					.filter(([, rel]) => !existsSync(join(dir, rel)))
					.map(([cmd]) => cmd)
			: [];
	return { built, bin, missing };
}

/**
 * Assert a software package is built (a real packed `.aospkg`, with a
 * non-empty, fully-present command set when the staged transition dir is
 * available to inspect) and return it. Throws with build instructions
 * otherwise.
 */
export function requireBuilt<T extends RegistryPackageRef>(
	pkg: T,
	name: string,
): T {
	const { built, bin, missing } = builtState(pkg);
	if (!built) {
		throw new Error(
			`software package ${name} is NOT BUILT (no valid ${pkg.packagePath}).\n${BUILD_INSTRUCTIONS}`,
		);
	}
	if (bin !== null && Object.keys(bin).length === 0) {
		throw new Error(
			`software package ${name} is an EMPTY placeholder (no commands staged into bin/).\n${BUILD_INSTRUCTIONS}`,
		);
	}
	if (missing.length > 0) {
		throw new Error(
			`software package ${name} is missing built commands: ${missing.join(", ")}.\n${BUILD_INSTRUCTIONS}`,
		);
	}
	return pkg;
}

/**
 * Skip reason for the C-sysroot package set ONLY (curl, duckdb, sqlite3,
 * zip, unzip). These need the patched wasi C sysroot
 * (`make -C toolchain/c`), which most checkouts don't build — a missing
 * artifact is an environment limitation, not a forgotten build, so suites may
 * skip with this reason instead of throwing.
 */
export function cSysrootPackageSkipReason(
	...packages: Array<{ pkg: RegistryPackageRef; name: string }>
): string | false {
	const unbuilt = packages.filter(({ pkg }) => {
		const { built, bin, missing } = builtState(pkg);
		return (
			!built ||
			(bin !== null && Object.keys(bin).length === 0) ||
			missing.length > 0
		);
	});
	if (unbuilt.length === 0) return false;
	return (
		`C-sysroot software packages not built: ${unbuilt.map(({ name }) => name).join(", ")} ` +
		"(needs the patched wasi C sysroot: `make -C toolchain/c`, then `just software-build`)"
	);
}

/** True when a built package stages the named command (via its bin map). */
export function packageCommandExists(
	pkg: RegistryPackageRef,
	command: string,
): boolean {
	const dir = manifestDir(pkg);
	if (!dir) return false;
	const bin = readBinMap(dir);
	const rel = bin?.[command];
	return typeof rel === "string" && existsSync(join(dir, rel));
}

/**
 * The staged command dir (`<manifest dir>/bin`) of a built package — for
 * harnesses that consume raw command dirs (e.g. `createWasmVmRuntime`).
 * Throws when the staged transition dir is unavailable.
 */
export function packageCommandsDir(pkg: RegistryPackageRef): string {
	const dir = manifestDir(pkg);
	if (!dir) {
		throw new Error(
			`software package has no staged transition dir next to ${pkg.packagePath}.\n${BUILD_INSTRUCTIONS}`,
		);
	}
	return join(dir, "bin");
}

/** First REGISTRY_SOFTWARE package that stages the named command. Throws if none. */
export function findPackageWithCommand(command: string): RegistryPackageRef {
	const pkg = REGISTRY_SOFTWARE.find((candidate) =>
		packageCommandExists(candidate, command),
	);
	if (!pkg) {
		throw new Error(
			`registry software does not provide "${command}".\n${BUILD_INSTRUCTIONS}`,
		);
	}
	return pkg;
}

/** All standard registry software packages — throws at import if any is unbuilt. */
export const REGISTRY_SOFTWARE = (
	[
		[coreutils, "coreutils"],
		[sed, "sed"],
		[grep, "grep"],
		[gawk, "gawk"],
		[findutils, "findutils"],
		[diffutils, "diffutils"],
		[tar, "tar"],
		[gzip, "gzip"],
		[jq, "jq"],
		[ripgrep, "ripgrep"],
		[fd, "fd"],
		[tree, "tree"],
		[file, "file"],
		[yq, "yq"],
		[codex, "codex-cli"],
		[curl, "curl"],
	] as Array<[RegistryPackageRef, string]>
).map(([pkg, name]) => requireBuilt(pkg, name));

/**
 * Test-only commands (e.g. `xu`, a registry VM-test binary) ship in NO
 * software package — they exist only in the native build output of the
 * registry. Synthesize a minimal transition-dir package around them so suites
 * can project them like any other software (`packagePath` accepts a package
 * dir for local fixtures). Throws when the native build output is absent
 * (same build instructions as everything else).
 */
export function testOnlyCommandSoftware(
	commands: string[] = ["xu"],
): RegistryPackageRef {
	// software/<pkg>/dist/package.aospkg -> toolchain/... — this
	// follows whichever registry checkout the deps are linked to.
	const nativeCommandsDir = join(
		dirname(coreutils.packagePath),
		"../../../..",
		"toolchain/target/wasm32-wasip1/release/commands",
	);
	const dir = join(tmpdir(), `agentos-test-cmds-${process.pid}`);
	const binDir = join(dir, "bin");
	mkdirSync(binDir, { recursive: true });
	const bin: Record<string, string> = {};
	for (const command of commands) {
		const src = join(nativeCommandsDir, command);
		if (!existsSync(src)) {
			throw new Error(
				`test-only command "${command}" is missing from the native build output ` +
					`(${nativeCommandsDir}).\n${BUILD_INSTRUCTIONS}`,
			);
		}
		copyFileSync(src, join(binDir, command));
		bin[command] = `bin/${command}`;
	}
	writeFileSync(
		join(dir, "package.json"),
		`${JSON.stringify({ name: "agentos-test-commands", version: "0.0.0", bin }, null, 2)}\n`,
	);
	writeFileSync(
		join(dir, "agentos-package.json"),
		`${JSON.stringify({ name: "agentos-test-commands", version: "1.0.0" }, null, 2)}\n`,
	);
	return { packagePath: dir };
}
