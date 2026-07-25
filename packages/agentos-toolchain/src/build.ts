import { execFileSync } from "node:child_process";
import {
	chmodSync,
	cpSync,
	existsSync,
	mkdirSync,
	readdirSync,
	readFileSync,
	rmSync,
	statSync,
	writeFileSync,
} from "node:fs";
import { join, resolve } from "node:path";
import { packAospkgFromTar, verifyAospkgCommands } from "./aospkg.js";
import {
	declaredCommandNames,
	readManifest,
	unscopedName,
} from "./manifest.js";

export interface BuildResult {
	name: string;
	version: string;
	commands: string[];
	outDir: string;
	outTar: string;
	/** The packed `.aospkg` (header + vbare manifest + mount index + mount tar). */
	outAospkg: string;
}

/**
 * Assemble a clean `dist/package/` for a WASM / command registry package.
 *
 * Why this exists: a package's `src/index.ts` sets the descriptor `dir` that the
 * sidecar projects read-only under `/opt/agentos/<name>/<version>`. The sidecar
 * copies that WHOLE directory verbatim. If `dir` pointed at the package source
 * root it would drag `src/`, dev `node_modules/`, `tsconfig.json`, `.turbo/`
 * into every VM. The descriptor must instead point at a CLEAN runtime dir that
 * holds ONLY what the package ships at runtime:
 *
 *   dist/package/
 *     agentos-package.json   pack-time manifest input (name, version, agent,
 *                            provides) — consumed by the `.aospkg` packer and
 *                            STRIPPED from the packed mount tar; the vbare
 *                            chunk1 manifest is the single runtime manifest
 *     bin/<cmd>              the compiled command binaries (copied verbatim)
 *     share/...              (optional) man pages, if the source package ships them
 *
 * The runtime artifact is `dist/package.aospkg` (see aospkg.ts); the plain
 * `dist/package.tar` is kept as the pack intermediate.
 *
 * Commands are derived from the source `bin/` directory's files (populate it
 * with `stage`); `name`/`version` come from the source `package.json`. A package
 * with NO (or an empty) `bin/` ships a valid, empty placeholder — it picks up
 * its commands automatically once `stage` can populate `bin/`.
 *
 * This handles the COMMON case (compiled command packages). JS agent packages
 * whose `dist/package` is a self-contained node runtime closure use `pack`
 * instead; meta packages that default-export an ARRAY of descriptors have no
 * `dir`/`bin` and never run this.
 *
 * Idempotent: `dist/package/` is removed and rebuilt on every run.
 */
export function build(packageDirInput?: string): BuildResult {
	const packageDir = resolve(packageDirInput ?? process.cwd());

	const srcPkgPath = join(packageDir, "package.json");
	if (!existsSync(srcPkgPath)) {
		throw new Error(`no package.json in ${packageDir}`);
	}
	let srcPkg: { name?: unknown; version?: unknown };
	try {
		srcPkg = JSON.parse(readFileSync(srcPkgPath, "utf8"));
	} catch (error) {
		throw new Error(
			`package.json in ${packageDir} is not valid JSON: ${String(error)}`,
		);
	}
	const name = srcPkg.name;
	const version = srcPkg.version;
	if (typeof name !== "string" || name.length === 0) {
		throw new Error(`package.json in ${packageDir} is missing a valid "name"`);
	}
	if (typeof version !== "string" || version.length === 0) {
		throw new Error(
			`package.json in ${packageDir} is missing a valid "version"`,
		);
	}

	const srcManifest = readManifest(packageDir);
	const manifest = {
		name:
			typeof srcManifest?.name === "string" && srcManifest.name.length > 0
				? srcManifest.name
				: unscopedName(name),
		version,
		...(srcManifest?.agent !== undefined ? { agent: srcManifest.agent } : {}),
		...(srcManifest?.provides !== undefined
			? { provides: srcManifest.provides }
			: {}),
	};

	const srcBinDir = join(packageDir, "bin");
	const hasBinDir = existsSync(srcBinDir) && statSync(srcBinDir).isDirectory();
	const commands = hasBinDir
		? readdirSync(srcBinDir, { withFileTypes: true })
				.filter((entry) => entry.isFile() || entry.isSymbolicLink())
				.map((entry) => entry.name)
				.sort()
		: [];
	const declaredCommands = declaredCommandNames(srcManifest);
	if (
		commands.length > 0 &&
		(commands.length !== declaredCommands.length ||
			commands.some((command, index) => command !== declaredCommands[index]))
	) {
		throw new Error(
			`refusing partial command package ${name}: declared=${declaredCommands.join(",")} ` +
				`staged=${commands.join(",")}`,
		);
	}

	const outDir = join(packageDir, "dist", "package");
	rmSync(outDir, { recursive: true, force: true });
	mkdirSync(outDir, { recursive: true });

	// Copy bin/ verbatim (preserve symlinks so any in-package relative aliases
	// stay in-tree, matching the sidecar's verbatim projection copy). Skipped
	// for a planned/empty package — there is nothing to copy yet.
	if (commands.length > 0) {
		cpSync(srcBinDir, join(outDir, "bin"), {
			recursive: true,
			verbatimSymlinks: true,
		});
		for (const command of commands) {
			chmodSync(join(outDir, "bin", command), 0o755);
		}
	}

	// share/ (man pages, etc.) — the projection builds a man symlink farm from
	// <pkg>/share/man.
	const srcShareDir = join(packageDir, "share");
	if (existsSync(srcShareDir) && statSync(srcShareDir).isDirectory()) {
		cpSync(srcShareDir, join(outDir, "share"), {
			recursive: true,
			verbatimSymlinks: true,
		});
	}

	writeFileSync(
		join(outDir, "agentos-package.json"),
		`${JSON.stringify(manifest, null, 2)}\n`,
	);

	const outTar = join(packageDir, "dist", "package.tar");
	rmSync(outTar, { force: true });
	execFileSync("tar", ["-cf", outTar, "-C", outDir, "."], { stdio: "pipe" });

	// Pack the runtime artifact. agentos-package.json is consumed as pack-time
	// input and stripped from the packed mount tar — the vbare chunk1 manifest
	// is the single runtime manifest the sidecar reads.
	const outAospkg = join(packageDir, "dist", "package.aospkg");
	rmSync(outAospkg, { force: true });
	packAospkgFromTar(outTar, outAospkg);
	if (commands.length > 0) {
		verifyAospkgCommands(readFileSync(outAospkg), declaredCommands);
	}

	process.stdout.write(
		commands.length > 0
			? `assembled ${name}@${version} -> ${outAospkg}\n` +
					`  commands (${commands.length}): ${commands.join(", ")}\n`
			: `assembled ${name}@${version} -> ${outAospkg} (PLANNED: empty placeholder, ` +
					`no bin/ yet — run stage with a commands dir to populate)\n`,
	);
	return { name, version, commands, outDir, outTar, outAospkg };
}
