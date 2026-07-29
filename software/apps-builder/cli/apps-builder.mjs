#!/usr/bin/env node

import { createHash } from "node:crypto";
import {
	copyFile,
	cp,
	mkdir,
	readFile,
	readdir,
	rename,
	rm,
	stat,
	writeFile,
} from "node:fs/promises";
import { builtinModules, createRequire } from "node:module";
import { dirname, extname, join, relative, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import * as esbuild from "esbuild-wasm/esm/browser.js";

const MANIFEST_VERSION = 1;
const MAX_DIAGNOSTIC_BYTES = 2 * 1024 * 1024;
const OPTIONAL_RUNTIME_MODULES = new Set([
	"bufferutil",
	"cbor-extract",
	"utf-8-validate",
	"ws",
]);

const configPath = process.argv[2];
if (!configPath) {
	throw new Error("usage: apps-builder <config.json>");
}

const config = JSON.parse(await readFile(configPath, "utf8"));
const workspace = resolve(config.workspace);
const release = resolve(config.release);
const entrypoint = resolve(workspace, config.entrypoint);
const maxOutputBytes = positiveInteger(config.maxOutputBytes, "maxOutputBytes");
const maxOutputFiles = positiveInteger(config.maxOutputFiles, "maxOutputFiles");
const maxFileBytes = positiveInteger(config.maxFileBytes, "maxFileBytes");

await rm(release, { recursive: true, force: true });
await mkdir(join(release, "modules"), { recursive: true });

const define = {
	"process.env.NODE_ENV": JSON.stringify("production"),
};
const require = createRequire(pathToFileURL(entrypoint));
const builderRequire = createRequire(import.meta.url);
if (config.usesRivetKit) {
	const wasmSource = require.resolve(
		"@rivetkit/rivetkit-wasm/rivetkit_wasm_bg.wasm",
	);
	const wasmBytes = await readFile(wasmSource);
	const hash = sha256(wasmBytes);
	const wasmName = `rivetkit-${hash.slice(0, 16)}.wasm`;
	await copyFile(wasmSource, join(release, "modules", wasmName));
	define.__AGENTOS_RIVETKIT_WASM_PATH__ = JSON.stringify(
		`./modules/${wasmName}`,
	);
}

// The Node entrypoint for esbuild-wasm starts a child process. AgentOS supports
// child processes, but the bundler does not need that extra transport layer.
// Run the browser service in-process and resolve files through this bounded
// platform-owned plugin instead.
globalThis.self ??= globalThis;
const esbuildWasm = await readFile(
	builderRequire.resolve("esbuild-wasm/esbuild.wasm"),
);
await esbuild.initialize({
	wasmModule: new WebAssembly.Module(esbuildWasm),
	worker: false,
});
const build = await esbuild.build({
	entryPoints: [entrypoint],
	outfile: join(release, "main.mjs"),
	bundle: true,
	format: "esm",
	platform: "node",
	target: "node22",
	banner: {
		js: 'import { createRequire as __agentOSCreateRequire } from "node:module"; const require = __agentOSCreateRequire(import.meta.url);',
	},
	treeShaking: true,
	minify: true,
	sourcemap: "external",
	// esbuild-wasm's in-process browser service attempts to JSON-decode an empty
	// metafile before reporting build errors. The release manifest below carries
	// the production provenance and hashes we need.
	metafile: false,
	write: false,
	logLevel: "silent",
	define,
	assetNames: "modules/[name]-[hash]",
	loader: {
		".wasm": "file",
		".bin": "file",
		".sql": "text",
		".txt": "text",
	},
	plugins: [nodeFileSystemPlugin()],
});
await esbuild.stop();

for (const output of build.outputFiles ?? []) {
	const outputPath = resolve(output.path);
	if (
		outputPath !== join(release, "main.mjs") &&
		!outputPath.startsWith(`${release}/`)
	) {
		throw new Error(`App Bundle output escapes release root: ${output.path}`);
	}
	await mkdir(dirname(outputPath), { recursive: true });
	await writeFile(outputPath, output.contents);
}

const unsupported = build.warnings.filter((warning) =>
	/dynamic import|not be bundled|unsupported/i.test(warning.text),
);
if (unsupported.length > 0) {
	throw new Error(
		`unsupported non-analyzable import: ${formatMessages(unsupported)}`,
	);
}

const sourceMap = join(release, "main.mjs.map");
await rename(sourceMap, `${configPath}.map`).catch(async (error) => {
	if (error?.code !== "ENOENT") throw error;
});

const staticRoot =
	config.staticRoot ??
	((await stat(join(workspace, "public")).catch(() => null))?.isDirectory()
		? "public"
		: undefined);
if (staticRoot === ".") {
	for (const sourcePath of config.sourceFiles ?? []) {
		const source = resolve(workspace, sourcePath);
		const target = resolve(join(release, "public"), sourcePath);
		if (
			!source.startsWith(`${workspace}/`) ||
			!target.startsWith(`${join(release, "public")}/`)
		) {
			throw new Error(`static source path escapes its root: ${sourcePath}`);
		}
		const info = await stat(source);
		await mkdir(dirname(target), { recursive: true });
		if (info.isDirectory()) {
			await cp(source, target, { recursive: true, force: false });
		} else if (info.isFile()) {
			await copyFile(source, target);
		}
	}
} else if (staticRoot) {
	const staticSource = resolve(workspace, staticRoot);
	const staticInfo = await stat(staticSource);
	if (!staticInfo.isDirectory()) {
		throw new Error(`static root is not a directory: ${staticRoot}`);
	}
	await cp(staticSource, join(release, "public"), {
		recursive: true,
		force: false,
	});
}

const entries = await walkRelease(release);
if (entries.length > maxOutputFiles) {
	throw new Error(
		`App Bundle emitted ${entries.length} files, limit is maxOutputFiles ${maxOutputFiles}`,
	);
}
let totalBytes = 0;
for (const entry of entries) {
	if (entry.size > maxFileBytes) {
		throw new Error(
			`App Bundle file ${entry.path} is ${entry.size} bytes, limit is maxFileBytes ${maxFileBytes}`,
		);
	}
	totalBytes += entry.size;
}
if (totalBytes > maxOutputBytes) {
	throw new Error(
		`App Bundle is ${totalBytes} bytes, limit is maxOutputBytes ${maxOutputBytes}`,
	);
}

const manifest = {
	version: MANIFEST_VERSION,
	mainModule: "main.mjs",
	modules: entries
		.filter((entry) => !entry.path.startsWith("public/"))
		.map((entry) => ({
			path: entry.path,
			type: entry.path.endsWith(".wasm")
				? "wasm"
				: entry.path.endsWith(".mjs") || entry.path.endsWith(".js")
					? "esm"
					: "data",
			size: entry.size,
			hash: entry.hash,
		})),
	assets: entries
		.filter((entry) => entry.path.startsWith("public/"))
		.map((entry) => ({
			path: entry.path,
			size: entry.size,
			hash: entry.hash,
		})),
};
await writeFile(
	join(release, "manifest.json"),
	`${JSON.stringify(manifest, null, 2)}\n`,
);
await writeFile(
	join(release, "agentos-package.json"),
	`${JSON.stringify({
		name: "agentos-app",
		version: String(config.version),
	})}\n`,
);

function positiveInteger(value, name) {
	if (!Number.isSafeInteger(value) || value < 1) {
		throw new TypeError(`${name} must be a positive safe integer`);
	}
	return value;
}

function sha256(bytes) {
	return createHash("sha256").update(bytes).digest("hex");
}

async function walkRelease(root) {
	const files = [];
	const walk = async (directory) => {
		for (const entry of await readdir(directory, { withFileTypes: true })) {
			const path = join(directory, entry.name);
			if (entry.isDirectory()) {
				await walk(path);
				continue;
			}
			if (!entry.isFile()) {
				throw new Error(`App Bundle contains unsupported entry: ${path}`);
			}
			const bytes = await readFile(path);
			files.push({
				path: relative(root, path).split("\\").join("/"),
				size: bytes.byteLength,
				hash: sha256(bytes),
			});
		}
	};
	await walk(root);
	return files.sort((a, b) => a.path.localeCompare(b.path));
}

function formatMessages(messages) {
	return messages
		.map((message) => message.text)
		.join("\n")
		.slice(0, MAX_DIAGNOSTIC_BYTES);
}

function nodeFileSystemPlugin() {
	const builtins = new Set([
		...builtinModules,
		...builtinModules.map((name) => `node:${name}`),
	]);
	return {
		name: "agentos-node-filesystem",
		setup(build) {
			build.onResolve({ filter: /.*/ }, async (args) => {
				if (builtins.has(args.path)) {
					return { path: args.path, external: true };
				}
				const importer =
					args.kind === "entry-point" || !args.importer
						? entrypoint
						: args.importer;
				try {
					if (
						args.kind === "import-statement" ||
						args.kind === "dynamic-import"
					) {
						return {
							path: await resolveEsmImport(args.path, importer),
						};
					}
					const resolver = createRequire(pathToFileURL(importer));
					return { path: resolver.resolve(args.path) };
				} catch (error) {
					if (OPTIONAL_RUNTIME_MODULES.has(args.path)) {
						return { path: args.path, external: true };
					}
					return {
						errors: [
							{
								text: `could not resolve ${JSON.stringify(args.path)} from ${importer}: ${error instanceof Error ? error.message : String(error)}`,
							},
						],
					};
				}
			});
			build.onLoad({ filter: /.*/ }, async (args) => {
				const extension = extname(args.path).toLowerCase();
				if (extension === ".node") {
					return {
						errors: [
							{
								text: `native Node addon is unsupported in AgentOS Apps: ${args.path}`,
							},
						],
					};
				}
				let contents = await readFile(args.path);
				if (
					config.usesRivetKit &&
					(extension === ".js" ||
						extension === ".mjs" ||
						extension === ".ts")
				) {
					// RivetKit deliberately hides this optional dependency from
					// ordinary bundlers. Apps always select the WASM runtime, so
					// make that one runtime edge statically analyzable while
					// leaving the native and engine-CLI fallbacks unreachable.
					const source = contents.toString("utf8");
					contents = Buffer.from(
						source.replaceAll(
							'import(["@rivetkit", "rivetkit-wasm"].join("/"))',
							'import("@rivetkit/rivetkit-wasm")',
						),
					);
				}
				return {
					contents,
					loader:
						extension === ".json"
							? "json"
							: extension === ".ts" || extension === ".cts"
								? "ts"
								: extension === ".tsx"
									? "tsx"
									: extension === ".jsx"
										? "jsx"
										: extension === ".css"
											? "css"
											: extension === ".sql" || extension === ".txt"
												? "text"
											: extension === ".wasm" || extension === ".bin"
												? "file"
												: "js",
				};
			});
		},
	};
}

async function resolveEsmImport(specifier, importer) {
	if (specifier.startsWith("file:")) return fileURLToPath(specifier);
	if (
		specifier.startsWith("./") ||
		specifier.startsWith("../") ||
		specifier.startsWith("/")
	) {
		return resolveModuleFile(
			specifier.startsWith("/")
				? specifier
				: resolve(dirname(importer), specifier),
		);
	}
	if (specifier.startsWith("#")) {
		return resolvePackageImport(specifier, importer);
	}

	const parts = specifier.split("/");
	const packageName = specifier.startsWith("@")
		? parts.slice(0, 2).join("/")
		: parts[0];
	const packageSubpath = parts.slice(packageName.startsWith("@") ? 2 : 1);
	let directory = dirname(importer);
	for (;;) {
		const packageRoot = join(directory, "node_modules", packageName);
		const packageJsonPath = join(packageRoot, "package.json");
		const packageJsonText = await readFile(packageJsonPath, "utf8").catch(
			(error) => {
				if (error?.code === "ENOENT") return undefined;
				throw error;
			},
		);
		if (packageJsonText !== undefined) {
			const packageJson = JSON.parse(packageJsonText);
			const subpath = packageSubpath.length
				? `./${packageSubpath.join("/")}`
				: ".";
			const exported = selectPackageExport(packageJson.exports, subpath);
			if (exported) return resolveModuleFile(resolve(packageRoot, exported));
			const fallback = packageSubpath.length
				? join(packageRoot, ...packageSubpath)
				: resolve(
						packageRoot,
						typeof packageJson.module === "string"
							? packageJson.module
							: typeof packageJson.main === "string"
								? packageJson.main
								: "index.js",
					);
			return resolveModuleFile(fallback);
		}
		const parent = dirname(directory);
		if (parent === directory) break;
		directory = parent;
	}
	throw new Error(
		`Cannot find package ${JSON.stringify(packageName)} imported from ${importer}`,
	);
}

async function resolvePackageImport(specifier, importer) {
	let directory = dirname(importer);
	for (;;) {
		const packageJsonPath = join(directory, "package.json");
		const packageJsonText = await readFile(packageJsonPath, "utf8").catch(
			(error) => {
				if (error?.code === "ENOENT") return undefined;
				throw error;
			},
		);
		if (packageJsonText !== undefined) {
			const packageJson = JSON.parse(packageJsonText);
			const imports = packageJson.imports;
			if (imports && typeof imports === "object" && !Array.isArray(imports)) {
				const exact = imports[specifier];
				if (exact !== undefined) {
					const selected = selectConditionalExport(exact);
					if (selected) return resolveModuleFile(resolve(directory, selected));
				}
				for (const [key, value] of Object.entries(imports)) {
					const star = key.indexOf("*");
					if (
						star < 0 ||
						!specifier.startsWith(key.slice(0, star)) ||
						!specifier.endsWith(key.slice(star + 1))
					) {
						continue;
					}
					const matched = specifier.slice(
						star,
						specifier.length - (key.length - star - 1),
					);
					const selected = selectConditionalExport(value);
					if (selected) {
						return resolveModuleFile(
							resolve(directory, selected.replaceAll("*", matched)),
						);
					}
				}
			}
		}
		const parent = dirname(directory);
		if (parent === directory) break;
		directory = parent;
	}
	throw new Error(
		`Cannot resolve package import ${JSON.stringify(specifier)} from ${importer}`,
	);
}

function selectPackageExport(exportsValue, subpath) {
	if (typeof exportsValue === "string") {
		return subpath === "." ? exportsValue : undefined;
	}
	if (Array.isArray(exportsValue)) {
		for (const value of exportsValue) {
			const selected = selectPackageExport(value, subpath);
			if (selected) return selected;
		}
		return undefined;
	}
	if (
		!exportsValue ||
		typeof exportsValue !== "object"
	) {
		return undefined;
	}
	const entries = Object.entries(exportsValue);
	if (entries.some(([key]) => key.startsWith("."))) {
		const exact = exportsValue[subpath];
		if (exact !== undefined) return selectConditionalExport(exact);
		for (const [key, value] of entries) {
			const star = key.indexOf("*");
			if (
				star < 0 ||
				!subpath.startsWith(key.slice(0, star)) ||
				!subpath.endsWith(key.slice(star + 1))
			) {
				continue;
			}
			const matched = subpath.slice(star, subpath.length - (key.length - star - 1));
			const selected = selectConditionalExport(value);
			return selected?.replaceAll("*", matched);
		}
		return undefined;
	}
	return subpath === "." ? selectConditionalExport(exportsValue) : undefined;
}

function selectConditionalExport(value) {
	if (typeof value === "string") return value;
	if (Array.isArray(value)) {
		for (const entry of value) {
			const selected = selectConditionalExport(entry);
			if (selected) return selected;
		}
		return undefined;
	}
	if (!value || typeof value !== "object") return undefined;
	for (const [condition, target] of Object.entries(value)) {
		if (
			condition === "import" ||
			condition === "node" ||
			condition === "production" ||
			condition === "default"
		) {
			const selected = selectConditionalExport(target);
			if (selected) return selected;
		}
	}
	return undefined;
}

async function resolveModuleFile(candidate) {
	const candidates = extname(candidate)
		? [candidate]
		: [
				candidate,
				`${candidate}.mjs`,
				`${candidate}.js`,
				`${candidate}.ts`,
				`${candidate}.tsx`,
				join(candidate, "index.mjs"),
				join(candidate, "index.js"),
				join(candidate, "index.ts"),
			];
	for (const path of candidates) {
		const info = await stat(path).catch((error) => {
			if (error?.code === "ENOENT") return undefined;
			throw error;
		});
		if (info?.isFile()) return path;
	}
	throw new Error(`Cannot resolve module file ${candidate}`);
}
