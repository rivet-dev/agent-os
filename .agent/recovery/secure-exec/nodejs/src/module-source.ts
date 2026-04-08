import { existsSync, readFileSync } from "node:fs";
import { dirname as pathDirname, join as pathJoin } from "node:path";
import { pathToFileURL } from "node:url";
import { transform, transformSync } from "esbuild";
import { initSync as initCjsLexerSync, parse as parseCjsExports } from "cjs-module-lexer";
import { init, initSync, parse } from "es-module-lexer";

const REQUIRE_TRANSFORM_MARKER = "/*__secure_exec_require_esm__*/";
const IMPORT_META_URL_HELPER = "__secureExecImportMetaUrl__";
const IMPORT_META_RESOLVE_HELPER = "__secureExecImportMetaResolve__";
const UNICODE_SET_REGEX_MARKER = "/v";
const CJS_IMPORT_DEFAULT_HELPER = "__secureExecImportedCjsModule__";

function isJavaScriptLikePath(filePath: string | undefined): boolean {
	return filePath === undefined || /\.[cm]?[jt]sx?$/.test(filePath);
}

function normalizeJavaScriptSource(source: string): string {
	const bomPrefix = source.charCodeAt(0) === 0xfeff ? "\uFEFF" : "";
	const shebangOffset = bomPrefix.length;
	if (!source.startsWith("#!", shebangOffset)) {
		return source;
	}
	return (
		bomPrefix +
		"//" +
		source.slice(shebangOffset + 2)
	);
}

function parseSourceSyntax(source: string, filePath?: string) {
	const [imports, , , hasModuleSyntax] = parse(source, filePath);
	const hasDynamicImport = imports.some((specifier) => specifier.d >= 0);
	const hasImportMeta = imports.some((specifier) => specifier.d === -2);
	return { hasModuleSyntax, hasDynamicImport, hasImportMeta };
}

/**
 * Expand `export * from '...'` re-exports into explicit named exports.
 *
 * The V8 isolate's module linker doesn't automatically resolve star
 * re-exports, so we pre-resolve them by reading the target module and
 * extracting its named exports. This runs on the host side before the
 * source is sent to the isolate.
 */
function expandStarReExports(source: string, hostPath: string): string {
	const starExportRegex = /export\s*\*\s*from\s*['"]([^'"]+)['"]\s*;?/g;
	let result = source;
	let match: RegExpExecArray | null;

	// Collect names already directly exported by this module to avoid duplicates
	initSync();
	const [, ownExports] = parse(source, hostPath);
	const ownExportNames = new Set(
		ownExports
			.map((e) => e.n)
			.filter((n): n is string => typeof n === "string"),
	);

	while ((match = starExportRegex.exec(source)) !== null) {
		const specifier = match[1];
		const dir = pathDirname(hostPath);
		const targetPath = specifier.startsWith(".")
			? pathJoin(dir, specifier)
			: null;

		if (!targetPath || !existsSync(targetPath)) continue;

		try {
			const targetSource = readFileSync(targetPath, "utf-8");
			const [, targetExports] = parse(targetSource, targetPath);
			const names = targetExports
				.map((e) => e.n)
				.filter(
					(n): n is string =>
						typeof n === "string" &&
						n !== "default" &&
						!ownExportNames.has(n),
				);

			if (names.length > 0) {
				// Track these names so subsequent export * don't duplicate
				for (const n of names) ownExportNames.add(n);
				result = result.replace(
					match[0],
					`export { ${names.join(", ")} } from '${specifier}';`,
				);
			} else {
				result = result.replace(match[0], "");
			}
		} catch {
			// If we can't resolve, leave the export * as-is
		}
	}

	return result;
}

function isValidIdentifier(value: string): boolean {
	return /^[$A-Z_][0-9A-Z_$]*$/i.test(value);
}

function getNearestPackageTypeSync(filePath: string): "module" | "commonjs" | null {
	let currentDir = pathDirname(filePath);
	while (true) {
		const packageJsonPath = pathJoin(currentDir, "package.json");
		if (existsSync(packageJsonPath)) {
			try {
				const pkgJson = JSON.parse(readFileSync(packageJsonPath, "utf8")) as {
					type?: unknown;
				};
				return pkgJson.type === "module" || pkgJson.type === "commonjs"
					? pkgJson.type
					: null;
			} catch {
				return null;
			}
		}

		const parentDir = pathDirname(currentDir);
		if (parentDir === currentDir) {
			return null;
		}
		currentDir = parentDir;
	}
}

function isCommonJsModuleForImportSync(source: string, formatPath: string): boolean {
	if (!isJavaScriptLikePath(formatPath)) {
		return false;
	}
	if (formatPath.endsWith(".cjs")) {
		return true;
	}
	if (formatPath.endsWith(".mjs")) {
		return false;
	}
	if (formatPath.endsWith(".js")) {
		const packageType = getNearestPackageTypeSync(formatPath);
		if (formatPath.includes("balanced")) {
			console.error(`[isCommonJsModuleForImportSync] path=${formatPath} packageType=${packageType}`);
		}
		if (packageType === "module") {
			return false;
		}
		if (packageType === "commonjs") {
			return true;
		}

		initSync();
		const syntax = parseSourceSyntax(source, formatPath);
		if (formatPath.includes("balanced")) {
			console.error(`[isCommonJsModuleForImportSync] hasModuleSyntax=${syntax.hasModuleSyntax}`);
		}
		return !syntax.hasModuleSyntax;
	}
	return false;
}

function buildCommonJsImportWrapper(source: string, filePath: string): string {
	initCjsLexerSync();
	const { exports: cjsExports } = parseCjsExports(source);
	let namedExports = Array.from(
		new Set(
			cjsExports.filter(
				(name) =>
					name !== "default" &&
					name !== "__esModule" &&
					isValidIdentifier(name),
			),
		),
	);

	// Node.js CJS interop: when module.exports = identifier, the identifier
	// becomes a named export. Detect `module.exports = <name>` and add it.
	if (namedExports.length === 0) {
		const match = source.match(/module\.exports\s*=\s*([a-zA-Z_$][a-zA-Z0-9_$]*)\s*;?\s*$/m);
		if (match && match[1] !== "undefined" && match[1] !== "null") {
			namedExports = [match[1]];
		}
	}

	// Also extract all enumerable property names from the module at load time.
	// This handles cases where module.exports is an object with properties
	// that cjs-module-lexer doesn't detect.
	const lines = [
		`const ${CJS_IMPORT_DEFAULT_HELPER} = globalThis._requireFrom(${JSON.stringify(filePath)}, "/");`,
		`export default ${CJS_IMPORT_DEFAULT_HELPER};`,
		...namedExports.map(
			(name) =>
				`export const ${name} = ${CJS_IMPORT_DEFAULT_HELPER} == null ? undefined : ${CJS_IMPORT_DEFAULT_HELPER}[${JSON.stringify(name)}];`,
		),
	];
	return lines.join("\n");
}

function getRequireTransformOptions(
	filePath: string,
	syntax: ReturnType<typeof parseSourceSyntax>,
) {
	const requiresEsmWrapper =
		syntax.hasModuleSyntax || syntax.hasImportMeta;
	const bannerLines = requiresEsmWrapper ? [REQUIRE_TRANSFORM_MARKER] : [];
	if (syntax.hasImportMeta) {
		bannerLines.push(
			`const ${IMPORT_META_URL_HELPER} = require("node:url").pathToFileURL(__secureExecFilename).href;`,
		);
	}

	return {
		banner: bannerLines.length > 0 ? bannerLines.join("\n") : undefined,
		define: syntax.hasImportMeta
			? {
					"import.meta.url": IMPORT_META_URL_HELPER,
				}
			: undefined,
		format: "cjs" as const,
		loader: "js" as const,
		platform: "node" as const,
		sourcefile: filePath,
		supported: {
			"dynamic-import": false,
		},
		target: "node22",
	};
}

function getImportTransformOptions(
	filePath: string,
	syntax: ReturnType<typeof parseSourceSyntax>,
) {
	const bannerLines: string[] = [];
	if (syntax.hasImportMeta) {
		bannerLines.push(
			`const ${IMPORT_META_URL_HELPER} = ${JSON.stringify(pathToFileURL(filePath).href)};`,
			`const ${IMPORT_META_RESOLVE_HELPER} = (specifier) => globalThis.__importMetaResolve(specifier, ${JSON.stringify(filePath)});`,
		);
	}
	return {
		banner: bannerLines.length > 0 ? bannerLines.join("\n") : undefined,
		define: syntax.hasImportMeta
			? {
					"import.meta.url": IMPORT_META_URL_HELPER,
					"import.meta.resolve": IMPORT_META_RESOLVE_HELPER,
				}
			: undefined,
		format: "esm" as const,
		loader: "js" as const,
		platform: "node" as const,
		sourcefile: filePath,
		target: "es2020",
	};
}

export async function sourceHasModuleSyntax(
	source: string,
	filePath?: string,
): Promise<boolean> {
	const normalizedSource = normalizeJavaScriptSource(source);
	if (filePath?.endsWith(".mjs")) {
		return true;
	}
	if (filePath?.endsWith(".cjs")) {
		return false;
	}

	await init;
	return parseSourceSyntax(normalizedSource, filePath).hasModuleSyntax;
}

export function transformSourceForRequireSync(
	source: string,
	filePath: string,
): string {
	if (!isJavaScriptLikePath(filePath)) {
		return source;
	}

	const normalizedSource = normalizeJavaScriptSource(source);
	initSync();
	const syntax = parseSourceSyntax(normalizedSource, filePath);
	if (!(syntax.hasModuleSyntax || syntax.hasDynamicImport || syntax.hasImportMeta)) {
		return normalizedSource;
	}

	try {
		return transformSync(normalizedSource, getRequireTransformOptions(filePath, syntax)).code;
	} catch {
		return normalizedSource;
	}
}

export async function transformSourceForRequire(
	source: string,
	filePath: string,
): Promise<string> {
	if (!isJavaScriptLikePath(filePath)) {
		return source;
	}

	const normalizedSource = normalizeJavaScriptSource(source);
	await init;
	const syntax = parseSourceSyntax(normalizedSource, filePath);
	if (!(syntax.hasModuleSyntax || syntax.hasDynamicImport || syntax.hasImportMeta)) {
		return normalizedSource;
	}

	try {
		return (
			await transform(normalizedSource, getRequireTransformOptions(filePath, syntax))
		).code;
	} catch {
		return normalizedSource;
	}
}

export async function transformSourceForImport(
	source: string,
	filePath: string,
): Promise<string> {
	if (!isJavaScriptLikePath(filePath)) {
		return source;
	}

	const normalizedSource = normalizeJavaScriptSource(source);
	await init;
	const syntax = parseSourceSyntax(normalizedSource, filePath);
	const needsTransform =
		normalizedSource.includes(UNICODE_SET_REGEX_MARKER) || syntax.hasImportMeta;
	if (!(syntax.hasModuleSyntax || syntax.hasDynamicImport || syntax.hasImportMeta)) {
		return normalizedSource;
	}
	if (!needsTransform) {
		return normalizedSource;
	}

	try {
		return (await transform(normalizedSource, getImportTransformOptions(filePath, syntax))).code;
	} catch {
		return normalizedSource;
	}
}

export function transformSourceForImportSync(
	source: string,
	filePath: string,
	formatPath: string = filePath,
): string {
	if (!isJavaScriptLikePath(filePath)) {
		return source;
	}

	const normalizedSource = normalizeJavaScriptSource(source);
	if (isCommonJsModuleForImportSync(normalizedSource, formatPath)) {
		return buildCommonJsImportWrapper(normalizedSource, filePath);
	}

	// Expand export * re-exports before V8 evaluation
	let processedSource = normalizedSource;
	if (/export\s*\*\s*from\s/.test(processedSource) && formatPath) {
		processedSource = expandStarReExports(processedSource, formatPath);
	}

	initSync();
	const syntax = parseSourceSyntax(processedSource, filePath);
	const needsTransform =
		processedSource.includes(UNICODE_SET_REGEX_MARKER) || syntax.hasImportMeta;
	if (!(syntax.hasModuleSyntax || syntax.hasDynamicImport || syntax.hasImportMeta)) {
		return processedSource;
	}
	if (!needsTransform) {
		return processedSource;
	}

	try {
		return transformSync(processedSource, getImportTransformOptions(filePath, syntax)).code;
	} catch {
		return processedSource;
	}
}
